#[cfg(feature = "dev-context-only-utils")]
use qualifier_attr::field_qualifiers;
use {
    crate::{
        account_loader::AccountLoader,
        transaction_processing_callback::TransactionProcessingCallback,
    },
    solana_account::{AccountSharedData, ReadableAccount},
    solana_pubkey::Pubkey,
    solana_svm_transaction::svm_transaction::SVMTransaction,
    solana_transaction_context::{
        subaccount_storage_address, transaction_accounts::KeyedAccountSharedData,
    },
    spl_generic_token::{generic_token, is_known_spl_token_id},
};

// we use internal aliases for clarity, the external type aliases are often confusing
type TxNativeBalances = Vec<u64>;
type TxTokenBalances = Vec<SvmTokenInfo>;
type TxSubaccountKeys = Vec<Pubkey>;
type BatchNativeBalances = Vec<TxNativeBalances>;
type BatchTokenBalances = Vec<TxTokenBalances>;
type BatchSubaccountKeys = Vec<TxSubaccountKeys>;

// to operate cleanly over Option<BalanceCollector> we use a trait impled on the outer and inner type
pub(crate) trait BalanceCollectionRoutines {
    fn collect_pre_balances<CB: TransactionProcessingCallback>(
        &mut self,
        account_loader: &mut AccountLoader<CB>,
        transaction: &impl SVMTransaction,
    );

    /// F10/PRS-314: capture subaccount pre-balances **after execution but
    /// before** `AccountLoader::update_accounts_for_successful_tx` runs, so the
    /// loader cache still returns the pre-execution state for the subaccount
    /// storage address. The lane is keyed by the owner-facing pubkey;
    /// `subaccount_storage_address` is applied here only to read pre-state out
    /// of the loader cache. Extends the tail of the per-tx native_pre vector
    /// with pre-lamports and records the owner pubkeys in `subaccount_keys`.
    /// Subaccount post values are appended later in `collect_post_balances`
    /// directly from the lane.
    fn collect_subaccount_pre_balances<CB: TransactionProcessingCallback>(
        &mut self,
        account_loader: &mut AccountLoader<CB>,
        subaccount_lane: &[KeyedAccountSharedData],
    );

    fn collect_post_balances<CB: TransactionProcessingCallback>(
        &mut self,
        account_loader: &mut AccountLoader<CB>,
        transaction: &impl SVMTransaction,
        subaccount_lane: &[KeyedAccountSharedData],
        unchanged_subaccount_addresses: &[Pubkey],
    );
}

#[derive(Debug, Default)]
#[cfg_attr(
    feature = "dev-context-only-utils",
    field_qualifiers(
        native_pre(pub),
        native_post(pub),
        token_pre(pub),
        token_post(pub),
        subaccount_keys(pub),
        unchanged_subaccount_keys(pub),
    )
)]
pub struct BalanceCollector {
    native_pre: BatchNativeBalances,
    native_post: BatchNativeBalances,
    token_pre: BatchTokenBalances,
    token_post: BatchTokenBalances,
    // F10/PRS-314: per-transaction owner-facing pubkeys of subaccounts *changed*
    // during execution, in the same order as the tail of `native_pre` /
    // `native_post` (positions [account_keys.len()..)). Empty for txs that
    // didn't change any subaccount.
    subaccount_keys: BatchSubaccountKeys,
    // F10/PRS-155: per-transaction owner-facing pubkeys of subaccounts that
    // were accessed but left *unchanged* (read-only reads/loads). These carry
    // no pre/post balances — the receipt lists them by owner address only.
    unchanged_subaccount_keys: BatchSubaccountKeys,
}

impl BalanceCollector {
    // we always provide one vec for every transaction, even if the vecs are empty
    pub(crate) fn new_with_transaction_count(transaction_count: usize) -> Self {
        Self {
            native_pre: Vec::with_capacity(transaction_count),
            native_post: Vec::with_capacity(transaction_count),
            token_pre: Vec::with_capacity(transaction_count),
            token_post: Vec::with_capacity(transaction_count),
            subaccount_keys: Vec::with_capacity(transaction_count),
            unchanged_subaccount_keys: Vec::with_capacity(transaction_count),
        }
    }

    // we use this pattern to prevent anything outside svm mutating BalanceCollector internals
    // with no public constructor, and only private fields, non-svm code can only disassemble the struct
    pub fn into_vecs(
        self,
    ) -> (
        BatchNativeBalances,
        BatchNativeBalances,
        BatchTokenBalances,
        BatchTokenBalances,
        BatchSubaccountKeys,
        BatchSubaccountKeys,
    ) {
        (
            self.native_pre,
            self.native_post,
            self.token_pre,
            self.token_post,
            self.subaccount_keys,
            self.unchanged_subaccount_keys,
        )
    }

    // gather native lamport balances for all accounts
    // and token balances for valid, initialized token accounts with valid, initialized mints
    fn collect_balances<CB: TransactionProcessingCallback>(
        &mut self,
        account_loader: &mut AccountLoader<CB>,
        transaction: &impl SVMTransaction,
    ) -> (TxNativeBalances, TxTokenBalances) {
        let mut native_balances = Vec::with_capacity(transaction.account_keys().len());
        let mut token_balances = vec![];

        let has_token_program = transaction.account_keys().iter().any(is_known_spl_token_id);

        for (index, key) in transaction.account_keys().iter().enumerate() {
            let Some(account) = account_loader.load_account(key) else {
                native_balances.push(0);
                continue;
            };

            native_balances.push(account.lamports());

            if has_token_program
                && !transaction.is_invoked(index)
                && !is_known_spl_token_id(key)
                && is_known_spl_token_id(account.owner())
                && let Some(token_info) =
                    SvmTokenInfo::unpack_token_account(account_loader, &account, index)
            {
                token_balances.push(token_info);
            }
        }

        (native_balances, token_balances)
    }

    pub(crate) fn lengths_match_expected(&self, expected_len: usize) -> bool {
        self.native_pre.len() == expected_len
            && self.native_post.len() == expected_len
            && self.token_pre.len() == expected_len
            && self.token_post.len() == expected_len
            && self.subaccount_keys.len() == expected_len
            && self.unchanged_subaccount_keys.len() == expected_len
    }
}

impl BalanceCollectionRoutines for BalanceCollector {
    fn collect_pre_balances<CB: TransactionProcessingCallback>(
        &mut self,
        account_loader: &mut AccountLoader<CB>,
        transaction: &impl SVMTransaction,
    ) {
        let (native_balances, token_balances) = self.collect_balances(account_loader, transaction);
        self.native_pre.push(native_balances);
        self.token_pre.push(token_balances);
        // The subaccount lane is empty at the pre-balance phase (subaccounts
        // are materialized lazily by `sol_load_subaccount` /
        // `sol_create_subaccount` during execution). The matching tail entries
        // are appended in `collect_subaccount_pre_balances` /
        // `collect_post_balances` below. Seed the per-tx slot here to keep
        // batch lengths aligned.
        self.subaccount_keys.push(Vec::new());
        // Unchanged subaccounts are recorded in `collect_post_balances` (owner
        // keys only); seed the per-tx slot here to keep batch lengths aligned.
        self.unchanged_subaccount_keys.push(Vec::new());
    }

    fn collect_subaccount_pre_balances<CB: TransactionProcessingCallback>(
        &mut self,
        account_loader: &mut AccountLoader<CB>,
        subaccount_lane: &[KeyedAccountSharedData],
    ) {
        if subaccount_lane.is_empty() {
            return;
        }

        let (Some(pre_tail), Some(subaccount_keys_tail)) =
            (self.native_pre.last_mut(), self.subaccount_keys.last_mut())
        else {
            // `collect_pre_balances` seeds both tails for every tx, so a missing
            // tail here means it was skipped — a caller bug. Assert loudly in
            // debug builds, but don't panic the validator hot path in release.
            debug_assert!(
                false,
                "collect_subaccount_pre_balances called without collect_pre_balances"
            );
            return;
        };
        for (owner_pubkey, _) in subaccount_lane.iter() {
            // The lane is keyed by the owner-facing pubkey;
            // `subaccount_storage_address` is the accounts-db addressing of the
            // subaccount, which is what the loader cache and accounts-db both
            // expose. The cache still holds pre-execution lamports here because
            // `update_accounts_for_successful_tx` has not run yet.
            let pre_lamports = account_loader
                .load_account(&subaccount_storage_address(owner_pubkey))
                .map(|account| account.lamports())
                .unwrap_or(0);
            pre_tail.push(pre_lamports);
            // The recipe exposes owner-facing pubkeys verbatim — no
            // transformation — so RPC consumers can map recipe entries back to
            // the addresses programs see.
            subaccount_keys_tail.push(*owner_pubkey);
        }
    }

    fn collect_post_balances<CB: TransactionProcessingCallback>(
        &mut self,
        account_loader: &mut AccountLoader<CB>,
        transaction: &impl SVMTransaction,
        subaccount_lane: &[KeyedAccountSharedData],
        unchanged_subaccount_addresses: &[Pubkey],
    ) {
        let (mut native_balances, token_balances) =
            self.collect_balances(account_loader, transaction);

        // F10/PRS-314: extend the per-tx native_post vector with post-execution
        // lamports for each touched subaccount. The lane already carries
        // post-execution state, so we read directly from it instead of going
        // through the loader cache. Token balances stay aligned with
        // `tx.account_keys()` only — subaccounts are program-private storage,
        // not SPL accounts.
        for (_, post_account) in subaccount_lane.iter() {
            native_balances.push(post_account.lamports());
        }

        self.native_post.push(native_balances);
        self.token_post.push(token_balances);

        // F10/PRS-155: record the read-only (unchanged) subaccounts by owner
        // key only — no balance is appended to native_pre/native_post for
        // them. The per-tx slot was seeded in `collect_pre_balances`.
        if !unchanged_subaccount_addresses.is_empty() {
            if let Some(tail) = self.unchanged_subaccount_keys.last_mut() {
                tail.extend_from_slice(unchanged_subaccount_addresses);
            }
        }
    }
}

impl BalanceCollectionRoutines for Option<BalanceCollector> {
    fn collect_pre_balances<CB: TransactionProcessingCallback>(
        &mut self,
        account_loader: &mut AccountLoader<CB>,
        transaction: &impl SVMTransaction,
    ) {
        if let Some(inner) = self {
            inner.collect_pre_balances(account_loader, transaction)
        }
    }

    fn collect_subaccount_pre_balances<CB: TransactionProcessingCallback>(
        &mut self,
        account_loader: &mut AccountLoader<CB>,
        subaccount_lane: &[KeyedAccountSharedData],
    ) {
        if let Some(inner) = self {
            inner.collect_subaccount_pre_balances(account_loader, subaccount_lane)
        }
    }

    fn collect_post_balances<CB: TransactionProcessingCallback>(
        &mut self,
        account_loader: &mut AccountLoader<CB>,
        transaction: &impl SVMTransaction,
        subaccount_lane: &[KeyedAccountSharedData],
        unchanged_subaccount_addresses: &[Pubkey],
    ) {
        if let Some(inner) = self {
            inner.collect_post_balances(
                account_loader,
                transaction,
                subaccount_lane,
                unchanged_subaccount_addresses,
            )
        }
    }
}

// this contains all the information we can provide to construct TransactionTokenBalance
// that type, in ledger, depends on UiTokenAmount from account-decoder, so we cannot build it here
#[derive(Debug, Clone, PartialEq)]
pub struct SvmTokenInfo {
    pub account_index: u8,
    pub mint: Pubkey,
    pub amount: u64,
    pub owner: Pubkey,
    pub program_id: Pubkey,
    pub decimals: u8,
}

impl SvmTokenInfo {
    fn unpack_token_account<CB: TransactionProcessingCallback>(
        account_loader: &mut AccountLoader<CB>,
        account: &AccountSharedData,
        index: usize,
    ) -> Option<Self> {
        let program_id = *account.owner();
        let generic_token::Account {
            mint,
            owner,
            amount,
        } = generic_token::Account::unpack(account.data(), &program_id)?;

        let mint_account = account_loader.load_account(&mint)?;
        if *mint_account.owner() != program_id {
            return None;
        }

        let generic_token::Mint { decimals, .. } =
            generic_token::Mint::unpack(mint_account.data(), &program_id)?;

        Some(Self {
            account_index: index.try_into().ok()?,
            mint,
            amount,
            owner,
            program_id,
            decimals,
        })
    }
}
