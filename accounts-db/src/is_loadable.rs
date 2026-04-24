use {crate::account_utils::is_cleanable_zero_lamport_account, solana_account::ReadableAccount};

/// A trait to see if an account is loadable or not.
pub trait IsLoadable {
    /// Is this account loadable?
    fn is_loadable(&self) -> bool;
}

impl<T: ReadableAccount> IsLoadable for T {
    fn is_loadable(&self) -> bool {
        // Hide accounts that are cleanable by zero-lamport clean from scan/list paths.
        // Under parasol F8 this covers:
        //  - classic default tombstones (lamports=0, data=[], owner=default, !executable, rent_epoch=0)
        //  - system-program-owned zero-lamport accounts with empty data
        // Valid zero-lamport accounts (non-default metadata) remain loadable.
        !is_cleanable_zero_lamport_account(self)
    }
}
