use {crate::account_utils::is_default_account, solana_account::ReadableAccount};

/// A trait to see if an account is loadable or not.
pub trait IsLoadable {
    /// Is this account loadable?
    fn is_loadable(&self) -> bool;
}

impl<T: ReadableAccount> IsLoadable for T {
    fn is_loadable(&self) -> bool {
        // Treat only default tombstone accounts as non-loadable.
        !is_default_account(self)
    }
}
