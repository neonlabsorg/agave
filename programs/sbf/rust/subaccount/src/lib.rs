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
//!   9 — read_subaccount: optionally create+write content, then read a
//!       [offset, offset+length) window back via `sol_read_subaccount` and
//!       (on a successful in-range read) verify the bytes match. Exercises
//!       the happy path plus the missing-subaccount and out-of-range
//!       (full/partial) edge cases — see `read_subaccount`.
//!  10 — load_subaccount_snapshot: read-only, base-free, start-of-block load.
//!       Takes the base pubkey from the payload (NOT from the account list, to
//!       prove the base account is not required) and verifies the snapshot data
//!       equals the expected bytes — see `load_snapshot_verify`.
//!  11 — load_subaccount_snapshot + verify owner / lamports / data_len / data
//!       against expected values from the payload — see
//!       `load_subaccount_snapshot_verify`.
//!  12 — load_subaccount_snapshot + attempt to write into the read-only data
//!       region (must fault) — see `load_subaccount_snapshot_cannot_modify`.
//!  13 — load_subaccount_snapshot twice for the same subaccount in one
//!       instruction; the second load must fail — see
//!       `load_subaccount_snapshot_twice`.
//!  14 — load_subaccount_snapshot + unload + reload + unload, proving a freed
//!       snapshot slot can be reused — see `load_subaccount_snapshot_unload_reload`.
//!  15 — load two distinct subaccount snapshots into two slots concurrently and
//!       verify each — see `load_two_subaccount_snapshots`.
//!  16 — writable load + overwrite, then load a snapshot of the SAME subaccount
//!       and verify it still reads the start-of-block value, proving the
//!       snapshot lane is independent of the live subaccount lane within a
//!       single instruction — see `subaccount_snapshot_independent_from_writable_load`.
//!  17 — load_account_snapshot: read-only, base-free, start-of-block load of an
//!       arbitrary account *by pubkey* (not seeds). Verifies the snapshot's
//!       owner / lamports / data_len / data against expected values from the
//!       payload — see `load_account_snapshot_verify`.
//!  18 — load_account_snapshot + attempt to write into the read-only data
//!       region (must fault) — see `load_account_snapshot_cannot_modify`.
//!  19 — load_account_snapshot twice for the same pubkey in one instruction;
//!       the second load must fail — see `load_account_snapshot_twice`.
//!  20 — load_account_snapshot + unload + reload + unload, proving a freed
//!       snapshot slot can be reused — see `load_account_snapshot_unload_reload`.
//!  21 — load two distinct account snapshots into two slots concurrently and
//!       verify each — see `load_two_account_snapshots`.
//!  22 — load a subaccount snapshot (by seeds) AND an account snapshot (by
//!       pubkey) in one instruction, proving the `Account` and `Subaccount`
//!       snapshot-key variants never collide — see
//!       `account_and_subaccount_snapshot_distinct`.
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
const TRANSFER_LAMPORTS: u64 = 100_000;
const COUNTER_BYTES: u64 = 8; // size of the u64 counter stored in the subaccount
const SLOT_HEADER_SIZE: u64 = 88;
const SLOT_HEADER_OFFSET_KEY: u64 = 8;
const SLOT_HEADER_OFFSET_OWNER: u64 = 40;
const SLOT_HEADER_OFFSET_LAMPORTS: u64 = 72;
const SLOT_HEADER_OFFSET_DATA_LEN: u64 = 80;

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
    fn sol_load_subaccount_snapshot(
        seeds_addr: *const u8,
        seeds_len: u64,
        out_header_addr: *mut u64,
        _arg4: u64,
        _arg5: u64,
    ) -> u64;
    fn sol_load_account_snapshot(
        pubkey_addr: *const u8,
        out_header_addr: *mut u64,
        _arg3: u64,
        _arg4: u64,
        _arg5: u64,
    ) -> u64;
    fn sol_unload_subaccount(vm_header_addr: u64) -> u64;
    fn sol_read_subaccount(
        seeds_addr: *const u8,
        seeds_len: u64,
        buff: *mut u8,
        offset: u64,
        length: u64,
    ) -> u64;
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
        5 => load_increment_unload(accounts, payload),
        6 => create_oversized_payload(accounts, payload),
        7 => transfer_lamports(accounts, payload),
        8 => create_load_twice(accounts, payload),
        9 => read_subaccount(accounts, payload),
        10 => load_snapshot_verify(payload),
        11 => load_subaccount_snapshot_verify(payload),
        12 => load_subaccount_snapshot_cannot_modify(payload),
        13 => load_subaccount_snapshot_twice(payload),
        14 => load_subaccount_snapshot_unload_reload(payload),
        15 => load_two_subaccount_snapshots(payload),
        16 => subaccount_snapshot_independent_from_writable_load(accounts, payload),
        17 => load_account_snapshot_verify(payload),
        18 => load_account_snapshot_cannot_modify(payload),
        19 => load_account_snapshot_twice(payload),
        20 => load_account_snapshot_unload_reload(payload),
        21 => load_two_account_snapshots(payload),
        22 => account_and_subaccount_snapshot_distinct(payload),
        _ => Err(ProgramError::InvalidInstructionData),
    }
}

fn seeds_from_payer(payer_key: &Pubkey) -> [&[u8]; 2] {
    [payer_key.as_ref(), SEED_TAG]
}

fn create_load_write_unload(accounts: &[AccountInfo], payload: &[u8]) -> ProgramResult {
    let base = accounts.get(0).ok_or(ProgramError::NotEnoughAccountKeys)?;
    let payer = accounts.get(1).ok_or(ProgramError::NotEnoughAccountKeys)?;
    let seeds = seeds_from_payer(base.key);

    let space = payload.len() as u64;
    let result = unsafe {
        sol_create_subaccount(
            payer.key as *const Pubkey as *const u8,
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
    let base = accounts.get(0).ok_or(ProgramError::NotEnoughAccountKeys)?;
    let seeds = seeds_from_payer(base.key);

    let (header_addr, _view_addr) = load_rust(&seeds)?;
    write_data(header_addr, payload);
    unload(header_addr)
}

fn create_twice(accounts: &[AccountInfo], payload: &[u8]) -> ProgramResult {
    let base = accounts.get(0).ok_or(ProgramError::NotEnoughAccountKeys)?;
    let payer = accounts.get(1).ok_or(ProgramError::NotEnoughAccountKeys)?;
    let seeds = seeds_from_payer(base.key);

    let space = payload.len() as u64;
    let r1 = unsafe {
        sol_create_subaccount(
            payer.key as *const Pubkey as *const u8,
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
            payer.key as *const Pubkey as *const u8,
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

fn create_load_twice(accounts: &[AccountInfo], payload: &[u8]) -> ProgramResult {
    let base = accounts.get(0).ok_or(ProgramError::NotEnoughAccountKeys)?;
    let payer = accounts.get(1).ok_or(ProgramError::NotEnoughAccountKeys)?;
    let seeds = seeds_from_payer(base.key);

    let space = payload.len() as u64;
    let r1 = unsafe {
        sol_create_subaccount(
            payer.key as *const Pubkey as *const u8,
            seeds.as_ptr() as *const u8,
            seeds.len() as u64,
            space,
            FUNDING_LAMPORTS,
        )
    };
    if r1 != SUCCESS {
        return Err(ProgramError::Custom(0x30));
    }
    let (_header_addr, _view_addr) = load_rust(&seeds)?;

    let (_header_addr2, _view_addr2) = load_rust(&seeds)?;

    // If the second load succeeded, we have two slots loaded for the same subaccount, which is not allowed.
    Err(ProgramError::Custom(0x31))
}

/// Exercises `sol_read_subaccount` (disc=9).
///
/// Payload layout:
///   byte  0       : do_create (1 ⇒ create+load+write `content`+unload first,
///                   0 ⇒ skip — leaves the subaccount non-existent)
///   bytes 1..9    : offset (u64 LE) — read start offset
///   bytes 9..17   : length (u64 LE) — number of bytes to read
///   bytes 17..    : content — written to the subaccount when do_create=1;
///                   also the data the read result is verified against
///
/// When `sol_read_subaccount` returns an error (out-of-range read, or a
/// missing subaccount whose data is empty) the syscall aborts the
/// instruction with that error, so control never returns here and the
/// transaction fails with the syscall's `InstructionError`. On a successful
/// in-range read we additionally assert the bytes returned match
/// `content[offset..offset+length]`, returning `Custom(0x92)` on mismatch so
/// a silently-wrong read is caught.
fn read_subaccount(accounts: &[AccountInfo], payload: &[u8]) -> ProgramResult {
    const MAX_READ: usize = 512;

    let base = accounts.get(0).ok_or(ProgramError::NotEnoughAccountKeys)?;
    let payer = accounts.get(1).ok_or(ProgramError::NotEnoughAccountKeys)?;
    let seeds = seeds_from_payer(base.key);

    let do_create = *payload
        .first()
        .ok_or(ProgramError::InvalidInstructionData)?
        != 0;
    let do_load = *payload.get(1).ok_or(ProgramError::InvalidInstructionData)? != 0;
    let offset = u64::from_le_bytes(
        payload
            .get(2..10)
            .and_then(|s| s.try_into().ok())
            .ok_or(ProgramError::InvalidInstructionData)?,
    );
    let length = u64::from_le_bytes(
        payload
            .get(10..18)
            .and_then(|s| s.try_into().ok())
            .ok_or(ProgramError::InvalidInstructionData)?,
    );
    let content = payload
        .get(18..)
        .ok_or(ProgramError::InvalidInstructionData)?;

    if do_create {
        let r = unsafe {
            sol_create_subaccount(
                payer.key as *const Pubkey as *const u8,
                seeds.as_ptr() as *const u8,
                seeds.len() as u64,
                content.len() as u64,
                FUNDING_LAMPORTS,
            )
        };
        if r != SUCCESS {
            return Err(ProgramError::Custom(0x90));
        }
        let (header_addr, _view_addr) = load_rust(&seeds)?;
        write_data(header_addr, content);
        if !do_load {
            unload(header_addr)?;
        }
    } else if do_load {
        // Load an existing subaccount without writing to it first. This
        // verifies that `sol_read_subaccount` can read from a loaded
        // subaccount's data, and that the loaded data is correct (it must
        // be the same bytes the test wrote in a previous instruction, since
        // the subaccount is never unloaded in between).
        let (_header_addr, _view_addr) = load_rust(&seeds)?;
    }

    let read_len = length as usize;
    if read_len > MAX_READ {
        return Err(ProgramError::Custom(0x91));
    }
    let mut buffer = [0u8; MAX_READ];
    let result = unsafe {
        sol_read_subaccount(
            seeds.as_ptr() as *const u8,
            seeds.len() as u64,
            buffer.as_mut_ptr(),
            offset,
            length,
        )
    };
    // An out-of-range or missing-subaccount read aborts the instruction in
    // the syscall, so reaching here means the read succeeded — verify it.
    if result != SUCCESS {
        return Err(ProgramError::from(result));
    }

    let start = offset as usize;
    let expected = content
        .get(start..start.saturating_add(read_len))
        .ok_or(ProgramError::Custom(0x93))?;
    if &buffer[..read_len] != expected {
        return Err(ProgramError::Custom(0x92));
    }

    Ok(())
}

/// Exercises `sol_load_subaccount_snapshot_rust` (disc=10).
///
/// Loads a read-only, start-of-block snapshot of the subaccount and verifies
/// its data equals `expected`. The base pubkey is taken from the payload rather
/// than from `accounts[..]`, so the caller can omit the base account from the
/// transaction entirely — proving the snapshot load does not require it.
///
/// Payload layout:
///   bytes 0..32 : base pubkey (the first seed)
///   bytes 32..  : expected snapshot data (what the start-of-block read must
///                 return)
///
/// Returns `Custom(0xA0)` on a data mismatch and `Custom(0xA1)` on a length
/// mismatch; the slot is always released before returning.
fn load_snapshot_verify(payload: &[u8]) -> ProgramResult {
    let base_bytes = payload
        .get(0..32)
        .ok_or(ProgramError::InvalidInstructionData)?;
    let expected = payload
        .get(32..)
        .ok_or(ProgramError::InvalidInstructionData)?;
    let seeds: [&[u8]; 2] = [base_bytes, SEED_TAG];

    let header_addr = load_snapshot(&seeds)?;

    let data_len = unsafe {
        core::ptr::read_unaligned((header_addr + SLOT_HEADER_OFFSET_DATA_LEN) as *const u64)
    } as usize;
    let result = if data_len != expected.len() {
        Err(ProgramError::Custom(0xA1))
    } else {
        let data_ptr = (header_addr + SLOT_HEADER_SIZE) as *const u8;
        let data = unsafe { core::slice::from_raw_parts(data_ptr, data_len) };
        if data == expected {
            Ok(())
        } else {
            Err(ProgramError::Custom(0xA0))
        }
    };

    // Always release the slot before returning, surfacing the verification
    // result over a successful-unload status.
    let u = unsafe { sol_unload_subaccount(header_addr) };
    result?;
    if u != SUCCESS {
        return Err(ProgramError::from(u));
    }
    Ok(())
}

/// Exercises `sol_load_subaccount_snapshot` with full metadata verification
/// (disc=11). Base pubkey is taken from the payload (base-free).
///
/// Payload layout:
///   bytes  0..32 : base pubkey (first seed)
///   bytes 32..64 : expected owner
///   bytes 64..72 : expected lamports (u64 LE)
///   bytes 72..   : expected snapshot data
///
/// Custom codes: 0xC0 data mismatch, 0xC1 data_len mismatch, 0xC2 owner
/// mismatch, 0xC3 lamports mismatch.
fn load_subaccount_snapshot_verify(payload: &[u8]) -> ProgramResult {
    let base = payload.get(0..32).ok_or(ProgramError::InvalidInstructionData)?;
    let expected_owner = payload.get(32..64).ok_or(ProgramError::InvalidInstructionData)?;
    let expected_lamports = u64::from_le_bytes(
        payload
            .get(64..72)
            .and_then(|s| s.try_into().ok())
            .ok_or(ProgramError::InvalidInstructionData)?,
    );
    let expected_data = payload.get(72..).ok_or(ProgramError::InvalidInstructionData)?;
    let seeds: [&[u8]; 2] = [base, SEED_TAG];

    let header_addr = load_snapshot(&seeds)?;

    let result: ProgramResult = (|| {
        let data_len = snapshot_data_len(header_addr);
        if data_len != expected_data.len() {
            return Err(ProgramError::Custom(0xC1));
        }
        let data =
            unsafe { core::slice::from_raw_parts((header_addr + SLOT_HEADER_SIZE) as *const u8, data_len) };
        if data != expected_data {
            return Err(ProgramError::Custom(0xC0));
        }
        if snapshot_owner(header_addr).as_ref() != expected_owner {
            return Err(ProgramError::Custom(0xC2));
        }
        if snapshot_lamports(header_addr) != expected_lamports {
            return Err(ProgramError::Custom(0xC3));
        }
        Ok(())
    })();

    let u = unsafe { sol_unload_subaccount(header_addr) };
    result?;
    if u != SUCCESS {
        return Err(ProgramError::from(u));
    }
    Ok(())
}

/// Read-only enforcement for a subaccount snapshot (disc=12). Loads the
/// snapshot then stores into its data region; the region is read-only so the
/// store must fault and abort the instruction. `Custom(0xC5)` is returned only
/// if the write was wrongly accepted.
fn load_subaccount_snapshot_cannot_modify(payload: &[u8]) -> ProgramResult {
    let base = payload.get(0..32).ok_or(ProgramError::InvalidInstructionData)?;
    let seeds: [&[u8]; 2] = [base, SEED_TAG];
    let header_addr = load_snapshot(&seeds)?;
    let data_ptr = (header_addr + SLOT_HEADER_SIZE) as *mut u8;
    unsafe { core::ptr::write_unaligned(data_ptr, 0xFF) };
    let _ = unsafe { sol_unload_subaccount(header_addr) };
    Err(ProgramError::Custom(0xC5))
}

/// Loading the same subaccount snapshot twice in one instruction must fail
/// (disc=13). Returns `Custom(0xC6)` if the second load unexpectedly succeeds;
/// otherwise propagates the syscall error.
fn load_subaccount_snapshot_twice(payload: &[u8]) -> ProgramResult {
    let base = payload.get(0..32).ok_or(ProgramError::InvalidInstructionData)?;
    let seeds: [&[u8]; 2] = [base, SEED_TAG];
    let _h1 = load_snapshot(&seeds)?;
    let mut header2: u64 = 0;
    let r2 = unsafe {
        sol_load_subaccount_snapshot(
            seeds.as_ptr() as *const u8,
            seeds.len() as u64,
            &mut header2 as *mut u64,
            0,
            0,
        )
    };
    if r2 == SUCCESS {
        return Err(ProgramError::Custom(0xC6));
    }
    Err(ProgramError::from(r2))
}

/// Load a subaccount snapshot, verify + unload, then reload the SAME subaccount
/// into a freed slot and verify again (disc=14). Proves slot reuse.
///
/// Payload layout: base(32) ++ expected data.
fn load_subaccount_snapshot_unload_reload(payload: &[u8]) -> ProgramResult {
    let base = payload.get(0..32).ok_or(ProgramError::InvalidInstructionData)?;
    let expected = payload.get(32..).ok_or(ProgramError::InvalidInstructionData)?;
    let seeds: [&[u8]; 2] = [base, SEED_TAG];

    let h1 = load_snapshot(&seeds)?;
    verify_snapshot_data(h1, expected)?;
    unload(h1)?;

    let h2 = load_snapshot(&seeds)?;
    verify_snapshot_data(h2, expected)?;
    unload(h2)
}

/// Load two distinct subaccount snapshots into two slots at once and verify
/// each (disc=15). Both slots are released before returning.
///
/// Payload layout:
///   bytes  0..32 : base #1
///   bytes 32..64 : base #2
///   bytes 64..66 : data #1 length (u16 LE)
///   bytes 66..66+len1        : expected data #1
///   bytes 66+len1..          : expected data #2
fn load_two_subaccount_snapshots(payload: &[u8]) -> ProgramResult {
    let base1 = payload.get(0..32).ok_or(ProgramError::InvalidInstructionData)?;
    let base2 = payload.get(32..64).ok_or(ProgramError::InvalidInstructionData)?;
    let len1 = u16::from_le_bytes(
        payload
            .get(64..66)
            .and_then(|s| s.try_into().ok())
            .ok_or(ProgramError::InvalidInstructionData)?,
    ) as usize;
    let data1 = payload
        .get(66..66 + len1)
        .ok_or(ProgramError::InvalidInstructionData)?;
    let data2 = payload
        .get(66 + len1..)
        .ok_or(ProgramError::InvalidInstructionData)?;

    let seeds1: [&[u8]; 2] = [base1, SEED_TAG];
    let seeds2: [&[u8]; 2] = [base2, SEED_TAG];
    let h1 = load_snapshot(&seeds1)?;
    let h2 = load_snapshot(&seeds2)?;

    let result: ProgramResult = (|| {
        verify_snapshot_data(h1, data1)?;
        verify_snapshot_data(h2, data2)?;
        Ok(())
    })();

    let u2 = unsafe { sol_unload_subaccount(h2) };
    let u1 = unsafe { sol_unload_subaccount(h1) };
    result?;
    if u1 != SUCCESS {
        return Err(ProgramError::from(u1));
    }
    if u2 != SUCCESS {
        return Err(ProgramError::from(u2));
    }
    Ok(())
}

/// Proves the snapshot lane is independent of the live subaccount lane within a
/// single instruction (disc=16). `accounts[0]` is the base (writable). Loads
/// the subaccount writable, overwrites its data with V2, then loads a snapshot
/// of the SAME subaccount and asserts it still reads the start-of-block value
/// V1. Finally commits V2 (unload of the writable slot).
///
/// Payload layout:
///   bytes 0..2 : V1 length (u16 LE)
///   bytes 2..2+len : expected start-of-block data V1
///   bytes 2+len..  : V2 to overwrite the live subaccount with
fn subaccount_snapshot_independent_from_writable_load(
    accounts: &[AccountInfo],
    payload: &[u8],
) -> ProgramResult {
    let base = accounts.get(0).ok_or(ProgramError::NotEnoughAccountKeys)?;
    let len1 = u16::from_le_bytes(
        payload
            .get(0..2)
            .and_then(|s| s.try_into().ok())
            .ok_or(ProgramError::InvalidInstructionData)?,
    ) as usize;
    let v1 = payload
        .get(2..2 + len1)
        .ok_or(ProgramError::InvalidInstructionData)?;
    let v2 = payload
        .get(2 + len1..)
        .ok_or(ProgramError::InvalidInstructionData)?;
    let seeds = seeds_from_payer(base.key);

    // Writable load + overwrite the live subaccount with V2 (subaccount lane).
    let (header_w, _view) = load_rust(&seeds)?;
    write_data(header_w, v2);

    // Snapshot load — must still observe the start-of-block value V1.
    let header_s = load_snapshot(&seeds)?;
    let result = verify_snapshot_data(header_s, v1);
    let us = unsafe { sol_unload_subaccount(header_s) };

    // Commit the writable overwrite.
    let uw = unload(header_w);

    result?;
    if us != SUCCESS {
        return Err(ProgramError::from(us));
    }
    uw
}

/// Loads a read-only, start-of-block snapshot of an arbitrary account by its
/// pubkey via `sol_load_account_snapshot` and returns the slot header address.
fn load_account_snapshot(pubkey: &[u8]) -> Result<u64, ProgramError> {
    let mut header_addr: u64 = 0;
    let result = unsafe {
        sol_load_account_snapshot(pubkey.as_ptr(), &mut header_addr as *mut u64, 0, 0, 0)
    };
    if result != SUCCESS {
        return Err(ProgramError::from(result));
    }
    Ok(header_addr)
}

/// Reads the `data_len` field out of a slot header.
fn snapshot_data_len(header_addr: u64) -> usize {
    let len =
        unsafe { core::ptr::read_unaligned((header_addr + SLOT_HEADER_OFFSET_DATA_LEN) as *const u64) };
    len as usize
}

/// Reads the `lamports` field out of a slot header.
fn snapshot_lamports(header_addr: u64) -> u64 {
    unsafe { core::ptr::read_unaligned((header_addr + SLOT_HEADER_OFFSET_LAMPORTS) as *const u64) }
}

/// Reads the 32-byte `owner` field out of a slot header.
fn snapshot_owner(header_addr: u64) -> Pubkey {
    let bytes = unsafe {
        core::ptr::read_unaligned((header_addr + SLOT_HEADER_OFFSET_OWNER) as *const [u8; 32])
    };
    Pubkey::new_from_array(bytes)
}

/// Verifies the snapshot's `data_len` and data bytes equal `expected`.
/// Returns `Custom(0xA1)` on a length mismatch and `Custom(0xA0)` on a byte
/// mismatch (same codes `load_snapshot_verify` uses).
fn verify_snapshot_data(header_addr: u64, expected: &[u8]) -> ProgramResult {
    let data_len = snapshot_data_len(header_addr);
    if data_len != expected.len() {
        return Err(ProgramError::Custom(0xA1));
    }
    let data = unsafe { core::slice::from_raw_parts((header_addr + SLOT_HEADER_SIZE) as *const u8, data_len) };
    if data != expected {
        return Err(ProgramError::Custom(0xA0));
    }
    Ok(())
}

/// Exercises `sol_load_account_snapshot` (disc=11).
///
/// Payload layout:
///   bytes  0..32 : account pubkey
///   bytes 32..64 : expected owner
///   bytes 64..72 : expected lamports (u64 LE)
///   bytes 72..   : expected snapshot data
///
/// Verifies the start-of-block owner / lamports / data_len / data, then
/// releases the slot. Custom codes: 0xB0 data mismatch, 0xB1 data_len
/// mismatch, 0xB2 owner mismatch, 0xB3 lamports mismatch.
fn load_account_snapshot_verify(payload: &[u8]) -> ProgramResult {
    let pubkey = payload.get(0..32).ok_or(ProgramError::InvalidInstructionData)?;
    let expected_owner = payload.get(32..64).ok_or(ProgramError::InvalidInstructionData)?;
    let expected_lamports = u64::from_le_bytes(
        payload
            .get(64..72)
            .and_then(|s| s.try_into().ok())
            .ok_or(ProgramError::InvalidInstructionData)?,
    );
    let expected_data = payload.get(72..).ok_or(ProgramError::InvalidInstructionData)?;

    let header_addr = load_account_snapshot(pubkey)?;

    let result: ProgramResult = (|| {
        let data_len = snapshot_data_len(header_addr);
        if data_len != expected_data.len() {
            return Err(ProgramError::Custom(0xB1));
        }
        let data =
            unsafe { core::slice::from_raw_parts((header_addr + SLOT_HEADER_SIZE) as *const u8, data_len) };
        if data != expected_data {
            return Err(ProgramError::Custom(0xB0));
        }
        if snapshot_owner(header_addr).as_ref() != expected_owner {
            return Err(ProgramError::Custom(0xB2));
        }
        if snapshot_lamports(header_addr) != expected_lamports {
            return Err(ProgramError::Custom(0xB3));
        }
        Ok(())
    })();

    // Always release the slot before surfacing the verification result.
    let u = unsafe { sol_unload_subaccount(header_addr) };
    result?;
    if u != SUCCESS {
        return Err(ProgramError::from(u));
    }
    Ok(())
}

/// Exercises read-only enforcement of an account snapshot (disc=12).
///
/// Loads the snapshot then attempts to store into its data region. The region
/// is mapped read-only, so the store must fault and abort the instruction in
/// the VM — control never returns. If it somehow returns, the write was
/// wrongly accepted: release the slot and report `Custom(0xB5)`.
fn load_account_snapshot_cannot_modify(payload: &[u8]) -> ProgramResult {
    let pubkey = payload.get(0..32).ok_or(ProgramError::InvalidInstructionData)?;
    let header_addr = load_account_snapshot(pubkey)?;
    let data_ptr = (header_addr + SLOT_HEADER_SIZE) as *mut u8;
    unsafe { core::ptr::write_unaligned(data_ptr, 0xFF) };
    // Unreachable on a correctly read-only mapping.
    let _ = unsafe { sol_unload_subaccount(header_addr) };
    Err(ProgramError::Custom(0xB5))
}

/// Loading the same account snapshot twice in one instruction must fail
/// (disc=13). Returns `Custom(0xB6)` if the second load unexpectedly succeeds;
/// otherwise propagates the syscall error so the test can assert on it.
fn load_account_snapshot_twice(payload: &[u8]) -> ProgramResult {
    let pubkey = payload.get(0..32).ok_or(ProgramError::InvalidInstructionData)?;
    let _h1 = load_account_snapshot(pubkey)?;
    let mut header2: u64 = 0;
    let r2 = unsafe {
        sol_load_account_snapshot(pubkey.as_ptr(), &mut header2 as *mut u64, 0, 0, 0)
    };
    if r2 == SUCCESS {
        return Err(ProgramError::Custom(0xB6));
    }
    Err(ProgramError::from(r2))
}

/// Loads an account snapshot, verifies + unloads it, then reloads the SAME
/// account into a (now-freed) slot and verifies again (disc=14). Proves a
/// released snapshot slot can be reused.
///
/// Payload layout: pubkey(32) ++ expected data.
fn load_account_snapshot_unload_reload(payload: &[u8]) -> ProgramResult {
    let pubkey = payload.get(0..32).ok_or(ProgramError::InvalidInstructionData)?;
    let expected = payload.get(32..).ok_or(ProgramError::InvalidInstructionData)?;

    let h1 = load_account_snapshot(pubkey)?;
    verify_snapshot_data(h1, expected)?;
    unload(h1)?;

    let h2 = load_account_snapshot(pubkey)?;
    verify_snapshot_data(h2, expected)?;
    unload(h2)
}

/// Loads two distinct account snapshots into two slots at once and verifies
/// each (disc=15). Both slots are released before returning.
///
/// Payload layout:
///   bytes  0..32 : pubkey #1
///   bytes 32..64 : pubkey #2
///   bytes 64..66 : data #1 length (u16 LE)
///   bytes 66..66+len1        : expected data #1
///   bytes 66+len1..          : expected data #2
fn load_two_account_snapshots(payload: &[u8]) -> ProgramResult {
    let pk1 = payload.get(0..32).ok_or(ProgramError::InvalidInstructionData)?;
    let pk2 = payload.get(32..64).ok_or(ProgramError::InvalidInstructionData)?;
    let len1 = u16::from_le_bytes(
        payload
            .get(64..66)
            .and_then(|s| s.try_into().ok())
            .ok_or(ProgramError::InvalidInstructionData)?,
    ) as usize;
    let data1 = payload
        .get(66..66 + len1)
        .ok_or(ProgramError::InvalidInstructionData)?;
    let data2 = payload
        .get(66 + len1..)
        .ok_or(ProgramError::InvalidInstructionData)?;

    let h1 = load_account_snapshot(pk1)?;
    let h2 = load_account_snapshot(pk2)?;

    let result: ProgramResult = (|| {
        verify_snapshot_data(h1, data1)?;
        verify_snapshot_data(h2, data2)?;
        Ok(())
    })();

    // Release both slots regardless of the verification outcome.
    let u2 = unsafe { sol_unload_subaccount(h2) };
    let u1 = unsafe { sol_unload_subaccount(h1) };
    result?;
    if u1 != SUCCESS {
        return Err(ProgramError::from(u1));
    }
    if u2 != SUCCESS {
        return Err(ProgramError::from(u2));
    }
    Ok(())
}

/// Loads a subaccount snapshot (by seeds) and an account snapshot (by pubkey)
/// in one instruction (disc=16). The two use distinct `SnapshotKey` variants —
/// `Subaccount(derived)` vs `Account(pubkey)` — so even when the test points
/// the account snapshot at the subaccount's *derived* address they must resolve
/// to independent lane entries with their own data.
///
/// Payload layout:
///   bytes  0..32 : base pubkey (first subaccount seed)
///   bytes 32..64 : account pubkey for the account snapshot
///   bytes 64..66 : account data length (u16 LE)
///   bytes 66..66+len : expected account-snapshot data
///   bytes 66+len..   : expected subaccount-snapshot data
fn account_and_subaccount_snapshot_distinct(payload: &[u8]) -> ProgramResult {
    let base = payload.get(0..32).ok_or(ProgramError::InvalidInstructionData)?;
    let account_pk = payload.get(32..64).ok_or(ProgramError::InvalidInstructionData)?;
    let acct_len = u16::from_le_bytes(
        payload
            .get(64..66)
            .and_then(|s| s.try_into().ok())
            .ok_or(ProgramError::InvalidInstructionData)?,
    ) as usize;
    let account_data = payload
        .get(66..66 + acct_len)
        .ok_or(ProgramError::InvalidInstructionData)?;
    let subaccount_data = payload
        .get(66 + acct_len..)
        .ok_or(ProgramError::InvalidInstructionData)?;

    let seeds: [&[u8]; 2] = [base, SEED_TAG];
    let h_sub = load_snapshot(&seeds)?;
    let h_acct = load_account_snapshot(account_pk)?;

    let result: ProgramResult = (|| {
        verify_snapshot_data(h_sub, subaccount_data)?;
        verify_snapshot_data(h_acct, account_data)?;
        Ok(())
    })();

    let ua = unsafe { sol_unload_subaccount(h_acct) };
    let us = unsafe { sol_unload_subaccount(h_sub) };
    result?;
    if us != SUCCESS {
        return Err(ProgramError::from(us));
    }
    if ua != SUCCESS {
        return Err(ProgramError::from(ua));
    }
    Ok(())
}

fn unload_twice(accounts: &[AccountInfo], payload: &[u8]) -> ProgramResult {
    let base = accounts.get(0).ok_or(ProgramError::NotEnoughAccountKeys)?;
    let payer = accounts.get(1).ok_or(ProgramError::NotEnoughAccountKeys)?;
    let seeds = seeds_from_payer(base.key);

    let space = payload.len() as u64;
    let r1 = unsafe {
        sol_create_subaccount(
            payer.key as *const Pubkey as *const u8,
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
    let base = accounts.get(0).ok_or(ProgramError::NotEnoughAccountKeys)?;
    let payer = accounts.get(1).ok_or(ProgramError::NotEnoughAccountKeys)?;
    let seeds = seeds_from_payer(base.key);

    if payload.len() != COUNTER_BYTES as usize {
        return Err(ProgramError::InvalidInstructionData);
    }
    let mut initial_bytes = [0u8; 8];
    initial_bytes.copy_from_slice(payload);
    let initial_value = u64::from_le_bytes(initial_bytes);

    let r = unsafe {
        sol_create_subaccount(
            payer.key as *const Pubkey as *const u8,
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

    cpi_increment(program_id, base)?;

    // Read the counter back from the direct-mapped data region. The slot
    // is still loaded — the outer never called unload — so this is the
    // exact same host bytes the inner just wrote.
    let data_ptr = (header_addr + SLOT_HEADER_SIZE) as *const u8;
    let observed = unsafe { core::ptr::read_unaligned(data_ptr as *const u64) };
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
fn load_increment_unload(accounts: &[AccountInfo], payload: &[u8]) -> ProgramResult {
    let base = accounts.get(0).ok_or(ProgramError::NotEnoughAccountKeys)?;
    let seeds = seeds_from_payer(base.key);

    let offset: u64 = payload
        .get(0..8)
        .and_then(|slice| slice.try_into().ok())
        .map(u64::from_le_bytes)
        .unwrap_or(0);

    let (header_addr, _view_addr) = load_rust(&seeds)?;

    let data_ptr = (header_addr + SLOT_HEADER_SIZE + offset) as *mut u8;
    let current = unsafe { core::ptr::read_unaligned(data_ptr as *const u64) };
    let next = current.wrapping_add(1);
    unsafe { core::ptr::write_unaligned(data_ptr as *mut u64, next) };

    unload(header_addr)
}

fn create_oversized_payload(accounts: &[AccountInfo], payload: &[u8]) -> ProgramResult {
    let base = accounts.get(0).ok_or(ProgramError::NotEnoughAccountKeys)?;
    let payer = accounts.get(1).ok_or(ProgramError::NotEnoughAccountKeys)?;
    let seeds = seeds_from_payer(base.key);

    let space = payload.len() as u64;
    let result = unsafe {
        sol_create_subaccount(
            payer.key as *const Pubkey as *const u8,
            seeds.as_ptr() as *const u8,
            seeds.len() as u64,
            space,
            FUNDING_LAMPORTS,
        )
    };
    if result != SUCCESS {
        return Err(ProgramError::Custom(0x50));
    }

    let new_length = 20 * 1024 * 1024; // 20 MiB, larger than the max payload of 10 MiB
    let (header_addr, _view_addr) = load_rust(&seeds)?;
    unsafe {
        let length_ptr = (header_addr + SLOT_HEADER_OFFSET_DATA_LEN) as *mut u64;
        core::ptr::write(length_ptr, new_length);
    }

    unsafe { sol_unload_subaccount(header_addr) };

    Ok(())
}

fn transfer_lamports(accounts: &[AccountInfo], payload: &[u8]) -> ProgramResult {
    let base = accounts.get(0).ok_or(ProgramError::NotEnoughAccountKeys)?;
    let payer = accounts.get(1).ok_or(ProgramError::NotEnoughAccountKeys)?;
    let seeds = seeds_from_payer(base.key);

    let is_balanced = payload.get(0).map(|b| *b != 0).unwrap_or(true);
    let is_unload = payload.get(1).map(|b| *b != 0).unwrap_or(true);

    let (header_addr, _view_addr) = load_rust(&seeds)?;
    let lamports_ptr = (header_addr + SLOT_HEADER_OFFSET_LAMPORTS) as *mut u64;
    let lamports_before = unsafe { core::ptr::read_unaligned(lamports_ptr) };
    if lamports_before < TRANSFER_LAMPORTS {
        return Err(ProgramError::Custom(0x60));
    } else {
        let lamports_after = lamports_before - TRANSFER_LAMPORTS;
        unsafe { core::ptr::write_unaligned(lamports_ptr, lamports_after) };
    }

    if is_balanced {
        **payer.lamports.borrow_mut() = payer
            .lamports()
            .checked_add(TRANSFER_LAMPORTS)
            .ok_or(ProgramError::Custom(0x61))?;
    }

    if is_unload {
        unload(header_addr)?;
    }

    Ok(())
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

fn load_snapshot(seeds: &[&[u8]]) -> Result<u64, ProgramError> {
    let mut header_addr: u64 = 0;
    let result = unsafe {
        sol_load_subaccount_snapshot(
            seeds.as_ptr() as *const u8,
            seeds.len() as u64,
            &mut header_addr as *mut u64,
            0,
            0,
        )
    };
    if result != SUCCESS {
        return Err(ProgramError::from(result));
    }
    Ok(header_addr)
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
        core::ptr::write(
            p.add(8) as *mut u64,
            header_addr + SLOT_HEADER_OFFSET_LAMPORTS,
        );
        core::ptr::write(p.add(16) as *mut u64, data_len);
        core::ptr::write(p.add(24) as *mut u64, header_addr + SLOT_HEADER_SIZE);
        core::ptr::write(
            p.add(32) as *mut u64,
            header_addr + SLOT_HEADER_OFFSET_OWNER,
        );
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
fn cpi_increment(callee_program_id: &Pubkey, base: &AccountInfo) -> ProgramResult {
    let inner_ix = Instruction {
        program_id: *callee_program_id,
        accounts: vec![AccountMeta::new(*base.key, false)],
        data: vec![5u8],
    };
    invoke_signed(&inner_ix, &[base.clone()], &[]).map_err(|_| ProgramError::Custom(0x41))
}
