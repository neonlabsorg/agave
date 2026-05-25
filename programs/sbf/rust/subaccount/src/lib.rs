//! SBF program exercising the F10 subaccount syscall surface end-to-end.
//!
//! Instruction layout (input bytes after the discriminator):
//!   data[0]      = discriminator
//!   data[1..]    = payload bytes to write into the subaccount data region
//!
//! Discriminator 0 — create + load + write + unload roundtrip.
//!
//! Accounts:
//!   [0] payer / base seed (signer, writable, system-program-owned)
//!   [1] system_program
//!
//! Seeds are `[payer_key, b"test-sub"]`; the subaccount inherits its
//! writable bit from the payer (the base seed account), and is funded by
//! `sol_create_subaccount` via a system_program::Transfer CPI from the payer.

#![allow(unexpected_cfgs)]

use {
    solana_account_info::AccountInfo,
    solana_program_entrypoint::SUCCESS,
    solana_program_error::{ProgramError, ProgramResult},
    solana_pubkey::Pubkey,
};

const SEED_TAG: &[u8] = b"test-sub";
const FUNDING_LAMPORTS: u64 = 2_000_000;
const SLOT_HEADER_SIZE: u64 = 88;

extern "C" {
    fn sol_create_subaccount(
        payer_pubkey_addr: *const u8,
        seeds_addr: *const u8,
        seeds_len: u64,
        space: u64,
        lamports: u64,
    ) -> u64;
    fn sol_load_subaccount_rust(
        seeds_addr: *const u8,
        seeds_len: u64,
        out_account_view_addr: *mut u64,
        out_header_addr: *mut u64,
        _arg5: u64,
    ) -> u64;
    fn sol_unload_subaccount(vm_header_addr: u64) -> u64;
}

solana_program_entrypoint::entrypoint!(process_instruction);

fn process_instruction(
    _program_id: &Pubkey,
    accounts: &[AccountInfo],
    instruction_data: &[u8],
) -> ProgramResult {
    let discriminator = *instruction_data
        .first()
        .ok_or(ProgramError::InvalidInstructionData)?;
    let payload = &instruction_data[1..];

    match discriminator {
        0 => create_load_write_unload(accounts, payload),
        _ => Err(ProgramError::InvalidInstructionData),
    }
}

fn create_load_write_unload(accounts: &[AccountInfo], payload: &[u8]) -> ProgramResult {
    let payer = accounts.first().ok_or(ProgramError::NotEnoughAccountKeys)?;
    let payer_key: Pubkey = *payer.key;

    // The first seed must be a 32-byte pubkey present in the transaction —
    // the runtime uses it to inherit the writable bit. We use the payer
    // both as funding source and as the base seed.
    let payer_key_bytes: &[u8] = payer_key.as_ref();
    let seeds: [&[u8]; 2] = [payer_key_bytes, SEED_TAG];

    let space = payload.len() as u64;
    let result = unsafe {
        sol_create_subaccount(
            &payer_key as *const Pubkey as *const u8,
            seeds.as_ptr() as *const u8,
            seeds.len() as u64,
            space,
            FUNDING_LAMPORTS,
        )
    };
    if result != SUCCESS {
        return Err(ProgramError::Custom(0x10));
    }

    let mut view_addr: u64 = 0;
    let mut header_addr: u64 = 0;
    let result = unsafe {
        sol_load_subaccount_rust(
            seeds.as_ptr() as *const u8,
            seeds.len() as u64,
            &mut view_addr as *mut u64,
            &mut header_addr as *mut u64,
            0,
        )
    };
    if result != SUCCESS {
        return Err(ProgramError::Custom(0x11));
    }

    // The slot's data region is direct-mapped onto the subaccount's
    // `AccountSharedData::data`, sitting at `header_addr + SLOT_HEADER_SIZE`
    // (vm_data_addr in the slot layout). Writes here land in the host
    // storage immediately; no CPI sync needed.
    if !payload.is_empty() {
        let data_ptr = (header_addr + SLOT_HEADER_SIZE) as *mut u8;
        unsafe {
            core::ptr::copy_nonoverlapping(payload.as_ptr(), data_ptr, payload.len());
        }
    }

    let result = unsafe { sol_unload_subaccount(header_addr) };
    if result != SUCCESS {
        return Err(ProgramError::Custom(0x12));
    }

    Ok(())
}
