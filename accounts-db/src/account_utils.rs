use {
    solana_account::ReadableAccount,
    solana_clock::Epoch,
    solana_pubkey::Pubkey,
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
