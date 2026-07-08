#![cfg(feature = "agave-unstable-api")]
//! Data shared between program runtime and built-in programs as well as SBF programs.
#![deny(clippy::indexing_slicing)]
#![cfg_attr(docsrs, feature(doc_auto_cfg))]

pub mod instruction;
pub mod instruction_accounts;
pub mod transaction_accounts;
mod vm_addresses;
pub mod vm_slice;

pub mod transaction;

use solana_pubkey::Pubkey;

pub const MAX_ACCOUNTS_PER_TRANSACTION: usize = 256;
// This is one less than MAX_ACCOUNTS_PER_TRANSACTION because
// one index is used as NON_DUP_MARKER in ABI v0 and v1.
pub const MAX_ACCOUNTS_PER_INSTRUCTION: usize = 255;
// F10: programs can load more subaccounts from the transaction context as
// needed, but these subaccounts cannot be passed in as instruction accounts.
// This limit prevents abuse of the mechanism to load an unbounded number of
// subaccounts.
pub const MAX_SUBACCOUNTS_PER_TRANSACTION: usize = 2048;
pub const MAX_INSTRUCTION_DATA_LEN: usize = 10 * 1024;
pub const MAX_ACCOUNT_DATA_LEN: u64 = 10 * 1024 * 1024;
// Note: With virtual_address_space_adjustments programs can grow accounts
// faster than they intend to, because the AccessViolationHandler might grow
// an account up to MAX_ACCOUNT_DATA_GROWTH_PER_INSTRUCTION at once.
pub const MAX_ACCOUNT_DATA_GROWTH_PER_TRANSACTION: i64 = MAX_ACCOUNT_DATA_LEN as i64 * 2;
pub const MAX_ACCOUNT_DATA_GROWTH_PER_INSTRUCTION: usize = 10 * 1_024;
// Maximum cross-program invocation and instructions per transaction
pub const MAX_INSTRUCTION_TRACE_LENGTH: usize = 64;

#[cfg(test)]
static_assertions::const_assert_eq!(
    MAX_ACCOUNTS_PER_INSTRUCTION,
    solana_program_entrypoint::NON_DUP_MARKER as usize,
);
#[cfg(test)]
static_assertions::const_assert_eq!(
    MAX_ACCOUNT_DATA_LEN,
    solana_system_interface::MAX_PERMITTED_DATA_LENGTH,
);
#[cfg(test)]
static_assertions::const_assert_eq!(
    MAX_ACCOUNT_DATA_GROWTH_PER_TRANSACTION,
    solana_system_interface::MAX_PERMITTED_ACCOUNTS_DATA_ALLOCATIONS_PER_TRANSACTION,
);
#[cfg(test)]
static_assertions::const_assert_eq!(
    MAX_ACCOUNT_DATA_GROWTH_PER_INSTRUCTION,
    solana_account_info::MAX_PERMITTED_DATA_INCREASE,
);

/// Index of an account inside of the transaction or an instruction.
pub type IndexOfAccount = u16;

/// F10: high bit of an `IndexOfAccount` flags a subaccount entry (see
/// `InstructionAccount::new_subaccount`). Subaccount lanes are stored in a
/// parallel Vec in `TransactionAccounts`, and instruction account indices with
/// this marker set are de-referenced via the subaccount lane instead of the
/// main account lane.
pub const SUBACCOUNT_MARKER: u16 = 1 << 15;

/// F10: derives the on-chain storage address for a subaccount given its
/// owner-facing pubkey (the PDA the program receives from
/// `sol_create_subaccount` / `sol_load_subaccount`). The storage address is
/// the sha256 hash of `[0x01, owner_pubkey]` and names the entry in
/// accounts-db that backs the subaccount across transactions.
///
/// Shared by the runtime (subaccount serializer, end-of-tx persistence in
/// `account_saver` / `account_loader`) and the RPC layer so callers can
/// resolve a subaccount pubkey to its persisted storage entry.
pub fn subaccount_storage_address(pubkey: &Pubkey) -> Pubkey {
    Pubkey::new_from_array(solana_sha256_hasher::hashv(&[&[1u8], pubkey.as_ref()]).to_bytes())
}

/// F10/PRS-310: derives the owner-facing pubkey of a subaccount from its
/// seeds and the owning program id.
///
/// The address is `sha256(seed_0 || seed_1 || ... || program_id || "SubAccount")`.
/// Paired with [`subaccount_storage_address`]: the runtime persists the
/// subaccount under `subaccount_storage_address(create_subaccount_address(..))`.
pub fn create_subaccount_address(
    seeds: &[&[u8]],
    program_id: &Pubkey,
) -> Result<Pubkey, solana_pubkey::PubkeyError> {
    if seeds.len() > solana_pubkey::MAX_SEEDS {
        return Err(solana_pubkey::PubkeyError::MaxSeedLengthExceeded);
    }
    if seeds
        .iter()
        .any(|seed| seed.len() > solana_pubkey::MAX_SEED_LEN)
    {
        return Err(solana_pubkey::PubkeyError::MaxSeedLengthExceeded);
    }
    /// Domain-separation tag mixed into the subaccount-address hash.
    /// Appended after the seeds and the program id so the resulting digest
    /// lives in a hash domain disjoint from regular PDAs (which use
    /// `b"ProgramDerivedAddress"`).
    const SUBACCOUNT_HASH_DOMAIN_TAG: &[u8; 10] = b"SubAccount";
    let mut hasher = solana_sha256_hasher::Hasher::default();
    for seed in seeds {
        hasher.hash(seed);
    }
    hasher.hashv(&[program_id.as_ref(), SUBACCOUNT_HASH_DOMAIN_TAG]);
    Ok(Pubkey::new_from_array(hasher.result().to_bytes()))
}
