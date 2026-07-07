//! Solana SVM Rent Calculator.
//!
//! Rent management for SVM.

use {
    solana_clock::Epoch,
    solana_pubkey::Pubkey,
    solana_rent::Rent,
    solana_transaction_context::{IndexOfAccount, transaction::TransactionContext},
    solana_transaction_error::TransactionResult,
};

/// When rent is collected from an exempt account, rent_epoch is set to this
/// value. The idea is to have a fixed, consistent value for rent_epoch for all accounts that do not collect rent.
/// This enables us to get rid of the field completely.
pub const RENT_EXEMPT_RENT_EPOCH: Epoch = Epoch::MAX;

/// Rent state of a Solana account.
#[derive(Debug, PartialEq, Eq)]
pub enum RentState {
    /// account.lamports == 0
    Uninitialized,
    /// 0 < account.lamports < rent-exempt-minimum
    RentPaying {
        lamports: u64,    // account.lamports()
        data_size: usize, // account.data().len()
    },
    /// account.lamports >= rent-exempt-minimum
    RentExempt,
}

/// Check rent state transition for an account in a transaction.
///
/// This method has a default implementation that calls into
/// `check_rent_state_with_account`.
pub fn check_rent_state(
    _pre_rent_state: Option<&RentState>,
    _post_rent_state: Option<&RentState>,
    _transaction_context: &TransactionContext,
    _index: IndexOfAccount,
) -> TransactionResult<()> {
    // F2+F3 (parasol): rent-state transition check neutralized (rent removed)
    Ok(())
}

/// Check rent state transition for an account directly.
///
/// This method has a default implementation that checks whether the
/// transition is allowed and returns an error if it is not. It also
/// verifies that the account is not the incinerator.
pub fn check_rent_state_with_account(
    _pre_rent_state: &RentState,
    _post_rent_state: &RentState,
    _address: &Pubkey,
    _account_index: IndexOfAccount,
) -> TransactionResult<()> {
    // F2+F3 (parasol): rent-state transition check neutralized (rent removed)
    Ok(())
}

/// Determine the rent state of an account.
///
/// This method has a default implementation that treats accounts with zero
/// lamports as uninitialized and uses the implemented `get_rent` to
/// determine whether an account is rent-exempt.
pub fn get_account_rent_state(
    rent: &Rent,
    account_lamports: u64,
    account_size: usize,
) -> RentState {
    if account_lamports == 0 {
        RentState::Uninitialized
    } else if rent.is_exempt(account_lamports, account_size) {
        RentState::RentExempt
    } else {
        RentState::RentPaying {
            data_size: account_size,
            lamports: account_lamports,
        }
    }
}

/// Check whether a transition from the pre_rent_state to the
/// post_rent_state is valid.
///
/// This method has a default implementation that allows transitions from
/// any state to `RentState::Uninitialized` or `RentState::RentExempt`.
/// Pre-state `RentState::RentPaying` can only transition to
/// `RentState::RentPaying` if the data size remains the same and the
/// account is not credited.
pub fn transition_allowed(pre_rent_state: &RentState, post_rent_state: &RentState) -> bool {
    match post_rent_state {
        RentState::Uninitialized | RentState::RentExempt => true,
        RentState::RentPaying {
            data_size: post_data_size,
            lamports: post_lamports,
        } => {
            match pre_rent_state {
                RentState::Uninitialized | RentState::RentExempt => false,
                RentState::RentPaying {
                    data_size: pre_data_size,
                    lamports: pre_lamports,
                } => {
                    // Cannot remain RentPaying if resized or credited.
                    post_data_size == pre_data_size && post_lamports <= pre_lamports
                }
            }
        }
    }
}
