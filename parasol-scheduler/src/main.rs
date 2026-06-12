use std::{collections::{BTreeSet, BinaryHeap, VecDeque}, time::Duration};

use agave_scheduler_bindings::{pack_message_flags::{self, check_flags}, worker_message_types::{not_included_reasons, parsing_and_sanitization_flags, resolve_flags, status_check_flags}, MAX_TRANSACTIONS_PER_MESSAGE};
use agave_scheduling_utils::{bridge::{KeyedTransactionMeta, ScheduleBatch, TransactionKey, TransactionState, TxDecision, WorkerAction}, handshake::ClientLogon, thread_aware_account_locks::{self, ThreadAwareAccountLocks, ThreadId, ThreadSet}};
use clap::Parser;
use serde::Deserialize;
use slotmap::SlotMap;
use solana_transaction::Address;
use ahash::AHashMap;

#[derive(Parser)]
struct Args {
    #[arg(long)]
    socket: String,
    #[arg(long)]
    config_path: String,
}

#[derive(Deserialize, Debug)]
struct TpuConfig {
    pub worker_count: usize,
    pub allocator_size: usize,
    pub tpu_to_pack_capacity: usize,
    pub progress_tracker_capacity: usize,
    pub pack_to_worker_capacity: usize,
    pub worker_to_pack_capacity: usize,
}


fn default_connect_timeout() -> std::time::Duration { std::time::Duration::from_millis(100) }

fn default_check_txs() -> usize { 0 }

#[derive(Deserialize, Debug)]
struct Config {
    pub tpu: TpuConfig,
    #[serde(with = "humantime_serde")]
    #[serde(default="default_connect_timeout")]
    pub connect_timeout: Duration,
    #[serde(default)]
    #[serde(with = "humantime_serde")]
    pub spin_delay: Option<Duration>,
    pub drain_tpu_granularity: usize,
    pub send_to_worker_granularity: usize,
    #[serde(default="default_check_txs")]
    pub check_max_inflight: usize,
    pub max_txs_per_worker: usize,
    pub slot_deadline: u64,
    #[serde(with = "humantime_serde")]
    pub report_delay: Option<Duration>,
    max_queue_size: usize,
}

slotmap::new_key_type! {
    struct SchedulerTxKey;
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum TxState {
    Enqueued,
    Active,
    Backpressured,
    Picked
}

impl Default for TxState {
    fn default() -> Self {
        TxState::Enqueued
    }
}

#[derive(Clone, Default)]
struct TxMeta {
    waiting_queue: usize,
    shared_key: TransactionKey,
    affinity_requirements: Vec<usize>,
    affinity_requirements_count: usize,
    state: TxState,
    score: usize,
    expire_slot: u64,
}

impl TxMeta {
    fn new(shared_key: TransactionKey, score: usize, slot: u64) -> Self {
        Self {
            shared_key,
            waiting_queue: 0,
            affinity_requirements: Vec::new(),
            affinity_requirements_count: 0,
            state: TxState::default(),
            score,
            expire_slot: slot
        }
    }

    fn set_affinity_requirements(&mut self, reqs: Vec<usize>) {
        self.affinity_requirements_count = reqs.iter().filter(|x| **x > 0).count();
        assert!(reqs.is_empty() || self.affinity_requirements_count > 0);
        self.affinity_requirements = reqs;
    }

    fn remove_affinity_requirement(&mut self, thread: usize) -> bool {
        self.affinity_requirements[thread] -= 1;
        if self.affinity_requirements[thread] == 0 {
            self.affinity_requirements_count -= 1;
            true
        } else {
            false
        }
    }
}

// tx lifecycle:
// 1. Enqueued for all the resources
// 2. Dequeued -> blocked state
//  .. here affinity constraints are calculated (max 1 for each locked account)
// 3. (affinity constraints + backpressure passed) -> picked state
// picked means that we already decided which tread it will be executed on

#[derive(Default)]
struct ResourceLockingQueue {
    acquire_queue: VecDeque<(SchedulerTxKey, /*is_write*/ bool)>,
    blocked_reads: usize,
    blocked_writes: usize,
}

impl ResourceLockingQueue {
    fn empty(&self) -> bool {
        self.acquire_queue.is_empty() && self.blocked_reads == 0 && self.blocked_writes == 0
    }

    fn push(&mut self, key: SchedulerTxKey, is_write: bool) -> bool {
        if self.acquire_queue.is_empty() && self.blocked_writes == 0 {
            if self.blocked_reads == 0 {
                if is_write {
                    self.blocked_writes += 1;
                } else {
                    self.blocked_reads += 1;
                }
                return false;
            } else if !is_write {
                self.blocked_reads += 1;
                return false;
            }
        }
        self.acquire_queue.push_back((key, is_write));
        true
    }

    fn unblock(&mut self, is_write: bool) {
        if is_write {
            assert!(self.blocked_writes > 0);
            self.blocked_writes -= 1;
        } else {
            assert!(self.blocked_reads > 0);
            self.blocked_reads -= 1;
        }
    }

    fn drain(&mut self) -> Option<SchedulerTxKey> {
        if self.blocked_writes > 0 {
            return None;
        }

        if self.blocked_reads > 0 {
            if let Some((_, is_write)) = self.acquire_queue.front() {
                if *is_write {
                    return None;
                }
            }
        }

        if let Some((key, is_write)) = self.acquire_queue.pop_front() {
            if is_write {
                self.blocked_writes += 1;
            } else {
                self.blocked_reads += 1;
            }
            Some(key)
        } else {
            None
        }
    }
}

#[derive(Eq, Ord)]
struct PickedTx(usize, SchedulerTxKey);

impl PartialEq for PickedTx {
    fn eq(&self, other: &PickedTx) -> bool {
        self.0.eq(&other.0)
    }
}

impl PartialOrd for PickedTx {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        self.0.partial_cmp(&other.0)
    }
}

#[derive(Default)]
struct AccWaiters {
    readers: Vec<SchedulerTxKey>,
    writers: Vec<SchedulerTxKey>,
    deregisters: usize
}

impl AccWaiters {
    fn drain(&mut self, ro: bool) -> Vec<SchedulerTxKey> {
        let mut result = std::mem::take(&mut self.readers);
        if !ro {
            result.append(&mut self.writers);
        }
        result
    }

    fn mark_deregister(&mut self) -> bool {
        self.deregisters += 1;
        self.deregisters * 2 >= self.readers.len() + self.writers.len()
    }

    fn retain(&mut self, predicate: &impl Fn(&SchedulerTxKey) -> bool)  {
        self.readers.retain(predicate);
        self.writers.retain(predicate);
        self.deregisters = 0;
    }
}

trait TxDataProvider {
    fn txdata(&self, key: TransactionKey) -> &TransactionState;
}

impl<T: Copy> TxDataProvider for agave_scheduling_utils::bridge::SchedulerBindingsBridge<T> {
    fn txdata(&self, key: TransactionKey) -> &TransactionState {
        self.transaction(key)
    }
}

trait MutableTxProvider: TxDataProvider {
    fn remove_tx(&mut self, key: TransactionKey);
}

impl<T: Copy> MutableTxProvider for agave_scheduling_utils::bridge::SchedulerBindingsBridge<T> {
    fn remove_tx(&mut self, key: TransactionKey) {
        self.drop_transaction(key);
    }
}

enum Event {
    AddressUnblocked(Address),
    AffinityRequirementDropped(SchedulerTxKey),
}

struct LockingQueue {
    resources: AHashMap<Address, ResourceLockingQueue>,
    metas: slotmap::SlotMap<SchedulerTxKey, TxMeta>,
    picked: Vec<BinaryHeap<PickedTx>>,

    affinity_waiters: AHashMap<Address, AccWaiters>,
    backpressured: Vec<BinaryHeap<PickedTx>>,
    max_worker_backlog: usize,

    picked_locks: ThreadAwareAccountLocks,
    num_threads: usize,

    events_to_dispatch: VecDeque<Event>,

    backlogs: Vec<usize>,

    seen_txs: usize,

    slot: u64,
    expires: usize
}

fn make_vector<T>(size: usize, f: impl FnMut() -> T) -> Vec<T> {
    let mut picked = Vec::new();
    picked.resize_with(size, f);
    picked
}

#[allow(dead_code)]
impl LockingQueue {
    fn new(num_threads: usize, max_worker_backlog: usize) -> Self {
        assert!(num_threads > 0);
        Self {
            max_worker_backlog,
            resources: AHashMap::new(),
            metas: SlotMap::with_key(),
            picked: make_vector(num_threads, || BinaryHeap::new()),
            backpressured: make_vector(num_threads, || BinaryHeap::new()),
            picked_locks: ThreadAwareAccountLocks::new(num_threads),
            num_threads,
            backlogs: make_vector(num_threads, || 0),
            affinity_waiters: AHashMap::new(),
            events_to_dispatch: VecDeque::new(),
            seen_txs: 0,
            slot: 0,
            expires: 0
        }
    }

    fn size(&self) -> usize {
        self.metas.len()
    }

    fn start_waiting_for_workers(&mut self, key: SchedulerTxKey, tx: &TransactionState) {
        let tx_meta = self.metas.get_mut(key).unwrap();
        assert!(tx_meta.waiting_queue == 0);
        tx_meta.state = TxState::Active;
        let mut requirements = Vec::new();
        let mut requirements_count = 0;
        let mut register_affinity = |thread: usize, addr: Address, is_write: bool| {
            requirements_count += 1;
            if requirements.is_empty() {
                requirements.resize(self.num_threads, 0);
            }
            let waiters = self.affinity_waiters.entry(addr).or_default();
            if is_write {
                waiters.writers.push(key);
            } else {
                waiters.readers.push(key);
            }
            requirements[thread] += 1;
        };

        for (lock, is_write) in tx.locks() {
            if let Some(acc_locks) = self.picked_locks.acc_locks(lock) {
                if is_write {
                    if let Some(locks) = &acc_locks.read_locks {
                        for thread in locks.thread_set.contained_threads_iter() {
                            register_affinity(thread, *lock, is_write);
                        }
                    }
                }
                if let Some(locks) = &acc_locks.write_locks {
                    register_affinity(locks.thread_id, *lock, is_write);
                }
            }
        }

        tx_meta.set_affinity_requirements(requirements);
        if tx_meta.affinity_requirements_count <= 1 {
            self.try_pick_tx(key, tx, true);
            assert!({
                let tx_meta = self.metas.get(key).unwrap();
                tx_meta.state == TxState::Picked || tx_meta.state == TxState::Backpressured
            });
        }
    }

    fn new_tx(&mut self, meta: TxMeta, tx: &TransactionState) {
        self.seen_txs += 1;
        let key = self.metas.insert(meta);
        let meta = self.metas.get_mut(key).unwrap();
        for (lock, is_write) in tx.locks() {
            if self.resources.entry(*lock).or_default().push(key, is_write) {
                meta.waiting_queue += 1;
            }
        }
        if meta.waiting_queue == 0 {
            self.start_waiting_for_workers(key, tx);
        }
    }

    fn try_pick_tx(&mut self, key: SchedulerTxKey, txdata: &TransactionState, was_blocked: bool) -> bool {
        let write_locks = txdata.write_locks().collect::<smallvec::SmallVec<[_; 64]>>();
        let read_locks = txdata.read_locks().collect::<smallvec::SmallVec<[_; 64]>>();
        let tx_meta = self.metas.get_mut(key).unwrap();

        if tx_meta.state == TxState::Picked {
            return false;
        }
        assert!(tx_meta.state != TxState::Enqueued);
        assert!(tx_meta.waiting_queue == 0);

        let lock_result = self.picked_locks.try_lock_accounts(
            write_locks.as_slice().iter().cloned(),
            read_locks.as_slice().iter().cloned(),
            ThreadSet::any(self.num_threads),
            |threads| {
                threads.contained_threads_iter()
                    .min_by(|thread1, thread2|
                        (self.backlogs[*thread1])
                            .cmp(&(self.backlogs[*thread2]))).unwrap()
            });

        match lock_result {
            Ok(thread) => {
                let score = tx_meta.score;
                if self.backlogs[thread] >= self.max_worker_backlog {
                    tx_meta.state = TxState::Backpressured;
                    self.backpressured[thread].push(PickedTx(score, key));
                    self.picked_locks.unlock_accounts(
                        write_locks.as_slice().iter().cloned(),
                        read_locks.as_slice().iter().cloned(),
                        thread);
                    return false;
                }

                self.backlogs[thread] += 1;

                self.picked[thread].push(PickedTx(score, key));
                tx_meta.state = TxState::Picked;
                if was_blocked {
                    for (lock, is_write) in txdata.locks() {
                        if {
                            let locks = self.resources.get_mut(lock).unwrap();
                            locks.unblock(is_write);
                            locks.empty()
                        } {
                            self.resources.remove(lock);
                        } else {
                            self.events_to_dispatch.push_back(Event::AddressUnblocked(*lock));
                        }
                    }
                }

                true
            },
            Err(err) => {
                assert!(tx_meta.affinity_requirements_count > 0);
                match err {
                    thread_aware_account_locks::TryLockError::MultipleConflicts => false,
                    thread_aware_account_locks::TryLockError::ThreadNotAllowed => unreachable!("We are explicitly allowing all threads here")
                }
            }
        }

    }

    // returns true if the key is already non-valid (expired)
    fn maybe_expire(&mut self, key: SchedulerTxKey, txdata: &mut impl MutableTxProvider) -> bool {
        let (shared_key, expire_slot) = {
            let Some(meta) = self.metas.get(key) else {
                return true;
            };
            (meta.shared_key, meta.expire_slot)
        };
        if expire_slot <= self.slot {
            self.expires += 1;
            txdata.remove_tx(shared_key);
            self.metas.remove(key);
            true
        } else {
            false
        }
    }

    fn dispatch_event(&mut self, ev: Event, txdata: &mut impl MutableTxProvider) {
        match ev {
            Event::AffinityRequirementDropped(key) => {
                if self.maybe_expire(key, txdata) {
                    return;
                }
                let Some(meta) = self.metas.get(key) else {
                    return;
                };
                let txdata = txdata.txdata(meta.shared_key);
                self.try_pick_tx(key, txdata, true);
            }
            Event::AddressUnblocked(address) => {
                while let Some(key) = self.resources.get_mut(&address).and_then(|q| q.drain()) {
                    if self.maybe_expire(key, txdata) {
                        return;
                    }
                    let (waiting_queue, txdata) = {
                        let meta = self.metas.get_mut(key).unwrap();
                        assert!(meta.waiting_queue > 0);
                        meta.waiting_queue -= 1;
                        (meta.waiting_queue, txdata.txdata(meta.shared_key))
                    };
                    if waiting_queue == 0 {
                        self.start_waiting_for_workers(key, txdata);
                    }
                }
                if self.resources.get(&address).map(|q| q.empty()) == Some(true) {
                    self.resources.remove(&address);
                }
            }
        }
    }

    fn drain_backpressured(&mut self, thread: usize, txdata: &mut impl MutableTxProvider) {
        while self.backlogs[thread] < self.max_worker_backlog {
            let Some(PickedTx(_, key)) = self.backpressured[thread].pop() else {
                break;
            };

            if self.metas.get(key).map(|meta| meta.state == TxState::Picked).unwrap_or(true) {
                continue;
            }

            if self.maybe_expire(key, txdata) {
                continue;
            }

            let txdata = txdata.txdata({
                let meta = self.metas.get(key).unwrap();
                assert!(meta.state == TxState::Backpressured);
                meta.shared_key
            });

            assert!(self.try_pick_tx(key, txdata, true));
        }
    }

    fn dispatch_events(&mut self, txdata: &mut impl MutableTxProvider) {
        while let Some(ev) = self.events_to_dispatch.pop_front() {
            self.dispatch_event(ev, txdata);
        }
    }

    fn remove_completed(&mut self, key: SchedulerTxKey, tx: &TransactionState, worker: ThreadId) {
        self.picked_locks.unlock_accounts(tx.write_locks(), tx.read_locks(), worker);
        self.backlogs[worker] -= 1;
        for (lock, _) in tx.locks() {
            if let Some(waiters) = self.affinity_waiters.get_mut(lock) {
                let acc_locks = self.picked_locks.acc_locks(lock);
                if acc_locks.map(|a| a.write_locks.is_some()) == Some(true) {
                    continue;
                }

                for waiter in waiters.drain(acc_locks.map_or(false, |acc| acc.read_locks.is_some())).into_iter() {
                    if let Some(tx_meta) = self.metas.get_mut(waiter) {
                        if tx_meta.remove_affinity_requirement(worker) && tx_meta.affinity_requirements_count <= 1 {
                            self.events_to_dispatch.push_back(Event::AffinityRequirementDropped(waiter));
                        }
                    }
                }
                if waiters.mark_deregister() {
                    waiters.retain(&|key| self.metas.get(*key).is_some());
                }
            }
        }
        self.metas.remove(key);
    }
}

fn tx_score(_data: &TransactionState) -> usize {
    0
}


fn main() {
    let args = Args::parse();
    env_logger::init();

    let config: Config = toml::from_slice(&std::fs::read(args.config_path).unwrap()).unwrap();
    let logon = ClientLogon {
        allocator_size: config.tpu.allocator_size,
        pack_to_worker_capacity: config.tpu.pack_to_worker_capacity,
        progress_tracker_capacity: config.tpu.progress_tracker_capacity,
        allocator_handles: 1,
        tpu_to_pack_capacity: config.tpu.tpu_to_pack_capacity,
        worker_count: config.tpu.worker_count + 1,
        worker_to_pack_capacity: config.tpu.worker_to_pack_capacity,
        flags: 0
    };

    let session = agave_scheduling_utils::handshake::client::connect(args.socket, logon, config.connect_timeout).unwrap();
    //let ClientSession{mut tpu_to_pack, ref allocators, .. } = session;

    let workers = session.workers.len() - 1;
    let check_worker = workers;

    let mut bridge = agave_scheduling_utils::bridge::SchedulerBindingsBridge::<SchedulerTxKey>::new(session);
    let mut to_check = BTreeSet::new();
    let mut check_inflight = 0;

    let mut locking_queue = LockingQueue::new(workers, config.max_txs_per_worker);

    let mut slot = 0;
    let mut last_report = std::time::Instant::now();
    let mut reschedules = 0;
    let mut drops = 0;

    let mut sent_stats = make_vector(workers, || 0);
    let mut receive_stats = make_vector(workers, || 0);

    loop {
        let mut to_spin = true;
        if let Some(item) = bridge.drain_progress() {
            slot = item.current_slot;
            locking_queue.slot = slot;
        }

        bridge.drain_tpu(|bridge, tx_key| {
            to_spin = false;
            let data = bridge.transaction(tx_key);
            if locking_queue.size() >= config.max_queue_size {
                drops += 1;
                return TxDecision::Drop;
            }
            if config.check_max_inflight > 0 {
                to_check.insert(tx_key);
            } else {
                locking_queue.new_tx(TxMeta::new(tx_key, tx_score(data), slot + config.slot_deadline), data);
            }
            TxDecision::Keep

        }, config.drain_tpu_granularity);

        if !to_check.is_empty() {
            let mut batch = Vec::new();
            while check_inflight < config.check_max_inflight && !to_check.is_empty() {
                let key = to_check.pop_last().unwrap();
                batch.push(KeyedTransactionMeta { key, meta: SchedulerTxKey::default() });
                check_inflight += 1;
            }
            bridge.schedule(ScheduleBatch {
                worker: check_worker,
                max_working_slot: slot + config.slot_deadline,
                flags: pack_message_flags::CHECK | check_flags::STATUS_CHECKS | check_flags::LOAD_FEE_PAYER_BALANCE | check_flags::LOAD_ADDRESS_LOOKUP_TABLES,
                transactions: &batch
            }).unwrap();
        }

        bridge.drain_worker(check_worker, |bridge, worker_resp| {
            match worker_resp.response {
                WorkerAction::Check(response, _pubkeys) => {
                    if locking_queue.size() >= config.max_queue_size {
                        drops += 1;
                        return TxDecision::Drop;
                    }
                    if response.parsing_and_sanitization_flags & parsing_and_sanitization_flags::FAILED != 0 {
                        TxDecision::Drop
                    } else if response.status_check_flags != status_check_flags::REQUESTED | status_check_flags::PERFORMED {
                        TxDecision::Drop
                    } else if response.resolve_flags & resolve_flags::FAILED != 0 {
                        TxDecision::Drop
                    } else {
                        locking_queue.new_tx(
                            TxMeta::new(
                                worker_resp.key,
                                tx_score(bridge.transaction(worker_resp.key)),
                                slot + config.slot_deadline),
                            bridge.transaction(worker_resp.key));
                        TxDecision::Keep
                    }
                },
                WorkerAction::Unprocessed => {
                    TxDecision::Drop
                }
                WorkerAction::Execute(_) => unreachable!("unexpected response from check worker")
            }
        }, config.check_max_inflight);

        for worker in 0..workers {
            bridge.drain_worker(worker, |bridge, worker_resp| {
                receive_stats[worker] += 1;
                to_spin = false;
                let shared_key = worker_resp.key;
                let key = worker_resp.meta;
                match worker_resp.response {
                    WorkerAction::Check(_, _) => panic!("unexpected worker response"),
                    WorkerAction::Execute(item) => {
                        assert!(item.not_included_reason != not_included_reasons::ACCOUNT_IN_USE);
                        let score = locking_queue.metas.get(key).unwrap().score;
                        if item.not_included_reason == not_included_reasons::WOULD_EXCEED_MAX_ACCOUNT_COST_LIMIT ||
                            item.not_included_reason == not_included_reasons::WOULD_EXCEED_ACCOUNT_DATA_BLOCK_LIMIT ||
                            item.not_included_reason == not_included_reasons::PROGRAM_EXECUTION_TEMPORARILY_RESTRICTED
                        {
                            reschedules += 1;
                            locking_queue.picked[worker].push(PickedTx(score, key));
                            TxDecision::Keep
                        } else {
                            locking_queue.remove_completed(key, bridge.transaction(shared_key), worker);
                            TxDecision::Drop
                        }
                    },
                    WorkerAction::Unprocessed => TxDecision::Drop // max_working_slot violation
                }
            }, config.max_txs_per_worker);
        }

        locking_queue.dispatch_events(&mut bridge);
        if let Some(period) = config.report_delay {
            let now = std::time::Instant::now();
            if last_report + period < now {
                let mut statuses = vec![0; 4];
                for (k, v) in locking_queue.metas.iter() {
                    match v.state {
                        TxState::Enqueued => {
                            assert!(v.waiting_queue > 0);
                            assert!(v.affinity_requirements_count == 0);
                            statuses[0] += 1;
                            let txdata = bridge.transaction(v.shared_key);
                            let mut blocked = 0;
                            for (lock, is_write) in txdata.locks() {
                                let Some(q) = locking_queue.resources.get(lock) else {
                                    continue;
                                };
                                for (tx, enq_is_write) in q.acquire_queue.iter() {
                                    if k == *tx {
                                        blocked += 1;
                                        assert!(is_write == *enq_is_write);
                                    }
                                }
                            }
                            assert!(blocked == v.waiting_queue);
                        }
                        TxState::Active => {
                            statuses[1] += 1;
                            assert!(v.waiting_queue == 0);
                            assert!(v.affinity_requirements_count > 0);
                        }
                        TxState::Backpressured => {
                            statuses[2] += 1;
                            assert!(v.waiting_queue == 0);
                        }
                        TxState::Picked => {
                            statuses[3] += 1;
                            assert!(v.waiting_queue == 0);
                            assert!(v.affinity_requirements_count <= 1);
                        }
                    }
                }
                for (_, v) in locking_queue.resources.iter() {
                    if !v.acquire_queue.is_empty() {
                        assert!(v.blocked_reads > 0 || v.blocked_writes > 0);
                    }
                }

                let expires = locking_queue.expires;
                let passed_txs = locking_queue.seen_txs;
                let queue_len = locking_queue.metas.len();
                let picked_queue_len = locking_queue.picked.iter().filter(|x| !x.is_empty()).count();
                println!("{}/{workers} saturated backlogs {:?} (sent/receive stats {sent_stats:?}/{receive_stats:?}); enqueued txs {queue_len}/{statuses:?}/{picked_queue_len}; txs seen/expires/reschedules/drops {passed_txs}/{expires}/{reschedules}/{drops} slot {slot}",
                    locking_queue.backlogs.iter().filter(|x| **x>0).count(), locking_queue.backlogs);
                last_report = now;
            }
        }

        for worker in 0..workers {
            locking_queue.drain_backpressured(worker, &mut bridge);
            let mut batch = smallvec::SmallVec::<[_; MAX_TRANSACTIONS_PER_MESSAGE]>::new();
            macro_rules! send_batch {
                () => {
                    assert!(batch.len() < MAX_TRANSACTIONS_PER_MESSAGE);
                    bridge.schedule(ScheduleBatch{
                        worker,
                        transactions: batch.as_slice(),
                        max_working_slot: slot + config.slot_deadline,
                        flags: pack_message_flags::EXECUTE
                    }).unwrap();
                    sent_stats[worker] += batch.len();
                    batch.clear();
                };
            }
            while let Some(PickedTx(_, key)) = locking_queue.picked[worker].pop() {
                batch.push(KeyedTransactionMeta::<SchedulerTxKey>{
                    key: locking_queue.metas.get(key).unwrap().shared_key,
                    meta: key
                });
                if batch.len() == config.send_to_worker_granularity {
                    send_batch!();
                }
            }
            if !batch.is_empty() {
                send_batch!();
            }

        }

        if to_spin {
            if let Some(delay) = config.spin_delay {
                std::thread::sleep(delay);
            }
        }
    }
}
