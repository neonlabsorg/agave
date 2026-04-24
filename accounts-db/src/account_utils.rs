// Functions here are wired up in subsequent patches (accounts_db cleanup,
// is_loadable predicate, storable_accounts iteration, svm account_loader, rpc
// visibility). Silence dead-code diagnostics until the full F8 surface is
// applied.
#![allow(dead_code)]

use {
    solana_account::ReadableAccount, solana_clock::Epoch, solana_pubkey::Pubkey,
    solana_system_interface::program as system_program,
};

pub(crate) fn is_default_account_meta(
    lamports: u64,
    data_len: usize,
    owner: &Pubkey,
    executable: bool,
    rent_epoch: Epoch,
) -> bool {
    lamports == 0
        && data_len == 0
        && !executable
        && rent_epoch == Epoch::default()
        && owner == &Pubkey::default()
}

pub(crate) fn is_default_account(account: &impl ReadableAccount) -> bool {
    is_default_account_meta(
        account.lamports(),
        account.data().len(),
        account.owner(),
        account.executable(),
        account.rent_epoch(),
    )
}

/// Accounts that can be purged by zero-lamport clean.
///
/// In this fork, in addition to classic default tombstones, we also treat
/// `owner = system_program`, `lamports = 0`, `data_len = 0` as cleanable.
pub(crate) fn is_cleanable_zero_lamport_account_meta(
    lamports: u64,
    data_len: usize,
    owner: &Pubkey,
    executable: bool,
    rent_epoch: Epoch,
) -> bool {
    is_default_account_meta(lamports, data_len, owner, executable, rent_epoch)
        || (lamports == 0 && data_len == 0 && owner == &system_program::id())
}

pub(crate) fn is_cleanable_zero_lamport_account(account: &impl ReadableAccount) -> bool {
    is_cleanable_zero_lamport_account_meta(
        account.lamports(),
        account.data().len(),
        account.owner(),
        account.executable(),
        account.rent_epoch(),
    )
}
