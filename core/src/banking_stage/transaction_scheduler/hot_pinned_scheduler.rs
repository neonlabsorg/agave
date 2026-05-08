//! Hot-pinned scheduler.
//!
//! Shaped like `PrioGraphScheduler` (look-ahead window, multi-cycle inner
//! loop, per-thread `MAX_BLOCK_UNITS / num_threads` CU cap, send-one-thread
//! on batch full) but with the priority graph replaced by a plain
//! `VecDeque` walked in container (priority + tx id) order. Statically
//! declared "hot" writable accounts are LPT bin-packed onto worker threads
//! at construction; every future tx writing one of those accounts is
//! forced onto its assigned thread. Honors `relax_intrabatch_account_locks`:
//! when set, conflicting txs may share a batch on the thread that already
//! holds the writable lock; when cleared, the working-account-set is
//! consulted and the batch is flushed before scheduling a conflicting tx
//! (same trick the greedy scheduler uses). A tx whose accounts can't
//! currently be locked is set aside and re-queued at end of pass; later
//! txs are free to leapfrog it.

#[cfg(feature = "dev-context-only-utils")]
use qualifier_attr::qualifiers;
use {
    super::{
        scheduler::{PreLockFilterAction, Scheduler, SchedulingSummary},
        scheduler_common::{
            select_thread, SchedulingCommon, TransactionSchedulingError, TransactionSchedulingInfo,
        },
        scheduler_error::SchedulerError,
        transaction_priority_id::TransactionPriorityId,
        transaction_state::TransactionState,
        transaction_state_container::StateContainer,
    },
    crate::banking_stage::{
        consumer::TARGET_NUM_TRANSACTIONS_PER_BATCH,
        read_write_account_set::ReadWriteAccountSet,
        scheduler_messages::{ConsumeWork, FinishedConsumeWork},
    },
    agave_scheduling_utils::thread_aware_account_locks::{
        ThreadAwareAccountLocks, ThreadId, ThreadSet, TryLockError,
    },
    ahash::AHashMap,
    crossbeam_channel::{Receiver, Sender},
    solana_cost_model::block_cost_limits::MAX_BLOCK_UNITS,
    solana_measure::measure_us,
    solana_pubkey::Pubkey,
    solana_runtime_transaction::transaction_with_meta::TransactionWithMeta,
    std::{collections::VecDeque, num::Saturating},
};

pub use super::scheduler_controller::HotAccount;

#[cfg_attr(feature = "dev-context-only-utils", qualifiers(pub))]
pub(crate) struct HotPinnedSchedulerConfig {
    /// High-water cap (`B`) on cumulative scheduled CU per `schedule()`
    /// invocation, distributed evenly across worker threads. Once a
    /// thread's `in_flight + batched` reaches `B / num_threads` it is
    /// ejected from the pass.
    pub max_scheduled_cus: u64,
    /// Low-water threshold (`A`) — a thread is only eligible for refill
    /// at the start of a pass if its in-flight CU has dropped below
    /// `A / num_threads`. Adds hysteresis to the worker's input queue:
    /// the worker drains from `B` down to `A` before the scheduler ships
    /// the next burst of batches, so each refill packs many full
    /// `target_transactions_per_batch` batches instead of one partial.
    /// Must satisfy `0 < refill_threshold_cus <= max_scheduled_cus`.
    pub refill_threshold_cus: u64,
    /// Per-call ceiling on the number of transactions taken out of the
    /// window before bailing — prevents one call from monopolizing the
    /// scheduler thread.
    pub max_scanned_transactions_per_scheduling_pass: usize,
    /// Target depth of the pre-filter look-ahead window. The window is
    /// refilled from the container each outer iteration.
    pub look_ahead_window_size: usize,
    /// Soft target for batch length. A thread whose accumulated batch
    /// reaches this threshold is flushed mid-loop.
    pub target_transactions_per_batch: usize,
    /// Statically declared hot writable accounts. Each is bin-packed
    /// onto exactly one worker thread at scheduler construction; every
    /// future tx writing one of these accounts is forced onto that
    /// thread. Empty by default — the scheduler then behaves as a plain
    /// least-loaded priority-ordered scheduler.
    pub hot_accounts: Vec<HotAccount>,
}

impl Default for HotPinnedSchedulerConfig {
    fn default() -> Self {
        Self {
            max_scheduled_cus: MAX_BLOCK_UNITS,
            // A = B/2 by default: worker drains half the queue between
            // refills, refills ship the other half in full-sized batches.
            refill_threshold_cus: MAX_BLOCK_UNITS / 2,
            max_scanned_transactions_per_scheduling_pass: 1000,
            look_ahead_window_size: 256,
            target_transactions_per_batch: TARGET_NUM_TRANSACTIONS_PER_BATCH,
            hot_accounts: Vec::new(),
        }
    }
}

#[cfg_attr(feature = "dev-context-only-utils", qualifiers(pub))]
pub(crate) struct HotPinnedScheduler<Tx> {
    common: SchedulingCommon<Tx>,
    /// Pre-filtered look-ahead window, drained in container order.
    window: VecDeque<TransactionPriorityId>,
    /// Locks of txs already in an in-progress batch this pass. Only
    /// consulted when `relax_intrabatch_account_locks` is `false`.
    working_account_set: ReadWriteAccountSet,
    /// Static `Pubkey -> ThreadId` map built once at construction by
    /// LPT bin-packing `config.hot_accounts` over `num_threads`. Empty
    /// when no hot accounts are configured.
    hot_account_threads: AHashMap<Pubkey, ThreadId>,
    config: HotPinnedSchedulerConfig,
}

impl<Tx: TransactionWithMeta> HotPinnedScheduler<Tx> {
    #[cfg_attr(feature = "dev-context-only-utils", qualifiers(pub))]
    pub(crate) fn new(
        consume_work_senders: Vec<Sender<ConsumeWork<Tx>>>,
        finished_consume_work_receiver: Receiver<FinishedConsumeWork<Tx>>,
        config: HotPinnedSchedulerConfig,
    ) -> Self {
        let window = VecDeque::with_capacity(config.look_ahead_window_size);
        let num_threads = consume_work_senders.len();
        let hot_account_threads = bin_pack_hot_accounts(&config.hot_accounts, num_threads);
        Self {
            common: SchedulingCommon::new(
                consume_work_senders,
                finished_consume_work_receiver,
                config.target_transactions_per_batch,
            ),
            window,
            working_account_set: ReadWriteAccountSet::default(),
            hot_account_threads,
            config,
        }
    }
}

/// LPT (Longest-Processing-Time-first) bin-packing: sort by descending
/// weight, place each account on the currently-lightest thread. Within
/// a 4/3 factor of optimal makespan, runs once at scheduler start.
fn bin_pack_hot_accounts(
    hot_accounts: &[HotAccount],
    num_threads: usize,
) -> AHashMap<Pubkey, ThreadId> {
    if hot_accounts.is_empty() || num_threads == 0 {
        return AHashMap::new();
    }
    let mut sorted: Vec<&HotAccount> = hot_accounts.iter().collect();
    sorted.sort_by(|a, b| b.weight.cmp(&a.weight));

    let mut load = vec![0u64; num_threads];
    let mut map = AHashMap::with_capacity(hot_accounts.len());
    for acc in sorted {
        let thread_id = (0..num_threads)
            .min_by_key(|&i| load[i])
            .expect("num_threads > 0");
        map.insert(acc.pubkey, thread_id);
        load[thread_id] = load[thread_id].saturating_add(acc.weight.max(1));
    }
    map
}

impl<Tx: TransactionWithMeta> Scheduler<Tx> for HotPinnedScheduler<Tx> {
    fn schedule<S: StateContainer<Tx>>(
        &mut self,
        container: &mut S,
        budget: u64,
        pre_graph_filter: impl Fn(&[&Tx], &mut [bool]),
        pre_lock_filter: impl Fn(&TransactionState<Tx>) -> PreLockFilterAction,
        relax_intrabatch_account_locks: bool,
    ) -> Result<SchedulingSummary, SchedulerError> {
        let mut budget = budget.saturating_sub(
            self.common
                .in_flight_tracker
                .cus_in_flight_per_thread()
                .iter()
                .sum(),
        );

        let starting_queue_size = container.queue_size();
        let starting_buffer_size = container.buffer_size();

        let num_threads = self.common.consume_work_senders.len();
        let max_cu_per_thread = self.config.max_scheduled_cus / num_threads as u64;
        // Low-water mark: skip refill on threads whose in-flight queue is
        // still above this. Clamped to `max_cu_per_thread` so a misconfig
        // can never disable refills entirely.
        let refill_threshold_per_thread = (self.config.refill_threshold_cus
            / num_threads as u64)
            .min(max_cu_per_thread);

        let mut schedulable_threads = ThreadSet::any(num_threads);
        for thread_id in 0..num_threads {
            if self.common.in_flight_tracker.cus_in_flight_per_thread()[thread_id]
                >= refill_threshold_per_thread
            {
                schedulable_threads.remove(thread_id);
            }
        }
        if schedulable_threads.is_empty() {
            return Ok(SchedulingSummary {
                starting_queue_size,
                starting_buffer_size,
                ..SchedulingSummary::default()
            });
        }

        #[cfg(debug_assertions)]
        debug_assert!(
            self.common.batches.is_empty(),
            "batches must start empty for scheduling"
        );

        let target_window_size = self.config.look_ahead_window_size;
        let mut unschedulable_ids: Vec<TransactionPriorityId> = Vec::new();
        let mut num_filtered_out = Saturating::<usize>(0);
        let mut total_filter_time_us = Saturating::<u64>(0);

        // Initial fill of the look-ahead window.
        Self::refill_window(
            container,
            &mut self.window,
            target_window_size,
            &pre_graph_filter,
            &mut num_filtered_out,
            &mut total_filter_time_us,
        );

        let mut num_scanned: usize = 0;
        let mut num_scheduled = Saturating::<usize>(0);
        let mut num_sent = Saturating::<usize>(0);
        let mut num_unschedulable_conflicts: usize = 0;
        let mut num_unschedulable_threads: usize = 0;

        'outer: while budget > 0
            && num_scanned < self.config.max_scanned_transactions_per_scheduling_pass
            && !schedulable_threads.is_empty()
        {
            if self.window.is_empty() {
                break;
            }

            while let Some(id) = self.window.pop_front() {
                num_scanned += 1;

                let Some(transaction_state) = container.get_mut_transaction_state(id.id) else {
                    panic!("transaction state must exist")
                };

                // Strict-locks mode: if this tx conflicts with anything
                // already in an open batch, flush the open batches first
                // so the locks can be re-acquired cleanly. Mirrors the
                // greedy scheduler's behavior at greedy_scheduler.rs:149.
                if !relax_intrabatch_account_locks
                    && !self
                        .working_account_set
                        .check_locks(transaction_state.transaction())
                {
                    self.working_account_set.clear();
                    let Saturating(sent) = num_sent;
                    let new_sent = self.common.send_batches()?;
                    num_sent = Saturating(sent + new_sent);
                }

                let maybe_schedule_info = try_schedule_transaction(
                    transaction_state,
                    &pre_lock_filter,
                    &mut self.common.account_locks,
                    schedulable_threads,
                    &self.hot_account_threads,
                    |thread_set| {
                        select_thread(
                            thread_set,
                            self.common.batches.total_cus(),
                            self.common.in_flight_tracker.cus_in_flight_per_thread(),
                            self.common.batches.transactions(),
                            self.common.in_flight_tracker.num_in_flight_per_thread(),
                        )
                    },
                );

                match maybe_schedule_info {
                    Err(TransactionSchedulingError::UnschedulableConflicts) => {
                        num_unschedulable_conflicts += 1;
                        unschedulable_ids.push(id);
                    }
                    Err(TransactionSchedulingError::UnschedulableThread) => {
                        num_unschedulable_threads += 1;
                        unschedulable_ids.push(id);
                    }
                    Ok(TransactionSchedulingInfo {
                        thread_id,
                        transaction,
                        max_age,
                        cost,
                    }) => {
                        if !relax_intrabatch_account_locks {
                            assert!(
                                self.working_account_set.take_locks(&transaction),
                                "locks must be available"
                            );
                        }
                        num_scheduled += 1;
                        self.common.batches.add_transaction_to_batch(
                            thread_id,
                            id.id,
                            transaction,
                            max_age,
                            cost,
                        );
                        budget = budget.saturating_sub(cost);

                        // Send only this thread's batch when it fills up,
                        // matching the prio-graph scheduler's behavior so
                        // partial batches on other threads keep
                        // accumulating.
                        if self.common.batches.transactions()[thread_id].len()
                            >= self.config.target_transactions_per_batch
                        {
                            let Saturating(sent) = num_sent;
                            let new_sent = self.common.send_batch(thread_id)?;
                            num_sent = Saturating(sent + new_sent);
                        }

                        if self.common.in_flight_tracker.cus_in_flight_per_thread()[thread_id]
                            + self.common.batches.total_cus()[thread_id]
                            >= max_cu_per_thread
                        {
                            // Partial-flush: ship whatever's accumulated on
                            // this thread *before* ejecting it, so the
                            // worker can start on the partial batch now
                            // instead of waiting for the rest of the pass
                            // to scan unschedulable txs. send_batch is a
                            // no-op on empty (scheduler_common.rs:183).
                            let Saturating(sent) = num_sent;
                            let new_sent = self.common.send_batch(thread_id)?;
                            num_sent = Saturating(sent + new_sent);

                            schedulable_threads.remove(thread_id);
                            if schedulable_threads.is_empty() {
                                break;
                            }
                        }
                    }
                }

                if num_scanned >= self.config.max_scanned_transactions_per_scheduling_pass
                    || budget == 0
                {
                    break;
                }
            }

            // Flush whatever's left of this iteration's batches so the
            // worker(s) can start consuming while we refill the window.
            self.working_account_set.clear();
            let Saturating(sent) = num_sent;
            let new_sent = self.common.send_batches()?;
            num_sent = Saturating(sent + new_sent);

            if budget == 0
                || num_scanned >= self.config.max_scanned_transactions_per_scheduling_pass
                || schedulable_threads.is_empty()
            {
                break 'outer;
            }

            Self::refill_window(
                container,
                &mut self.window,
                target_window_size,
                &pre_graph_filter,
                &mut num_filtered_out,
                &mut total_filter_time_us,
            );
        }

        // Final flush — covers the case where we broke out without sending
        // a partial batch.
        self.working_account_set.clear();
        let Saturating(sent) = num_sent;
        let new_sent = self.common.send_batches()?;
        num_sent = Saturating(sent + new_sent);

        // Re-queue txs that proved unschedulable this pass.
        container.push_ids_into_queue(unschedulable_ids.into_iter());

        // Re-queue any remaining pre-filtered window items so we don't lose
        // them across schedule() calls.
        container.push_ids_into_queue(self.window.drain(..));

        let Saturating(num_scheduled) = num_scheduled;
        let Saturating(num_sent) = num_sent;
        let Saturating(num_filtered_out) = num_filtered_out;
        let Saturating(total_filter_time_us) = total_filter_time_us;
        assert_eq!(
            num_scheduled, num_sent,
            "number of scheduled and sent transactions must match"
        );

        Ok(SchedulingSummary {
            starting_queue_size,
            starting_buffer_size,
            num_scheduled,
            num_unschedulable_conflicts,
            num_unschedulable_threads,
            num_filtered_out,
            filter_time_us: total_filter_time_us,
        })
    }

    fn scheduling_common_mut(&mut self) -> &mut SchedulingCommon<Tx> {
        &mut self.common
    }
}

impl<Tx: TransactionWithMeta> HotPinnedScheduler<Tx> {
    fn refill_window<S: StateContainer<Tx>>(
        container: &mut S,
        window: &mut VecDeque<TransactionPriorityId>,
        target_window_size: usize,
        pre_graph_filter: &impl Fn(&[&Tx], &mut [bool]),
        num_filtered_out: &mut Saturating<usize>,
        total_filter_time_us: &mut Saturating<u64>,
    ) {
        const MAX_FILTER_CHUNK_SIZE: usize = 128;
        while window.len() < target_window_size {
            let chunk_size = (target_window_size - window.len()).min(MAX_FILTER_CHUNK_SIZE);
            let mut ids = Vec::with_capacity(chunk_size);
            for _ in 0..chunk_size {
                if let Some(id) = container.pop() {
                    ids.push(id);
                } else {
                    break;
                }
            }
            if ids.is_empty() {
                break;
            }

            let txs: Vec<&Tx> = ids
                .iter()
                .map(|id| container.get_transaction(id.id).unwrap())
                .collect();
            let mut filter = vec![true; ids.len()];
            let (_, filter_us) = measure_us!(pre_graph_filter(&txs, &mut filter));
            *total_filter_time_us += filter_us;

            let drained = ids.len();
            for (id, keep) in ids.into_iter().zip(filter) {
                if keep {
                    window.push_back(id);
                } else {
                    *num_filtered_out += 1;
                    container.remove_by_id(id.id);
                }
            }

            // Container ran dry mid-chunk — no point spinning further.
            if drained < chunk_size {
                break;
            }
        }
    }
}

fn try_schedule_transaction<Tx: TransactionWithMeta>(
    transaction_state: &mut TransactionState<Tx>,
    pre_lock_filter: impl Fn(&TransactionState<Tx>) -> PreLockFilterAction,
    account_locks: &mut ThreadAwareAccountLocks,
    allowed_threads: ThreadSet,
    hot_account_threads: &AHashMap<Pubkey, ThreadId>,
    thread_selector: impl FnOnce(ThreadSet) -> ThreadId,
) -> Result<TransactionSchedulingInfo<Tx>, TransactionSchedulingError> {
    match pre_lock_filter(transaction_state) {
        PreLockFilterAction::AttemptToSchedule => {}
    }

    let transaction = transaction_state.transaction();

    let account_keys = transaction.account_keys();
    let write_account_locks = account_keys
        .iter()
        .enumerate()
        .filter_map(|(index, key)| transaction.is_writable(index).then_some(key));
    let read_account_locks = account_keys
        .iter()
        .enumerate()
        .filter_map(|(index, key)| (!transaction.is_writable(index)).then_some(key));

    // If this tx writes a configured hot account, force it onto that
    // account's pre-assigned thread by intersecting `allowed_threads`.
    // Premise: at most one hot writable per tx — if more are ever
    // observed, we silently fall back to the unconstrained set so the
    // tx can still make progress.
    let allowed_threads = if hot_account_threads.is_empty() {
        allowed_threads
    } else {
        let mut hot_thread: Option<ThreadId> = None;
        let mut multiple_hot = false;
        for (index, key) in account_keys.iter().enumerate() {
            if !transaction.is_writable(index) {
                continue;
            }
            if let Some(&t) = hot_account_threads.get(key) {
                if hot_thread.is_some_and(|prev| prev != t) {
                    multiple_hot = true;
                    break;
                }
                hot_thread = Some(t);
            }
        }
        match (hot_thread, multiple_hot) {
            (Some(t), false) => allowed_threads & ThreadSet::only(t),
            _ => allowed_threads,
        }
    };

    let thread_id = match account_locks.try_lock_accounts(
        write_account_locks,
        read_account_locks,
        allowed_threads,
        thread_selector,
    ) {
        Ok(thread_id) => thread_id,
        Err(TryLockError::MultipleConflicts) => {
            return Err(TransactionSchedulingError::UnschedulableConflicts);
        }
        Err(TryLockError::ThreadNotAllowed) => {
            return Err(TransactionSchedulingError::UnschedulableThread);
        }
    };

    let (transaction, max_age) = transaction_state.take_transaction_for_scheduling();
    let cost = transaction_state.cost();

    Ok(TransactionSchedulingInfo {
        thread_id,
        transaction,
        max_age,
        cost,
    })
}
