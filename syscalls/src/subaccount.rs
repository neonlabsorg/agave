use solana_program_runtime::memory::translate_vm_slice;
use solana_svm_measure::measure::Measure;
#[allow(deprecated)]
use {
    crate::{Error, consume_compute_meter, translate_slice, translate_slice_mut, translate_type, translate_type_mut,},
    solana_account::{AccountSharedData, ReadableAccount, WritableAccount},
    solana_address::{Address, error::AddressError},
    solana_instruction::error::InstructionError,
    solana_program_entrypoint::SUCCESS,
    solana_program_runtime::{
        cpi::{CallerAccount, SolAccountInfo,},
        invoke_context::{InvokeContext, SerializedAccountMetadata},
    },
    solana_pubkey::{Pubkey, MAX_SEED_LEN, MAX_SEEDS, PUBKEY_BYTES},
    solana_sbpf::{
        declare_builtin_function,
        memory_region::{MemoryMapping, MemoryRegion},
    },
    solana_sdk_ids::system_program,
    solana_sha256_hasher::hashv,
    solana_svm_log_collector::ic_msg,
    solana_system_interface::MAX_PERMITTED_DATA_LENGTH,
    solana_transaction_context::{vm_slice::VmSlice, 
        MAX_ACCOUNTS_PER_TRANSACTION, SUBACCOUNT_MARKER
    },
};

// ============================================================================
// F10 — Subaccounts syscalls (PRS-153)
//
 // This module implements the currently supported subaccount syscall surface:
 // create, load, and unload. Earlier rollout notes referenced
 // `sol_set_subaccount_slice` and `sol_self_invoke_*`, but those syscalls are
 // not part of the current design and are intentionally not described here.
 // ============================================================================

// Derives the on-chain storage address for a subaccount given its owner-side
// pubkey. Mirrors `subaccount_address` from parasol-dev `syscalls/src/lib.rs`.
fn subaccount_address(pubkey: &Pubkey) -> Pubkey {
    let subaccount_address = hashv(&[&[1u8], pubkey.as_ref()]);
    Pubkey::new_from_array(subaccount_address.to_bytes())
}

fn create_subaccount_address(
    seeds: &[&[u8]],
    program_id: &Address,
) -> Result<Address, AddressError> {
    if seeds.len() > MAX_SEEDS {
        return Err(AddressError::MaxSeedLengthExceeded);
    }
    if seeds.iter().any(|seed| seed.len() > MAX_SEED_LEN) {
        return Err(AddressError::MaxSeedLengthExceeded);
    }

    // Perform the calculation inline, calling this from within a program is
    // not supported
    {
        const SUBACCOUNT_MARKER: &[u8; 10] = b"SubAccount";

        let mut hasher = solana_sha256_hasher::Hasher::default();
        for seed in seeds.iter() {
            hasher.hash(seed);
        }
        hasher.hashv(&[program_id.as_ref(), SUBACCOUNT_MARKER]);
        let hash = hasher.result();

        Ok(Address::from(hash.to_bytes()))
    }
}

declare_builtin_function!(
    /// F10: allocate a subaccount for the current program and return its index.
    SyscallCreateSubaccount,
    fn rust(
        invoke_context: &mut InvokeContext,
        _payer_pubkey_addr: u64,
        seeds_addr: u64,
        seeds_len: u64,
        space: u64,
        lamports: u64,
        memory_mapping: &mut MemoryMapping,
    ) -> Result<u64, Error> {
        let syscall_base_cost = invoke_context.get_execution_cost().syscall_base_cost;
        consume_compute_meter(invoke_context, syscall_base_cost)?;
        let check_aligned = invoke_context.get_check_aligned();

        let mut compute_subaccounts_time = Measure::start("compute_subaccounts");
        let (program_id, subaccount_pubkey) = {
            let instruction_context = invoke_context
                .transaction_context
                .get_current_instruction_context()?;
            let program_id = *instruction_context.get_program_key()?;
            let (subaccount_pubkey, is_writable) = translate_subaccount_seeds(
                &program_id,
                seeds_addr,
                seeds_len,
                memory_mapping,
                check_aligned,
                invoke_context,
                &instruction_context,
            )?;

            if !is_writable {
                return Err(InstructionError::ReadonlyDataModified.into());
            }

            (program_id, subaccount_pubkey)
        };
        compute_subaccounts_time.stop();
        invoke_context.timings.compute_subaccounts_us += compute_subaccounts_time.as_us();

        // F10 W9: load any pre-existing on-chain state for the subaccount
        // address through the SVM `TransactionProcessingCallback`. Mirrors
        // the parasol-dev reference implementation — fresh allocations fall
        // back to `AccountSharedData::default()` (the common case), while
        // already-allocated subaccounts re-enter with their full payload so
        // self-invoke deserialization sees real data instead of empty bytes.
        let subaccount_index = if let Some(subaccount_index) = invoke_context
            .transaction_context
            .find_index_of_subaccount(&subaccount_pubkey)
        {
            subaccount_index
        } else {
            let subaccount_address = subaccount_address(&subaccount_pubkey);
            let (subaccount, _slot) = invoke_context
                .get_account_shared_data(&subaccount_address)
                .unwrap_or_else(|| (AccountSharedData::default(), 0));
            let data_len_cost = (subaccount.data().len() as u64)
                .checked_div(invoke_context.get_execution_cost().cpi_bytes_per_unit)
                .unwrap_or(u64::MAX);
            consume_compute_meter(invoke_context, data_len_cost)?;

            invoke_context
                .transaction_context
                .add_subaccount(subaccount_pubkey, subaccount)?
        };

        let system_program_index = invoke_context
            .transaction_context
            .find_index_of_account(&system_program::id())
            .ok_or(InstructionError::MissingAccount)?;

        let dedup_map = vec![u16::MAX; MAX_ACCOUNTS_PER_TRANSACTION];
        invoke_context
            .transaction_context
            .configure_next_instruction(
                system_program_index,
                Vec::new(),
                dedup_map,
                std::borrow::Cow::Borrowed(&[]),
            )?;
        invoke_context.transaction_context.push()?;
        {
            let instruction_context = invoke_context
                .transaction_context
                .get_current_instruction_context()?;
            let mut subaccount = instruction_context.try_borrow_subaccount_by_tx_index(subaccount_index, true)?;

            // If `to` already has data or a non-system owner, refuse — the
            // slot has already been claimed. `message_processor` enforces the
            // same invariant for the main lane.
            if !subaccount.get_data().is_empty() || !system_program::check_id(subaccount.get_owner())
            {
                ic_msg!(
                    invoke_context,
                    "Allocate: subaccount {:?} already in use",
                    subaccount_pubkey,
                );
                return Err(InstructionError::AccountAlreadyInitialized.into());
            }

            if space > MAX_PERMITTED_DATA_LENGTH {
                ic_msg!(
                    invoke_context,
                    "Allocate: requested {}, max allowed {}",
                    space,
                    MAX_PERMITTED_DATA_LENGTH
                );
                return Err(InstructionError::InvalidArgument.into());
            }

            subaccount.set_data_length(space as usize)?;
            subaccount.set_owner(&program_id.to_bytes())?;
        }
        invoke_context.transaction_context.pop()?;

        if lamports > 0 {
            // W8e: fund the subaccount from the payer account. Requires the
            // self-invoke path so the system-program `Transfer` instruction
            // can execute inside the current transaction frame.
        }

        // Mark the account as a subaccount via the SDK-side `rent_epoch`
        // sentinel (see parasol-fork-dev SDK commit 5407b64b / W8b-sdk).
        invoke_context
            .transaction_context
            .accounts()
            .try_borrow_mut_subaccount(subaccount_index)?
            .set_subaccount_mark();

        // Sync the freshly-created subaccount with any `sol_load_subaccount`
        // slot that holds it. The slot's `caller_account_view_addr` points
        // at the program-owned `SolAccountInfo`-shaped struct; the slot's
        // `caller_account_metadata` carries the field VM-addresses captured
        // at load time. `CallerAccount::from_sol_account_info` verifies the
        // program hasn't drifted those pointers and yields mut handles to
        // the lamports / owner fields the program reads. The data region is
        // re-installed because `set_data_length(space)` may have
        // reallocated the underlying `AccountSharedData` buffer.
        sync_subaccount_slot_after_mutation(
            invoke_context,
            memory_mapping,
            check_aligned,
            subaccount_index,
        )?;

        Ok(SUCCESS)
    }
);

// F10: helpers for `sol_load_subaccount` / `sol_unload_subaccount`.
//
// Each pre-reserved slot owns two regions in the VM memory map:
//   - a fixed-size 88-byte header region at `vm_header_addr` (writable,
//     backed by the input buffer), and
//   - a placeholder data region at `vm_data_addr` (readonly empty initially)
//     whose VM-address window is reserved with
//     `SUBACCOUNT_SLOT_DATA_RESERVED_VM_BYTES`. `sol_load_subaccount` swaps
//     this region for one pointing at the loaded subaccount's
//     `AccountSharedData` storage (direct mapping), so the program reads
//     and writes the live host data with no extra copy.
//
// Header layout (matches the aligned `serialize_parameters` non-duplicate
// subaccount record header):
//
//   offset 0  : NON_DUP_MARKER (u8)
//   offset 1  : is_signer (u8)
//   offset 2  : is_writable (u8)
//   offset 3  : is_executable (u8)
//   offset 4  : padding (4 bytes)
//   offset 8  : key (32 bytes)
//   offset 40 : owner (32 bytes)
//   offset 72 : lamports (u64)
//   offset 80 : data_len (u64)
//
// The slot does NOT include rent_epoch in the header — the loader exposes it
// to the program through the caller-supplied `SolAccountInfo` instead.

use solana_program_runtime::serialization::{
    SLOT_HEADER_OFFSET_KEY, SLOT_HEADER_OFFSET_LAMPORTS, SLOT_HEADER_OFFSET_OWNER,
    SUBACCOUNT_SLOT_HEADER_SIZE,
};

/// Writes the slot's 88-byte header bytes (key/owner/lamports/data_len) at
/// the slot's stable header VM address.
#[allow(clippy::too_many_arguments)]
#[allow(clippy::arithmetic_side_effects)]
fn write_subaccount_slot_header(
    memory_mapping: &MemoryMapping,
    check_aligned: bool,
    vm_header_addr: u64,
    key: &Pubkey,
    owner: &Pubkey,
    lamports: u64,
    data_len: usize,
    is_writable: bool,
) -> Result<(), Error> {
    let header = translate_slice_mut::<u8>(
        memory_mapping,
        vm_header_addr,
        SUBACCOUNT_SLOT_HEADER_SIZE as u64,
        check_aligned,
    )?;
    header.fill(0);
    header[0] = solana_program_entrypoint::NON_DUP_MARKER;
    header[1] = 0; // is_signer — subaccounts can't sign
    header[2] = is_writable as u8;
    header[3] = 0; // is_executable
    header[SLOT_HEADER_OFFSET_KEY as usize..SLOT_HEADER_OFFSET_OWNER as usize]
        .copy_from_slice(key.as_ref());
    header[SLOT_HEADER_OFFSET_OWNER as usize..SLOT_HEADER_OFFSET_LAMPORTS as usize]
        .copy_from_slice(owner.as_ref());
    header[SLOT_HEADER_OFFSET_LAMPORTS as usize
        ..SLOT_HEADER_OFFSET_LAMPORTS as usize + std::mem::size_of::<u64>()]
        .copy_from_slice(&lamports.to_le_bytes());
    header[SUBACCOUNT_SLOT_HEADER_SIZE - std::mem::size_of::<u64>()..SUBACCOUNT_SLOT_HEADER_SIZE]
        .copy_from_slice(&(data_len as u64).to_le_bytes());
    Ok(())
}

/// Replaces the slot's data region with one backed by the loaded subaccount's
/// `AccountSharedData` storage (direct mapping). The new region inherits the
/// slot's stable `vm_data_addr` so subsequent program reads/writes hit the
/// live host data with no extra copy.
///
/// SAFETY: the host pointer captured in the `MemoryRegion` must remain valid
/// for the lifetime of the VM. Since the subaccount is registered with
/// `transaction_context.add_subaccount`, its `AccountSharedData` storage is
/// pinned to the transaction context for the whole instruction's duration.
fn install_subaccount_data_region(
    invoke_context: &mut InvokeContext,
    memory_mapping: &mut MemoryMapping,
    subaccount_index: solana_transaction_context::IndexOfAccount,
    vm_data_addr: u64,
    is_writable: bool,
) -> Result<(), Error> {
    let mut borrowed = invoke_context
        .transaction_context
        .accounts()
        .try_borrow_mut_subaccount(subaccount_index)?;
    let host_slice = borrowed.data_as_mut_slice();
    let new_region = if is_writable {
        let mut region = MemoryRegion::new_writable(host_slice, vm_data_addr);
        region.access_violation_handler_payload = Some(subaccount_index | SUBACCOUNT_MARKER);
        region
    } else {
        // Need an immutable view; reborrow as shared reference for region.
        MemoryRegion::new_readonly(&*host_slice, vm_data_addr)
    };
    drop(borrowed);
    let (region_index, _) = memory_mapping
        .find_region(vm_data_addr)
        .ok_or(InstructionError::MissingAccount)?;
    memory_mapping
        .replace_region(region_index, new_region)
        .map_err(|_| InstructionError::InvalidArgument)?;
    Ok(())
}

/// Restores the slot's data region to the empty readonly placeholder so the
/// next `sol_load_subaccount` call can install a fresh region.
fn restore_subaccount_data_placeholder(
    memory_mapping: &mut MemoryMapping,
    vm_data_addr: u64,
) -> Result<(), Error> {
    let (region_index, _) = memory_mapping
        .find_region(vm_data_addr)
        .ok_or(InstructionError::MissingAccount)?;
    memory_mapping
        .replace_region(region_index, MemoryRegion::new_readonly(&[], vm_data_addr))
        .map_err(|_| InstructionError::InvalidArgument)?;
    Ok(())
}

/// Syncs a subaccount's freshly-mutated `AccountSharedData` state back into
/// the matching `SubaccountSlot` (if any). Called from `SyscallCreateSubaccount`
/// after a `system_program::Allocate` push/pop frame mutates the subaccount.
///
/// Verification: the program's account-view pointers (in the
/// caller-supplied `SolAccountInfo` at `slot.caller_account_view_addr`) must
/// match the field-addresses captured in `slot.caller_account_metadata` at
/// load time. `CallerAccount::from_sol_account_info` performs that check and
/// returns mut handles to the VM-side fields the program reads.
///
/// State propagation:
///   1. Slot header lamports / owner — updated through the CallerAccount
///      mut handles (which point at `slot_header[72..80]` / `slot_header[40..72]`
///      via the program-supplied `SolAccountInfo` pointers).
///   2. Program-side `SolAccountInfo.data_len` — written through
///      `caller_account.ref_to_len_in_vm`.
///   3. Slot header `data_len` field at `vm_data_addr - 8` (== slot header
///      offset 80).
///   4. Slot data region — re-installed because `set_data_length` may have
///      reallocated the underlying `AccountSharedData` storage.
fn sync_subaccount_slot_after_mutation(
    invoke_context: &mut InvokeContext,
    memory_mapping: &mut MemoryMapping,
    check_aligned: bool,
    subaccount_index: solana_transaction_context::IndexOfAccount,
) -> Result<(), Error> {
    // Look up the matching slot. Clone out everything we need so we don't
    // hold a borrow on `syscall_context` across the AccountSharedData read.
    let Some((view_addr, metadata, vm_data_addr, is_writable)) = ({
        let syscall_context = invoke_context.get_syscall_context()?;
        syscall_context
            .subaccount_slots
            .iter()
            .find(|s| s.occupied_subaccount_index == Some(subaccount_index))
            .and_then(|s| {
                s.caller_account_metadata
                    .as_ref()
                    .map(|m| (s.caller_account_view_addr, m.clone(), s.vm_data_addr, s.is_writable))
            })
    }) else {
        return Ok(());
    };

    // Read fresh state from the AccountSharedData storage.
    let (lamports, owner, data_len) = {
        let borrowed = invoke_context
            .transaction_context
            .accounts()
            .try_borrow_subaccount(subaccount_index)?;
        (borrowed.lamports(), *borrowed.owner(), borrowed.data().len())
    };

    // Build a CallerAccount from the program-supplied SolAccountInfo. This
    // verifies (under `stricter_abi_and_runtime_constraints`) that the
    // program hasn't moved the field pointers since load time.
    let view = translate_type::<SolAccountInfo>(memory_mapping, view_addr, check_aligned)?;
    {
        let caller_account = CallerAccount::from_sol_account_info(
            invoke_context,
            memory_mapping,
            check_aligned,
            view_addr,
            view,
            &metadata,
        )?;

        *caller_account.lamports = lamports;
        *caller_account.owner = owner;
        *caller_account.ref_to_len_in_vm = data_len as u64;
    }

    // Slot header `data_len` lives at `vm_data_addr - 8` (==
    // `vm_header_addr + SLOT_HEADER_OFFSET_DATA_LEN`); mirror what
    // `update_caller_account` does for the standard CPI flow.
    let serialized_len_ptr = translate_type_mut::<u64>(
        memory_mapping,
        vm_data_addr.saturating_sub(std::mem::size_of::<u64>() as u64),
        check_aligned,
    )?;
    *serialized_len_ptr = data_len as u64;

    // Re-install the slot's data region against the (possibly-relocated)
    // `AccountSharedData` buffer.
    install_subaccount_data_region(
        invoke_context,
        memory_mapping,
        subaccount_index,
        vm_data_addr,
        is_writable,
    )?;
    Ok(())
}

fn translate_subaccount_seeds(
    program_id: &Pubkey,
    seeds_addr: u64,
    seeds_len: u64,
    memory_mapping: &MemoryMapping,
    check_aligned: bool,
    invoke_context: &InvokeContext,
    instruction_context: &solana_transaction_context::InstructionContext,
) -> Result<(Pubkey, bool), Error> {
    let untranslated_seeds =
        translate_slice::<VmSlice<u8>>(memory_mapping, seeds_addr, seeds_len, check_aligned)?;
    if untranslated_seeds.len() > MAX_SEEDS {
        return Err(Box::new(InstructionError::MaxSeedLengthExceeded));
    }
    let seeds = untranslated_seeds
        .iter()
        .map(|untranslated_seed| {
            translate_vm_slice(untranslated_seed, memory_mapping, check_aligned)
        })
        .collect::<Result<Vec<_>, Error>>()?;
    // ic_msg!(invoke_context, "2");
    let base_seed: [u8; 32] = (*seeds.first().ok_or(InstructionError::InvalidArgument)?)
        .try_into()
        .map_err(|_| InstructionError::InvalidArgument)?;
    let base_pubkey = Pubkey::new_from_array(base_seed);
    let base_index_in_transaction = invoke_context
        .transaction_context
        .find_index_of_account(&base_pubkey)
        .ok_or(InstructionError::InvalidArgument)?;
    let base_index_in_instruction = instruction_context
        .get_index_of_account_in_instruction(base_index_in_transaction)
        .map_err(|_| InstructionError::InvalidArgument)?;

    let is_writable = instruction_context
        .is_instruction_account_writable(base_index_in_instruction)
        .map_err(|_| InstructionError::InvalidArgument)?;

    let subaccount_pubkey = create_subaccount_address(&seeds, program_id)
        .map_err(|_| InstructionError::InvalidSeeds)?;

    Ok((subaccount_pubkey, is_writable))
}


declare_builtin_function!(
    /// F10: load an on-chain subaccount into a pre-reserved VM slot, with
    /// the slot's data region direct-mapped onto the live `AccountSharedData`.
    ///
    /// `seeds_addr` / `seeds_len` describe a `&[&[u8]]` seed list the same
    /// way `sol_create_subaccount` accepts: the first seed is interpreted
    /// as a base account pubkey that must be present in the current
    /// instruction's account list — its writable bit propagates to the
    /// loaded subaccount. The PDA derived from `(seeds, program_id)` names
    /// the subaccount whose on-chain storage at `subaccount_address(pda)`
    /// is fetched and exposed inside the VM.
    ///
    /// `account_view_addr` is a VM pointer to a caller-owned account-view
    /// buffer (typically a `SolAccountInfo`-shaped struct on the program's
    /// stack or heap). The syscall does **not** write into this buffer —
    /// the program populates it itself from the slot's regions. The pointer
    /// is stored opaquely on the slot so the runtime can reconcile state
    /// at CPI sync time using the captured field addresses (the slot's
    /// [`SerializedAccountMetadata`]).
    ///
    /// Loading the same subaccount twice in a single invocation is rejected
    /// with [`InstructionError::AccountAlreadyInitialized`] — the program
    /// must `sol_unload_subaccount` first if it wants to rebind the slot
    /// to a fresh account-view buffer.
    ///
    /// On success writes the slot's stable `vm_header_addr` into
    /// `*out_header_addr` and returns [`SUCCESS`].
    /// `sol_unload_subaccount` takes the `vm_header_addr` to release.
    SyscallLoadSubaccount,
    fn rust(
        invoke_context: &mut InvokeContext,
        seeds_addr: u64,
        seeds_len: u64,
        account_view_addr: u64,
        out_header_addr: u64,
        _arg5: u64,
        memory_mapping: &mut MemoryMapping,
    ) -> Result<u64, Error> {
        let mut load_subaccount_time = Measure::start("load_subaccount");
        let syscall_base_cost = invoke_context.get_execution_cost().syscall_base_cost;
        consume_compute_meter(invoke_context, syscall_base_cost)?;
        let check_aligned = invoke_context.get_check_aligned();

        let mut compute_subaccounts_time = Measure::start("compute_subaccounts");
        // Translate seeds → derive PDA → inherit base account writable bit.
        let (subaccount_pubkey, is_writable) = {
            let instruction_context = invoke_context
                .transaction_context
                .get_current_instruction_context()?;
            let program_id = *instruction_context.get_program_key()?;
            translate_subaccount_seeds(
                &program_id,
                seeds_addr,
                seeds_len,
                memory_mapping,
                check_aligned,
                invoke_context,
                &instruction_context,
            )?
        };
        compute_subaccounts_time.stop();
        invoke_context.timings.compute_subaccounts_us += compute_subaccounts_time.as_us();

        // Find or load the on-chain subaccount state and snapshot the fields
        // we'll write into the slot header. Rent epoch is not captured —
        // the program reads it from the subaccount data area or from chain
        // when it needs it; the slot header doesn't carry it.
        let (subaccount_index, data_len, lamports, owner_bytes) = {
            let existing = invoke_context
                .transaction_context
                .find_index_of_subaccount(&subaccount_pubkey);
            let subaccount_index = if let Some(idx) = existing {
                idx
            } else {
                let on_chain_address = subaccount_address(&subaccount_pubkey);
                let (loaded, _slot) = invoke_context
                    .get_account_shared_data(&on_chain_address)
                    .unwrap_or_else(|| (AccountSharedData::default(), 0));
                let data_len_cost = (loaded.data().len() as u64)
                    .checked_div(invoke_context.get_execution_cost().cpi_bytes_per_unit)
                    .unwrap_or(u64::MAX);
                consume_compute_meter(invoke_context, data_len_cost)?;
                invoke_context
                    .transaction_context
                    .add_subaccount(subaccount_pubkey, loaded)?
            };
            let mut borrowed = invoke_context
                .transaction_context
                .accounts()
                .try_borrow_mut_subaccount(subaccount_index)?;
            // Tag as a subaccount so end-of-tx persistence stays in the
            // subaccount lane (see W8b-sdk).
            borrowed.set_subaccount_mark();
            let data_len = borrowed.data().len();
            let lamports = borrowed.lamports();
            let owner_bytes = *borrowed.owner();
            drop(borrowed);
            (subaccount_index, data_len, lamports, owner_bytes)
        };

        // Reject loading the same subaccount twice — that would alias two
        // writable views onto the same `AccountSharedData` storage and
        // CPI sync would have ambiguous metadata to verify against. Then
        // pick a free slot.
        let (slot_index, vm_header_addr, vm_data_addr) = {
            let syscall_context = invoke_context.get_syscall_context_mut()?;
            if syscall_context
                .subaccount_slots
                .iter()
                .any(|s| s.occupied_subaccount_index == Some(subaccount_index))
            {
                ic_msg!(
                    invoke_context,
                    "sol_load_subaccount: subaccount {} is already loaded",
                    subaccount_pubkey,
                );
                return Err(InstructionError::AccountAlreadyInitialized.into());
            }
            let Some((slot_index, slot)) = syscall_context
                .subaccount_slots
                .iter_mut()
                .enumerate()
                .find(|(_, slot)| slot.occupied_subaccount_index.is_none())
            else {
                ic_msg!(invoke_context, "sol_load_subaccount: all slots in use");
                return Err(InstructionError::MaxAccountsExceeded.into());
            };
            (slot_index, slot.vm_header_addr, slot.vm_data_addr)
        };

        // Stamp the slot header with the loaded subaccount's metadata.
        write_subaccount_slot_header(
            memory_mapping,
            check_aligned,
            vm_header_addr,
            &subaccount_pubkey,
            &owner_bytes,
            lamports,
            data_len,
            is_writable,
        )?;

        // Direct-map the slot's data region onto the live AccountSharedData
        // storage so program reads/writes hit the host buffer with zero copy.
        install_subaccount_data_region(
            invoke_context,
            memory_mapping,
            subaccount_index,
            vm_data_addr,
            is_writable,
        )?;

        // Build the metadata describing where each header field lives in VM
        // memory. CPI sync uses these addresses to flow state changes back
        // into the caller's view. The program is responsible for populating
        // its own `SolAccountInfo` (or equivalent) buffer using these
        // addresses; this syscall does NOT write into `proposed_view_addr` —
        // the pointer is stored opaquely on the slot for later sync.
        let metadata = SerializedAccountMetadata {
            original_data_len: data_len,
            vm_key_addr: vm_header_addr.saturating_add(SLOT_HEADER_OFFSET_KEY),
            vm_owner_addr: vm_header_addr.saturating_add(SLOT_HEADER_OFFSET_OWNER),
            vm_lamports_addr: vm_header_addr.saturating_add(SLOT_HEADER_OFFSET_LAMPORTS),
            vm_data_addr,
        };

        // Stash the metadata + opaque view pointer in the slot for later sync.
        let syscall_context = invoke_context.get_syscall_context_mut()?;
        if let Some(slot) = syscall_context.subaccount_slots.get_mut(slot_index) {
            slot.occupied_subaccount_index = Some(subaccount_index);
            slot.caller_account_view_addr = account_view_addr;
            slot.caller_account_metadata = Some(metadata);
            slot.is_writable = is_writable;
        }

        let header_out = translate_type_mut::<u64>(memory_mapping, out_header_addr, check_aligned)?;
        *header_out = vm_header_addr;

        load_subaccount_time.stop();
        invoke_context.timings.load_subaccounts_us += load_subaccount_time.as_us();

        Ok(SUCCESS)
    }
);

declare_builtin_function!(
    /// F10: release a subaccount slot previously populated by
    /// `sol_load_subaccount`. The data region was direct-mapped onto the host
    /// `AccountSharedData`, so any program writes are already in the host
    /// storage; this syscall only reconciles the header (lamports / owner /
    /// data_len) back into `AccountSharedData`, restores the slot's data
    /// region to the empty readonly placeholder, and zeros the header so a
    /// stale read after free can't observe prior state.
    ///
    /// The slot is identified by `vm_header_addr` — the value
    /// `sol_load_subaccount` returned through its out-pointer.
    SyscallUnloadSubaccount,
    fn rust(
        invoke_context: &mut InvokeContext,
        vm_header_addr: u64,
        _arg2: u64,
        _arg3: u64,
        _arg4: u64,
        _arg5: u64,
        memory_mapping: &mut MemoryMapping,
    ) -> Result<u64, Error> {
        let syscall_base_cost = invoke_context.get_execution_cost().syscall_base_cost;
        consume_compute_meter(invoke_context, syscall_base_cost)?;
        let check_aligned = invoke_context.get_check_aligned();

        let (slot_index, subaccount_index, vm_data_addr) = {
            let syscall_context = invoke_context.get_syscall_context()?;
            let entry = syscall_context
                .subaccount_slots
                .iter()
                .enumerate()
                .find(|(_, slot)| slot.vm_header_addr == vm_header_addr);
            let Some((slot_index, slot)) = entry else {
                ic_msg!(
                    invoke_context,
                    "sol_unload_subaccount: no slot at vm_header_addr {:#x}",
                    vm_header_addr,
                );
                return Err(InstructionError::InvalidArgument.into());
            };
            let Some(idx) = slot.occupied_subaccount_index else {
                ic_msg!(
                    invoke_context,
                    "sol_unload_subaccount: slot at {:#x} is not loaded",
                    vm_header_addr,
                );
                return Err(InstructionError::InvalidArgument.into());
            };
            (slot_index, idx, slot.vm_data_addr)
        };

        // Read header fields out of VM memory (lamports / owner / data_len).
        let owner_bytes: [u8; PUBKEY_BYTES] = {
            let owner_slice = translate_slice::<u8>(
                memory_mapping,
                vm_header_addr.saturating_add(SLOT_HEADER_OFFSET_OWNER),
                PUBKEY_BYTES as u64,
                check_aligned,
            )?;
            let mut bytes = [0u8; PUBKEY_BYTES];
            bytes.copy_from_slice(owner_slice);
            bytes
        };
        let lamports = *translate_type_mut::<u64>(
            memory_mapping,
            vm_header_addr.saturating_add(SLOT_HEADER_OFFSET_LAMPORTS),
            check_aligned,
        )?;
        let data_len_offset =
            (SUBACCOUNT_SLOT_HEADER_SIZE as u64).saturating_sub(std::mem::size_of::<u64>() as u64);
        let data_len = *translate_type_mut::<u64>(
            memory_mapping,
            vm_header_addr.saturating_add(data_len_offset),
            check_aligned,
        )? as usize;

        // Restore the data placeholder BEFORE mutating AccountSharedData so
        // we don't have an outstanding writable region whose len may go stale
        // when the underlying buffer reallocs on resize.
        restore_subaccount_data_placeholder(memory_mapping, vm_data_addr)?;

        let mut borrowed = invoke_context
            .transaction_context
            .accounts()
            .try_borrow_mut_subaccount(subaccount_index)?;
        if borrowed.lamports() != lamports {
            borrowed.set_lamports(lamports);
        }
        if borrowed.data().len() != data_len {
            borrowed.resize(data_len, 0);
        }
        let owner_pubkey = Pubkey::new_from_array(owner_bytes);
        if *borrowed.owner() != owner_pubkey {
            borrowed.set_owner(owner_pubkey);
        }
        drop(borrowed);

        // Zero the slot header so a stale read can't observe the prior key /
        // owner / lamports. Free the slot in the syscall context.
        write_subaccount_slot_header(
            memory_mapping,
            check_aligned,
            vm_header_addr,
            &Pubkey::default(),
            &Pubkey::default(),
            0,
            0,
            false,
        )?;
        let syscall_context = invoke_context.get_syscall_context_mut()?;
        if let Some(slot) = syscall_context
            .subaccount_slots
            .get_mut(slot_index)
        {
            slot.occupied_subaccount_index = None;
            slot.caller_account_view_addr = 0;
            slot.caller_account_metadata = None;
            slot.is_writable = false;
        }

        Ok(SUCCESS)
    }
);
