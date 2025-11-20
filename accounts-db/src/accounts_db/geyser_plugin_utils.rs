use {
    crate::{
        accounts_db::{AccountStorageEntry, AccountsDb},
        accounts_update_notifier_interface::AccountsUpdateNotifierInterface,
        append_vec,
        buffered_reader::RequiredLenBufFileRead,
    },
    solana_account::{AccountSharedData, ReadableAccount},
    solana_clock::Slot,
    solana_measure::meas_dur,
    solana_message::inner_instruction::InnerInstructionsList,
    solana_metrics::*,
    solana_pubkey::Pubkey,
    solana_sdk_ids::sysvar as sysvar_program,
    solana_system_interface::program as system_program,
    solana_transaction::sanitized::SanitizedTransaction,
    std::{
        cmp::Reverse,
        ops::AddAssign,
        time::{Duration, Instant},
    },
};

#[derive(Default)]
pub struct GeyserPluginNotifyAtSnapshotRestoreStats {
    pub notified_accounts: usize,
    pub elapsed_notifying: Duration,
    pub total_pure_notify: Duration,
}

impl GeyserPluginNotifyAtSnapshotRestoreStats {
    pub fn report(&self) {
        datapoint_info!(
            "accountsdb_plugin_notify_account_restore_from_snapshot_summary",
            ("notified_accounts", self.notified_accounts, i64),
            (
                "elapsed_notifying_us",
                self.elapsed_notifying.as_micros(),
                i64
            ),
            (
                "total_pure_notify_us",
                self.total_pure_notify.as_micros(),
                i64
            ),
        );
    }
}

impl AddAssign for GeyserPluginNotifyAtSnapshotRestoreStats {
    fn add_assign(&mut self, other: Self) {
        self.notified_accounts += other.notified_accounts;
        self.elapsed_notifying += other.elapsed_notifying;
        self.total_pure_notify += other.total_pure_notify;
    }
}

/// Determines if an account can be mutated by a program during transaction execution.
/// This implements Solana's runtime rules for account mutability:
/// - Sysvars cannot be mutated (checked by base58 prefix)
/// - System program can modify any account's lamports
/// - Other programs can only modify accounts they own
fn can_runtime_mutate_account(invoked_program_id: &Pubkey, account_owner: &Pubkey) -> bool {
    if sysvar_program::check_id(account_owner) {
        return false;
    }

    if system_program::check_id(invoked_program_id) {
        return true;
    }

    account_owner == invoked_program_id
}

/// Determines whether an account should be notified to Geyser plugins.
/// Returns true if the account can actually be mutated by the transaction.
///
/// Optimized to check account mutability with early exits:
/// - Fee payer (index 0) is always mutable
/// - Only writable accounts are checked
/// - Top-level instructions that touch the account are examined
/// - Inner instructions (CPIs) that touch the account are also examined
/// - Ownership rules determine if program can mutate the account
fn should_notify_account_to_geyser(
    txn: &Option<&SanitizedTransaction>,
    account: &AccountSharedData,
    pubkey: &Pubkey,
    inner_instructions: &Option<&InnerInstructionsList>,
) -> bool {
    let Some(txn) = txn else {
        return true;
    };

    let message = txn.message();
    let account_keys = message.account_keys();

    let account_index = account_keys.iter().position(|key| key == pubkey);
    let Some(account_index) = account_index else {
        // Account not in transaction - shouldn't happen, but notify to be safe
        return true;
    };

    if !message.is_writable(account_index) {
        return false;
    }

    if account_index == 0 {
        return true;
    }

    let Some(inner_instructions) = inner_instructions else {
        return true;
    };

    // Check top-level instructions
    for (program_id, instruction) in message.program_instructions_iter() {
        let touches_account = instruction
            .accounts
            .iter()
            .any(|&idx| idx as usize == account_index);

        if !touches_account {
            continue;
        }

        if can_runtime_mutate_account(program_id, account.owner()) {
            return true;
        }
    }

    // Check inner instructions (CPIs)
    for instruction_list in inner_instructions.iter() {
        for ix in instruction_list {
            let touches_account = ix
                .instruction
                .accounts
                .iter()
                .any(|&idx| idx as usize == account_index);

            if !touches_account {
                continue;
            }

            let prog_idx = ix.instruction.program_id_index as usize;
            if let Some(program_id) = account_keys.get(prog_idx) {
                if can_runtime_mutate_account(program_id, account.owner()) {
                    return true;
                }
            }
        }
    }

    false
}

impl AccountsDb {
    /// Notify the plugins of account data when AccountsDb is restored from a snapshot.
    ///
    /// Since accounts may have multiple versions in different slots, plugins must handle
    /// deduplication by inspected the slot and write version of each account notification.
    pub fn notify_account_restore_from_snapshot(&self) {
        let Some(accounts_update_notifier) = &self.accounts_update_notifier else {
            return;
        };

        let mut notify_stats = GeyserPluginNotifyAtSnapshotRestoreStats::default();
        if accounts_update_notifier.snapshot_notifications_enabled() {
            let mut storages = self.storage.all_storages();
            storages.sort_unstable_by_key(|storage| Reverse(storage.slot));
            let mut reader = append_vec::new_scan_accounts_reader();
            storages
                .iter()
                .map(|storage| {
                    Self::notify_accounts_in_storage(
                        accounts_update_notifier.as_ref(),
                        &mut reader,
                        storage,
                    )
                })
                .for_each(|stats| notify_stats += stats);
        }

        accounts_update_notifier.notify_end_of_restore_from_snapshot();
        notify_stats.report();
    }

    pub fn notify_account_at_accounts_update(
        &self,
        slot: Slot,
        account: &AccountSharedData,
        txn: &Option<&SanitizedTransaction>,
        pubkey: &Pubkey,
        write_version: u64,
        inner_instructions: &Option<&InnerInstructionsList>,
    ) {
        if let Some(accounts_update_notifier) = &self.accounts_update_notifier {
            // Filter accounts based on whether they can actually be mutated by the transaction
            if should_notify_account_to_geyser(txn, account, pubkey, inner_instructions) {
                accounts_update_notifier.notify_account_update(
                    slot,
                    account,
                    txn,
                    pubkey,
                    write_version,
                );
            }
        }
    }

    fn notify_accounts_in_storage<'a>(
        notifier: &dyn AccountsUpdateNotifierInterface,
        reader: &mut impl RequiredLenBufFileRead<'a>,
        storage: &'a AccountStorageEntry,
    ) -> GeyserPluginNotifyAtSnapshotRestoreStats {
        let mut pure_notify_time = Duration::ZERO;
        let mut i = 0;
        let notifying_start = Instant::now();
        storage
            .accounts
            .scan_accounts_for_geyser(reader, |account| {
                i += 1;
                // later entries in the same slot are more recent and override earlier accounts for the same pubkey
                // We can pass an incrementing number here for write_version in the future, if the storage does not have a write_version.
                // As long as all accounts for this slot are in 1 append vec that can be iterated oldest to newest.
                let (_, notify_dur) = meas_dur!(notifier.notify_account_restore_from_snapshot(
                    storage.slot(),
                    i as u64,
                    &account
                ));
                pure_notify_time += notify_dur;
            })
            .expect("must scan accounts storage");
        let notifying_time = notifying_start.elapsed();

        GeyserPluginNotifyAtSnapshotRestoreStats {
            notified_accounts: i,
            elapsed_notifying: notifying_time,
            total_pure_notify: pure_notify_time,
        }
    }
}

#[cfg(test)]
pub mod tests {
    use {
        super::*,
        crate::accounts_update_notifier_interface::{
            AccountForGeyser, AccountsUpdateNotifier, AccountsUpdateNotifierInterface,
        },
        dashmap::DashMap,
        std::sync::{
            atomic::{AtomicBool, Ordering},
            Arc,
        },
    };

    impl AccountsDb {
        pub fn set_geyser_plugin_notifier(&mut self, notifier: Option<AccountsUpdateNotifier>) {
            self.accounts_update_notifier = notifier;
        }
    }

    #[derive(Debug, Default)]
    struct GeyserTestPlugin {
        pub accounts_notified: DashMap<Pubkey, Vec<(Slot, u64, AccountSharedData)>>,
        pub is_startup_done: AtomicBool,
    }

    impl AccountsUpdateNotifierInterface for GeyserTestPlugin {
        fn snapshot_notifications_enabled(&self) -> bool {
            true
        }

        /// Notified when an account is updated at runtime, due to transaction activities
        fn notify_account_update(
            &self,
            slot: Slot,
            account: &AccountSharedData,
            _txn: &Option<&SanitizedTransaction>,
            pubkey: &Pubkey,
            write_version: u64,
        ) {
            self.accounts_notified.entry(*pubkey).or_default().push((
                slot,
                write_version,
                account.clone(),
            ));
        }

        /// Notified when the AccountsDb is initialized at start when restored
        /// from a snapshot.
        fn notify_account_restore_from_snapshot(
            &self,
            slot: Slot,
            write_version: u64,
            account: &AccountForGeyser<'_>,
        ) {
            self.accounts_notified
                .entry(*account.pubkey)
                .or_default()
                .push((slot, write_version, account.to_account_shared_data()));
        }

        fn notify_end_of_restore_from_snapshot(&self) {
            self.is_startup_done.store(true, Ordering::Relaxed);
        }
    }

    #[test]
    fn test_notify_account_restore_from_snapshot() {
        let mut accounts = AccountsDb::new_single_for_tests();
        let key1 = Pubkey::new_unique();
        let key2 = Pubkey::new_unique();
        let account = AccountSharedData::new(1, 0, &Pubkey::default());

        // Account with key1 is updated twice in two different slots, should get notified twice
        // Need to add root and flush write cache for each slot to ensure accounts are written
        // to correct slots. Cache flush can skip writes if accounts have already been written to
        // a newer slot
        accounts.store_for_tests((0, [(&key1, &account)].as_slice()));
        accounts.add_root_and_flush_write_cache(0);
        accounts.store_for_tests((1, [(&key1, &account)].as_slice()));
        accounts.add_root_and_flush_write_cache(1);

        // Account with key2 is updated in a single slot, should get notified once
        accounts.store_for_tests((2, [(&key2, &account)].as_slice()));
        accounts.add_root_and_flush_write_cache(2);

        // Do the notification
        let notifier = GeyserTestPlugin::default();
        let notifier = Arc::new(notifier);
        accounts.set_geyser_plugin_notifier(Some(notifier.clone()));
        accounts.notify_account_restore_from_snapshot();

        // Ensure key1 was notified twice in different slots
        {
            let notified_key1 = notifier.accounts_notified.get(&key1).unwrap();
            assert_eq!(notified_key1.len(), 2);
            let (slot, write_version, _account) = &notified_key1[0];
            assert_eq!(*slot, 1);
            assert_eq!(*write_version, 1);
            let (slot, write_version, _account) = &notified_key1[1];
            assert_eq!(*slot, 0);
            assert_eq!(*write_version, 1);
        }

        // Ensure key2 was notified once
        {
            let notified_key2 = notifier.accounts_notified.get(&key2).unwrap();
            assert_eq!(notified_key2.len(), 1);
            let (slot, write_version, _account) = &notified_key2[0];
            assert_eq!(*slot, 2);
            assert_eq!(*write_version, 1);
        }

        // Ensure we were notified that startup is done
        assert!(notifier.is_startup_done.load(Ordering::Relaxed));
    }

    #[test]
    fn test_notify_account_at_accounts_update() {
        let mut accounts = AccountsDb::new_single_for_tests();

        let notifier = GeyserTestPlugin::default();

        let notifier = Arc::new(notifier);
        accounts.set_geyser_plugin_notifier(Some(notifier.clone()));

        // Account with key1 is updated twice in two different slots -- should only get notified twice.
        // Account with key2 is updated slot0, should get notified once
        // Account with key3 is updated in slot1, should get notified once
        let key1 = solana_pubkey::new_rand();
        let account1_lamports1: u64 = 1;
        let account1 =
            AccountSharedData::new(account1_lamports1, 1, AccountSharedData::default().owner());
        let slot0 = 0;
        accounts.store_for_tests((slot0, &[(&key1, &account1)][..]));

        let key2 = solana_pubkey::new_rand();
        let account2_lamports: u64 = 200;
        let account2 =
            AccountSharedData::new(account2_lamports, 1, AccountSharedData::default().owner());
        accounts.store_for_tests((slot0, &[(&key2, &account2)][..]));

        let account1_lamports2 = 2;
        let slot1 = 1;
        let account1 = AccountSharedData::new(account1_lamports2, 1, account1.owner());
        accounts.store_for_tests((slot1, &[(&key1, &account1)][..]));

        let key3 = solana_pubkey::new_rand();
        let account3_lamports: u64 = 300;
        let account3 =
            AccountSharedData::new(account3_lamports, 1, AccountSharedData::default().owner());
        accounts.store_for_tests((slot1, &[(&key3, &account3)][..]));

        assert_eq!(notifier.accounts_notified.get(&key1).unwrap().len(), 2);
        assert_eq!(
            notifier.accounts_notified.get(&key1).unwrap()[0]
                .2
                .lamports(),
            account1_lamports1
        );
        assert_eq!(notifier.accounts_notified.get(&key1).unwrap()[0].0, slot0);
        assert_eq!(
            notifier.accounts_notified.get(&key1).unwrap()[1]
                .2
                .lamports(),
            account1_lamports2
        );
        assert_eq!(notifier.accounts_notified.get(&key1).unwrap()[1].0, slot1);

        assert_eq!(notifier.accounts_notified.get(&key2).unwrap().len(), 1);
        assert_eq!(
            notifier.accounts_notified.get(&key2).unwrap()[0]
                .2
                .lamports(),
            account2_lamports
        );
        assert_eq!(notifier.accounts_notified.get(&key2).unwrap()[0].0, slot0);
        assert_eq!(notifier.accounts_notified.get(&key3).unwrap().len(), 1);
        assert_eq!(
            notifier.accounts_notified.get(&key3).unwrap()[0]
                .2
                .lamports(),
            account3_lamports
        );
        assert_eq!(notifier.accounts_notified.get(&key3).unwrap()[0].0, slot1);
    }
}
