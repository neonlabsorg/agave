//! SBF program exercising the F10 subaccount syscall surface end-to-end.
//!
//! Instruction layout (input bytes after the discriminator):
//!   data[0]      = discriminator
//!   data[1..]    = payload bytes (interpretation depends on the discriminator)
//!
//! Discriminators:
//!   0 — create + load + write payload + unload (happy path)
//!   1 — load existing subaccount + overwrite with payload + unload
//!   2 — create + create again (second create must fail)
//!   3 — create + load + unload + unload again (second unload must fail)
//!   4 — create + load + write initial u64 + self-CPI(disc=5) +
//!       verify increment + unload (CPI mutation, slot stays loaded across CPI)
//!   5 — load + read u64 + write u64+1 + unload (increment, invoked via CPI)
//!
//! Accounts:
//!   [0] payer / base seed (signer, writable, system-program-owned)
//!   [1] system_program
//!   [2] this program (readonly) — required for the self-CPI in disc=4
//!
//! Seeds are `[payer_key, b"test-sub"]`; the subaccount inherits its
//! writable bit from the payer (the base seed account), and is funded by
//! `sol_create_subaccount` via a system_program::Transfer CPI from the payer.

#![allow(unexpected_cfgs)]

use {
    solana_account_info::AccountInfo,
    solana_cpi::invoke_signed,
    solana_instruction::{AccountMeta, Instruction},
    solana_program_entrypoint::SUCCESS,
    solana_program_error::{ProgramError, ProgramResult},
    solana_pubkey::Pubkey,
};

const SEED_TAG: &[u8] = b"test-sub";
const FUNDING_LAMPORTS: u64 = 2_000_000;
const COUNTER_BYTES: u64 = 8; // size of the u64 counter stored in the subaccount
const SLOT_HEADER_SIZE: u64 = 88;
const SLOT_HEADER_OFFSET_KEY: u64 = 8;
const SLOT_HEADER_OFFSET_OWNER: u64 = 40;
const SLOT_HEADER_OFFSET_LAMPORTS: u64 = 72;

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
    fn sol_load_subaccount_c(
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
    program_id: &Pubkey,
    accounts: &[AccountInfo],
    instruction_data: &[u8],
) -> ProgramResult {
    let discriminator = *instruction_data
        .first()
        .ok_or(ProgramError::InvalidInstructionData)?;
    let payload = &instruction_data[1..];

    match discriminator {
        0 => create_load_write_unload(accounts, payload),
        1 => load_overwrite_unload(accounts, payload),
        2 => create_twice(accounts, payload),
        3 => unload_twice(accounts, payload),
        4 => create_write_cpi_increment(program_id, accounts, payload),
        5 => load_increment_unload(accounts),
        _ => Err(ProgramError::InvalidInstructionData),
    }
}

fn seeds_from_payer(payer_key: &Pubkey) -> [&[u8]; 2] {
    [payer_key.as_ref(), SEED_TAG]
}

fn create_load_write_unload(accounts: &[AccountInfo], payload: &[u8]) -> ProgramResult {
    let payer = accounts.first().ok_or(ProgramError::NotEnoughAccountKeys)?;
    let payer_key: Pubkey = *payer.key;
    let seeds = seeds_from_payer(&payer_key);

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

    let (header_addr, _view_addr) = load_rust(&seeds)?;
    write_data(header_addr, payload);
    unload(header_addr)
}

fn load_overwrite_unload(accounts: &[AccountInfo], payload: &[u8]) -> ProgramResult {
    let payer = accounts.first().ok_or(ProgramError::NotEnoughAccountKeys)?;
    let payer_key: Pubkey = *payer.key;
    let seeds = seeds_from_payer(&payer_key);

    let (header_addr, _view_addr) = load_rust(&seeds)?;
    write_data(header_addr, payload);
    unload(header_addr)
}

fn create_twice(accounts: &[AccountInfo], payload: &[u8]) -> ProgramResult {
    let payer = accounts.first().ok_or(ProgramError::NotEnoughAccountKeys)?;
    let payer_key: Pubkey = *payer.key;
    let seeds = seeds_from_payer(&payer_key);

    let space = payload.len() as u64;
    let r1 = unsafe {
        sol_create_subaccount(
            &payer_key as *const Pubkey as *const u8,
            seeds.as_ptr() as *const u8,
            seeds.len() as u64,
            space,
            FUNDING_LAMPORTS,
        )
    };
    if r1 != SUCCESS {
        return Err(ProgramError::Custom(0x20));
    }
    let r2 = unsafe {
        sol_create_subaccount(
            &payer_key as *const Pubkey as *const u8,
            seeds.as_ptr() as *const u8,
            seeds.len() as u64,
            space,
            FUNDING_LAMPORTS,
        )
    };
    if r2 == SUCCESS {
        return Err(ProgramError::Custom(0x21));
    }
    Err(ProgramError::from(r2))
}

fn unload_twice(accounts: &[AccountInfo], payload: &[u8]) -> ProgramResult {
    let payer = accounts.first().ok_or(ProgramError::NotEnoughAccountKeys)?;
    let payer_key: Pubkey = *payer.key;
    let seeds = seeds_from_payer(&payer_key);

    let space = payload.len() as u64;
    let r1 = unsafe {
        sol_create_subaccount(
            &payer_key as *const Pubkey as *const u8,
            seeds.as_ptr() as *const u8,
            seeds.len() as u64,
            space,
            FUNDING_LAMPORTS,
        )
    };
    if r1 != SUCCESS {
        return Err(ProgramError::Custom(0x30));
    }
    let (header_addr, _view_addr) = load_rust(&seeds)?;
    let u1 = unsafe { sol_unload_subaccount(header_addr) };
    if u1 != SUCCESS {
        return Err(ProgramError::Custom(0x31));
    }
    let u2 = unsafe { sol_unload_subaccount(header_addr) };
    if u2 == SUCCESS {
        return Err(ProgramError::Custom(0x32));
    }
    Err(ProgramError::from(u2))
}

/// Outer half of the CPI-increment scenario (disc=4).
///
/// 1. Create subaccount sized for a single u64 counter.
/// 2. Load (C ABI) + write `initial_value` (from `payload[0..8]`).
///    Slot stays loaded across the CPI.
/// 3. Stamp a valid `SolAccountInfo` at the slot's view buffer — the
///    runtime's `translate_subaccount_slots` verifies pointers there
///    against `slot.caller_account_metadata` at CPI entry.
/// 4. Self-CPI to disc=5 (the inner increment handler). Same program_id,
///    same seeds ⇒ same PDA. Inner loads the same subaccount in its own
///    frame, reads, increments, writes back, unloads.
/// 5. After the CPI returns, read the counter directly from the
///    direct-mapped data region (the outer's slot is still loaded and
///    points at the same host `AccountSharedData`) and verify it equals
///    `initial_value + 1`.
/// 6. Unload.
fn create_write_cpi_increment(
    program_id: &Pubkey,
    accounts: &[AccountInfo],
    payload: &[u8],
) -> ProgramResult {
    let payer = accounts.first().ok_or(ProgramError::NotEnoughAccountKeys)?;
    let payer_key: Pubkey = *payer.key;
    let seeds = seeds_from_payer(&payer_key);

    if payload.len() != COUNTER_BYTES as usize {
        return Err(ProgramError::InvalidInstructionData);
    }
    let mut initial_bytes = [0u8; 8];
    initial_bytes.copy_from_slice(payload);
    let initial_value = u64::from_le_bytes(initial_bytes);

    let r = unsafe {
        sol_create_subaccount(
            &payer_key as *const Pubkey as *const u8,
            seeds.as_ptr() as *const u8,
            seeds.len() as u64,
            COUNTER_BYTES,
            FUNDING_LAMPORTS,
        )
    };
    if r != SUCCESS {
        return Err(ProgramError::Custom(0x40));
    }

    let (header_addr, view_addr) = load_c(&seeds)?;
    populate_sol_account_info(view_addr, header_addr, COUNTER_BYTES);
    write_data(header_addr, &initial_bytes);

    cpi_increment(program_id, payer)?;

    // Read the counter back from the direct-mapped data region. The slot
    // is still loaded — the outer never called unload — so this is the
    // exact same host bytes the inner just wrote.
    let data_ptr = (header_addr + SLOT_HEADER_SIZE) as *const u8;
    let observed =
        unsafe { core::ptr::read_unaligned(data_ptr as *const u64) };
    let expected = initial_value.wrapping_add(1);
    if observed != expected {
        return Err(ProgramError::Custom(0x42));
    }

    unload(header_addr)
}

/// Inner half of the CPI-increment scenario (disc=5).
///
/// Invoked via CPI from disc=4. The outer's slot remains loaded in the
/// outer's frame, but the inner has its own frame with its own fresh
/// `subaccount_slots`, so `sol_load_subaccount` here allocates a new
/// slot pointing at the same shared `AccountSharedData` (direct-mapped).
/// Reads u64, increments, writes back, unloads. Inner's unload only
/// frees its OWN slot — the outer's slot is unaffected.
fn load_increment_unload(accounts: &[AccountInfo]) -> ProgramResult {
    let payer = accounts.first().ok_or(ProgramError::NotEnoughAccountKeys)?;
    let payer_key: Pubkey = *payer.key;
    let seeds = seeds_from_payer(&payer_key);

    let (header_addr, _view_addr) = load_rust(&seeds)?;

    let data_ptr = (header_addr + SLOT_HEADER_SIZE) as *mut u8;
    let current = unsafe { core::ptr::read_unaligned(data_ptr as *const u64) };
    let next = current.wrapping_add(1);
    unsafe { core::ptr::write_unaligned(data_ptr as *mut u64, next) };

    unload(header_addr)
}

fn load_rust(seeds: &[&[u8]]) -> Result<(u64, u64), ProgramError> {
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
        return Err(ProgramError::from(result));
    }
    Ok((header_addr, view_addr))
}

fn load_c(seeds: &[&[u8]]) -> Result<(u64, u64), ProgramError> {
    let mut view_addr: u64 = 0;
    let mut header_addr: u64 = 0;
    let result = unsafe {
        sol_load_subaccount_c(
            seeds.as_ptr() as *const u8,
            seeds.len() as u64,
            &mut view_addr as *mut u64,
            &mut header_addr as *mut u64,
            0,
        )
    };
    if result != SUCCESS {
        return Err(ProgramError::from(result));
    }
    Ok((header_addr, view_addr))
}

fn unload(header_addr: u64) -> ProgramResult {
    let result = unsafe { sol_unload_subaccount(header_addr) };
    if result != SUCCESS {
        return Err(ProgramError::from(result));
    }
    Ok(())
}

fn write_data(header_addr: u64, payload: &[u8]) {
    if payload.is_empty() {
        return;
    }
    let data_ptr = (header_addr + SLOT_HEADER_SIZE) as *mut u8;
    unsafe {
        core::ptr::copy_nonoverlapping(payload.as_ptr(), data_ptr, payload.len());
    }
}

/// Writes the runtime's `SolAccountInfo` layout (program-runtime/src/cpi.rs:93)
/// at `view_addr`. Field offsets are pinned by hand so the layout matches
/// what `translate_subaccount_slots::<SolAccountInfo>` decodes:
///   offset  0: key_addr     (u64) → vm_header_addr + SLOT_HEADER_OFFSET_KEY
///   offset  8: lamports_addr (u64) → vm_header_addr + SLOT_HEADER_OFFSET_LAMPORTS
///   offset 16: data_len      (u64)
///   offset 24: data_addr     (u64) → vm_header_addr + SLOT_HEADER_SIZE
///   offset 32: owner_addr    (u64) → vm_header_addr + SLOT_HEADER_OFFSET_OWNER
///   offset 40: rent_epoch    (u64)
///   offset 48: is_signer / is_writable / executable (3× u8)
fn populate_sol_account_info(view_addr: u64, header_addr: u64, data_len: u64) {
    unsafe {
        let p = view_addr as *mut u8;
        core::ptr::write(p.add(0) as *mut u64, header_addr + SLOT_HEADER_OFFSET_KEY);
        core::ptr::write(p.add(8) as *mut u64, header_addr + SLOT_HEADER_OFFSET_LAMPORTS);
        core::ptr::write(p.add(16) as *mut u64, data_len);
        core::ptr::write(p.add(24) as *mut u64, header_addr + SLOT_HEADER_SIZE);
        core::ptr::write(p.add(32) as *mut u64, header_addr + SLOT_HEADER_OFFSET_OWNER);
        core::ptr::write(p.add(40) as *mut u64, 0u64);
        core::ptr::write(p.add(48), 0u8); // is_signer
        core::ptr::write(p.add(49), 1u8); // is_writable
        core::ptr::write(p.add(50), 0u8); // executable
    }
}

/// Issues the self-CPI into discriminator 5. The inner call needs only
/// the payer in its `accounts[..]` for seed derivation. The runtime's
/// `translate_subaccount_slots` decodes our loaded slot per
/// `slot.account_view_kind` — we loaded via `sol_load_subaccount_c` and
/// stamped a `SolAccountInfo`, so the slot is decoded as C even though
/// `solana_cpi::invoke_signed` goes through `sol_invoke_signed_rust`.
fn cpi_increment(callee_program_id: &Pubkey, payer: &AccountInfo) -> ProgramResult {
    let inner_ix = Instruction {
        program_id: *callee_program_id,
        accounts: vec![AccountMeta::new(*payer.key, true)],
        data: vec![5u8],
    };
    invoke_signed(&inner_ix, &[payer.clone()], &[]).map_err(|_| ProgramError::Custom(0x41))
}
