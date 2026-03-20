use {
    crate::account_utils::is_cleanable_zero_lamport_account, solana_account::ReadableAccount,
};

/// A trait to see if an account is loadable or not.
pub trait IsLoadable {
    /// Is this account loadable?
    fn is_loadable(&self) -> bool;
}

impl<T: ReadableAccount> IsLoadable for T {
    fn is_loadable(&self) -> bool {
        // Hide accounts that are cleanable by zero-lamport clean from scan/list paths.
        !is_cleanable_zero_lamport_account(self)
    }
}
