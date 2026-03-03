use {
    crossbeam_channel::Sender,
    log::{info, warn},
    solana_clock::Slot,
    solana_ledger::{blockstore::Blockstore, leader_schedule_cache::LeaderScheduleCache},
    solana_measure::measure::Measure,
    solana_pubkey::Pubkey,
    solana_rpc::{
        optimistically_confirmed_bank_tracker::{BankNotification, BankNotificationSenderConfig},
        rpc_subscriptions::RpcSubscriptions,
    },
    solana_runtime::{
        bank_forks::{BankForks, SetRootError},
        installed_scheduler_pool::BankWithScheduler,
        snapshot_controller::SnapshotController,
    },
    std::sync::{Arc, RwLock},
};

/// Sets the new root, additionally performs the callback after setting the bank forks root
/// During this transition period where both replay stage and votor can root depending on the feature flag we
/// have a callback that cleans up progress map and other tower bft structures. Then the callgraph is
///
/// ReplayStage::check_and_handle_new_root -> root_utils::check_and_handle_new_root(callback)
///                                                             |
///                                                             v
/// ReplayStage::handle_new_root           -> root_utils::set_bank_forks_root(callback) -> callback()
///
/// Votor does not need the progress map or other tower bft structures, so it will not use the callback.
#[allow(clippy::too_many_arguments)]
pub fn check_and_handle_new_root<CB>(
    parent_slot: Slot,
    new_root: Slot,
    snapshot_controller: Option<&SnapshotController>,
    highest_super_majority_root: Option<Slot>,
    bank_notification_sender: &Option<BankNotificationSenderConfig>,
    drop_bank_sender: &Sender<Vec<BankWithScheduler>>,
    blockstore: &Blockstore,
    leader_schedule_cache: &Arc<LeaderScheduleCache>,
    bank_forks: &RwLock<BankForks>,
    rpc_subscriptions: Option<&RpcSubscriptions>,
    my_pubkey: &Pubkey,
    callback: CB,
) -> Result<(), SetRootError>
where
    CB: FnOnce(&BankForks),
{
    let mut total_time = Measure::start("check_and_handle_new_root_total");

    // get the root bank before squash
    let prev_root = bank_forks.read().unwrap().root();
    let root_bank = bank_forks
        .read()
        .unwrap()
        .get(new_root)
        .expect("Root bank doesn't exist");

    let mut parents_time = Measure::start("parents_traversal");
    let mut rooted_banks = root_bank.parents();
    parents_time.stop();
    let full_chain_len = rooted_banks.len();

    let oldest_parent = rooted_banks.last().map(|last| last.parent_slot());
    rooted_banks.push(root_bank.clone());
    let rooted_slots: Vec<_> = rooted_banks.iter().map(|bank| bank.slot()).collect();

    // For large root jumps, only mark slots above the previous root as newly rooted.
    // Slots at or below prev_root are already rooted in blockstore.
    let new_rooted_slots: Vec<_> = rooted_slots
        .iter()
        .copied()
        .filter(|&slot| slot > prev_root)
        .collect();

    info!(
        "[FINALIZE_DIAG] check_and_handle_new_root: new_root={} prev_root={} \
         parents_chain_len={} new_rooted_slots={} parents_ms={}",
        new_root, prev_root, full_chain_len, new_rooted_slots.len(), parents_time.as_ms()
    );

    // The following differs from rooted_slots by including the parent slot of the oldest parent bank.
    let rooted_slots_with_parents = bank_notification_sender
        .as_ref()
        .is_some_and(|sender| sender.should_send_parents)
        .then(|| {
            let mut new_chain = rooted_slots.clone();
            new_chain.push(oldest_parent.unwrap_or(parent_slot));
            new_chain
        });

    // Call leader schedule_cache.set_root() before blockstore.set_root() because
    // bank_forks.root is consumed by repair_service to update gossip, so we don't want to
    // get shreds for repair on gossip before we update leader schedule, otherwise they may
    // get dropped.
    leader_schedule_cache.set_root(rooted_banks.last().unwrap());

    let mut set_roots_time = Measure::start("blockstore_set_roots");
    blockstore
        .set_roots(new_rooted_slots.iter())
        .expect("Ledger set roots failed");
    set_roots_time.stop();

    let mut set_bank_forks_root_time = Measure::start("set_bank_forks_root");
    set_bank_forks_root(
        new_root,
        bank_forks,
        snapshot_controller,
        highest_super_majority_root,
        drop_bank_sender,
        callback,
    )?;
    set_bank_forks_root_time.stop();

    blockstore.slots_stats.mark_rooted(new_root);

    let mut notify_time = Measure::start("notify_roots");
    if let Some(rpc_subscriptions) = rpc_subscriptions {
        // Only notify newly rooted slots to avoid flooding subscription handlers.
        rpc_subscriptions.notify_roots(new_rooted_slots);
    }
    notify_time.stop();

    if let Some(sender) = bank_notification_sender {
        let dependency_work = sender
            .dependency_tracker
            .as_ref()
            .map(|s| s.get_current_declared_work());
        sender
            .sender
            .send((BankNotification::NewRootBank(root_bank), dependency_work))
            .unwrap_or_else(|err| warn!("bank_notification_sender failed: {err:?}"));

        if let Some(new_chain) = rooted_slots_with_parents {
            let dependency_work = sender
                .dependency_tracker
                .as_ref()
                .map(|s| s.get_current_declared_work());
            sender
                .sender
                .send((BankNotification::NewRootedChain(new_chain), dependency_work))
                .unwrap_or_else(|err| warn!("bank_notification_sender failed: {err:?}"));
        }
    }

    total_time.stop();
    info!(
        "[FINALIZE_DIAG] check_and_handle_new_root done: new_root={} \
         set_roots_ms={} set_bank_forks_root_ms={} notify_ms={} total_ms={}",
        new_root,
        set_roots_time.as_ms(),
        set_bank_forks_root_time.as_ms(),
        notify_time.as_ms(),
        total_time.as_ms()
    );
    info!("{my_pubkey}: new root {new_root}");
    Ok(())
}

/// Sets the bank forks root:
/// - Prune the program cache
/// - Prune bank forks and drop the removed banks
/// - Calls the callback for use in replay stage and tests
pub fn set_bank_forks_root<CB>(
    new_root: Slot,
    bank_forks: &RwLock<BankForks>,
    snapshot_controller: Option<&SnapshotController>,
    highest_super_majority_root: Option<Slot>,
    drop_bank_sender: &Sender<Vec<BankWithScheduler>>,
    callback: CB,
) -> Result<(), SetRootError>
where
    CB: FnOnce(&BankForks),
{
    let mut prune_cache_time = Measure::start("prune_program_cache");
    bank_forks.read().unwrap().prune_program_cache(new_root);
    prune_cache_time.stop();

    let mut set_root_time = Measure::start("bank_forks_set_root");
    let removed_banks = bank_forks.write().unwrap().set_root(
        new_root,
        snapshot_controller,
        highest_super_majority_root,
    )?;
    set_root_time.stop();

    let removed_count = removed_banks.len();
    let mut send_time = Measure::start("drop_bank_send");
    drop_bank_sender
        .send(removed_banks)
        .unwrap_or_else(|err| warn!("bank drop failed: {err:?}"));
    send_time.stop();

    let mut callback_time = Measure::start("callback");
    let r_bank_forks = bank_forks.read().unwrap();
    callback(&r_bank_forks);
    callback_time.stop();

    info!(
        "[FINALIZE_DIAG] set_bank_forks_root: new_root={} prune_cache_ms={} \
         set_root_ms={} removed_banks={} send_ms={} callback_ms={}",
        new_root,
        prune_cache_time.as_ms(),
        set_root_time.as_ms(),
        removed_count,
        send_time.as_ms(),
        callback_time.as_ms()
    );
    Ok(())
}
