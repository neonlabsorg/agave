use solana_program_runtime::{invoke_context::OccupiedSubaccountIndex, memory::translate_vm_slice};
use solana_svm_measure::measure::Measure;
use solana_svm_timings::ExecuteTimings;

use {
    crate::{
        consume_compute_meter, translate_slice, translate_slice_mut, translate_type,
        translate_type_mut, Error, BPF_ALIGN_OF_U128,
    },
    solana_account::{AccountSharedData, ReadableAccount, WritableAccount},
    solana_account_info::AccountInfo,
    solana_instruction::error::InstructionError,
    solana_program_entrypoint::SUCCESS,
    solana_program_runtime::{
        cpi::{CallerAccount, SolAccountInfo},
        invoke_context::{AccountViewKind, InvokeContext, SerializedAccountMetadata},
        serialization::SUBACCOUNT_ACCOUNT_VIEW_RESERVED_SIZE,
    },
    solana_pubkey::{Pubkey, MAX_SEEDS, PUBKEY_BYTES},
    solana_sbpf::{
        declare_builtin_function,
        memory_region::{MemoryMapping, MemoryRegion},
    },
    solana_sdk_ids::system_program,
    solana_svm_log_collector::ic_msg,
    solana_system_interface::instruction::SystemInstruction,
    solana_transaction_context::{
        create_subaccount_address, subaccount_storage_address, vm_slice::VmSlice, IndexOfAccount,
        InstructionAccount, MAX_ACCOUNTS_PER_TRANSACTION, SUBACCOUNT_MARKER,
        transaction_accounts::SnapshotKey,
    },
};

/// The runtime reserves exactly [`SUBACCOUNT_ACCOUNT_VIEW_RESERVED_SIZE`] bytes
/// for the account-view buffer, so the `AccountInfo` written there by
/// [`load_subaccount`] must fit within that reservation.
const _: () = assert!(
    core::mem::size_of::<AccountInfo>() <= SUBACCOUNT_ACCOUNT_VIEW_RESERVED_SIZE,
    "AccountInfo does not fit within the reserved subaccount account-view buffer",
);

/// The runtime reserves exactly [`SUBACCOUNT_ACCOUNT_VIEW_RESERVED_SIZE`] bytes
/// for the account-view buffer, so the `SolAccountInfo` written there by
/// [`load_subaccount_c`] must fit within that reservation.
const _: () = assert!(
    core::mem::size_of::<SolAccountInfo>() <= SUBACCOUNT_ACCOUNT_VIEW_RESERVED_SIZE,
    "SolAccountInfo does not fit within the reserved subaccount account-view buffer",
);

const _: () = assert!(SUBACCOUNT_ACCOUNT_VIEW_RESERVED_SIZE % BPF_ALIGN_OF_U128 == 0);

/// F10: shared precondition for the entire subaccount syscall surface
/// (create / load / read / unload). Subaccount slots rely on direct-mapped
/// account data and the stricter ABI pointer checks, so every one of these
/// syscalls must refuse to run unless both features are active.
///
/// This must be the first thing each syscall does, before it mutates the
/// `TransactionContext`, issues a system `Transfer` CPI, or persists any
/// state — otherwise those side effects would happen under an ABI the rest of
/// the runtime does not expect. Returns `UnsupportedSysvar` when the
/// preconditions are not met.
fn check_subaccount_syscall_enabled(
    invoke_context: &mut InvokeContext,
    syscall_name: &str,
) -> Result<(), Error> {
    let feature_set = invoke_context.get_feature_set();
    let stricter_abi_and_runtime_constraints = feature_set.stricter_abi_and_runtime_constraints;
    let account_data_direct_mapping = feature_set.account_data_direct_mapping;
    if !(stricter_abi_and_runtime_constraints && account_data_direct_mapping) {
        ic_msg!(
            invoke_context,
            "{}: requires stricter ABI/runtime constraints && account data direct mapping features",
            syscall_name,
        );
        return Err(InstructionError::UnsupportedSysvar.into());
    }
    Ok(())
}

fn find_or_add_subaccount(
    invoke_context: &mut InvokeContext,
    subaccount_pubkey: Pubkey,
) -> Result<IndexOfAccount, Error> {
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
        let storage_address = subaccount_storage_address(&subaccount_pubkey);
        let (subaccount, _slot) = invoke_context
            .get_account_shared_data(&storage_address)
            .unwrap_or_else(|| (AccountSharedData::default(), 0));
        let data_len_cost = (subaccount.data().len() as u64)
            .checked_div(invoke_context.get_execution_cost().cpi_bytes_per_unit)
            .unwrap_or(u64::MAX);
        consume_compute_meter(invoke_context, data_len_cost)?;

        invoke_context
            .transaction_context
            .add_subaccount(subaccount_pubkey, subaccount)?
    };

    Ok(subaccount_index)
}

fn find_or_add_snapshot(
    invoke_context: &mut InvokeContext,
    snapshot_key: &SnapshotKey,
) -> Result<IndexOfAccount, Error> {
    let snapshot_index = if let Some(snapshot_index) = invoke_context
        .transaction_context
        .find_index_of_snapshot(snapshot_key)
    {
        snapshot_index
    } else {
        let storage_address = match snapshot_key {
            SnapshotKey::Account(pubkey) => pubkey,
            SnapshotKey::Subaccount(pubkey) => &subaccount_storage_address(pubkey),
        };
        let (snapshot, _slot) = invoke_context
            .get_account_shared_data_at_block_start(storage_address)
            .unwrap_or_else(|| (AccountSharedData::default(), 0));
        let data_len_cost = (snapshot.data().len() as u64)
            .checked_div(invoke_context.get_execution_cost().cpi_bytes_per_unit)
            .unwrap_or(u64::MAX);
        consume_compute_meter(invoke_context, data_len_cost)?;

        invoke_context
            .transaction_context
            .add_snapshot(snapshot_key, snapshot)?
    };

    Ok(snapshot_index)
}

// ============================================================================
// F10 — Subaccounts syscalls (PRS-153)
//
// This module implements the currently supported subaccount syscall surface:
// create, load, and unload.
// ============================================================================

declare_builtin_function!(
    /// F10: allocate a subaccount for the current program and return its index.
    SyscallCreateSubaccount,
    fn rust(
        invoke_context: &mut InvokeContext,
        payer_pubkey_addr: u64,
        seeds_addr: u64,
        seeds_len: u64,
        space: u64,
        lamports: u64,
        memory_mapping: &mut MemoryMapping,
    ) -> Result<u64, Error> {
        check_subaccount_syscall_enabled(invoke_context, "sol_create_subaccount")?;

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

        let subaccount_index = find_or_add_subaccount(invoke_context, subaccount_pubkey)?;
        // F10/PRS-314: a created subaccount must always be persisted by the
        // end-of-tx dirty filter, even a zero-lamport/zero-space allocation
        // that no later setter mutates (`set_data_length(0)` is a no-op and
        // with no funding transfer there is no lamport change to touch on).
        // Touch it here so persistence is correct by construction rather than
        // relying on a setter to remember. The load path leaves an unmodified
        // subaccount untouched; a writable load touches it once mutated (see
        // `commit_subaccount_slot`).
        invoke_context
            .transaction_context
            .accounts()
            .touch(subaccount_index | SUBACCOUNT_MARKER)?;
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

        // F10 host-pinning: if a slot currently owns this subaccount, detach
        // its writable data region before the `set_data_length` below may
        // relocate the underlying `Vec<u8>`. The guard is consumed by
        // `sync_subaccount_slot_after_mutation` at the end of this syscall,
        // which re-installs a fresh region against the (possibly-moved)
        // host buffer. On any `?` between here and that call, the guard
        // drops and the placeholder stays in the slot — safe for an aborted
        // instruction.
        let data_guard = SubaccountDataGuard::detach_if_loaded(
            invoke_context,
            memory_mapping,
            subaccount_index,
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

            subaccount.set_data_length(space as usize)?;
            subaccount.set_owner(&program_id.to_bytes())?;
        }
        invoke_context.transaction_context.pop()?;

        if lamports > 0 {
            // Fund the subaccount via a CPI into `system_program::Transfer`.
            // The subaccount is exposed to the callee as a regular
            // instruction account at callee-index 1 — its `index_in_transaction`
            // carries `SUBACCOUNT_MARKER`, so the runtime's
            // `try_borrow_instruction_account` → `try_borrow_mut` dispatch
            // routes the borrow into the subaccount lane (see
            // `transaction_accounts::try_borrow_mut`). The system_processor
            // therefore reads the subaccount just like any other writable
            // `BorrowedInstructionAccount` without lane-awareness.
            //
            // We bypass `native_invoke` / `prepare_next_instruction` because
            // the latter resolves `AccountMeta::pubkey` only through the
            // main account lane and would not find a subaccount pubkey. The
            // dedup_map similarly indexes only the main lane (its length is
            // `MAX_ACCOUNTS_PER_TRANSACTION`), so the subaccount tx-index is
            // intentionally absent from it.
            //
            // Privilege escalation is prevented by *propagating* the payer's
            // caller-side signer/writable bits verbatim into the callee
            // instruction account: by construction the callee cannot have
            // more privileges than the caller. If the caller granted
            // neither signer nor writable, `system_program::Transfer` will
            // refuse the operation with `MissingRequiredSignature` /
            // `ReadonlyLamportChange` itself.
            let payer_pubkey = *translate_type::<Pubkey>(
                memory_mapping,
                payer_pubkey_addr,
                check_aligned,
            )?;
            let payer_index_in_transaction = invoke_context
                .transaction_context
                .find_index_of_account(&payer_pubkey)
                .ok_or_else(|| {
                    ic_msg!(
                        invoke_context,
                        "Transfer: payer {} not in transaction",
                        payer_pubkey,
                    );
                    InstructionError::MissingAccount
                })?;
            let (payer_index_in_outer, payer_is_signer, payer_is_writable) = {
                let outer_ix_ctx = invoke_context
                    .transaction_context
                    .get_current_instruction_context()?;
                let payer_index_in_outer = outer_ix_ctx
                    .get_index_of_account_in_instruction(payer_index_in_transaction)
                    .map_err(|_| {
                        ic_msg!(
                            invoke_context,
                            "Transfer: payer {} not in instruction",
                            payer_pubkey,
                        );
                        InstructionError::MissingAccount
                    })?;
                (
                    payer_index_in_outer,
                    outer_ix_ctx.is_instruction_account_signer(payer_index_in_outer)?,
                    outer_ix_ctx.is_instruction_account_writable(payer_index_in_outer)?,
                )
            };

            let transfer_data = bincode::serialize(&SystemInstruction::Transfer { lamports })
                .map_err(|_| InstructionError::InvalidInstructionData)?;
            let mut dedup_map = vec![u16::MAX; MAX_ACCOUNTS_PER_TRANSACTION];
            *dedup_map
                .get_mut(payer_index_in_transaction as usize)
                .ok_or(InstructionError::MissingAccount)? = 0;
            invoke_context
                .transaction_context
                .configure_next_instruction(
                    system_program_index,
                    vec![
                        InstructionAccount::new(
                            payer_index_in_transaction,
                            payer_is_signer,
                            payer_is_writable,
                        ),
                        InstructionAccount::new_subaccount(
                            subaccount_index,
                            false, // is_signer — subaccounts can't sign
                            true,  // is_writable — funding mutates lamports
                        ),
                    ],
                    dedup_map,
                    std::borrow::Cow::Owned(transfer_data),
                )?;
            let mut compute_units_consumed = 0;
            invoke_context.process_instruction(
                &mut compute_units_consumed,
                &mut ExecuteTimings::default(),
            )?;

            // Sync the payer's new lamports back to the outer frame's VM
            // buffer. `system_program::Transfer` modified the runtime account
            // via `add_lamports_delta`, but the BPF input buffer (which the
            // outer instruction's `AccountInfo.lamports` points at) was
            // populated by `serialize_parameters` at the start of the
            // instruction and is now stale. Without this sync,
            // `deserialize_parameters` at end-of-instruction would observe a
            // mismatch (VM buffer = pre-transfer lamports, runtime =
            // post-transfer lamports) and call `set_lamports` on the runtime
            // account, applying the delta a second time and tripping
            // `UnbalancedInstruction` at the outer pop.
            let payer_lamports_after = invoke_context
                .transaction_context
                .get_current_instruction_context()?
                .try_borrow_instruction_account(payer_index_in_outer)?
                .get_lamports();
            let vm_lamports_addr = invoke_context
                .get_syscall_context()?
                .accounts_metadata
                .get(payer_index_in_outer as usize)
                .ok_or(InstructionError::MissingAccount)?
                .vm_lamports_addr;
            *translate_type_mut::<u64>(memory_mapping, vm_lamports_addr, check_aligned)? =
                payer_lamports_after;
        }

        // Mark the account as a subaccount via the SDK-side `rent_epoch`
        // sentinel (see parasol-fork-dev SDK commit 5407b64b / W8b-sdk).
        invoke_context
            .transaction_context
            .accounts()
            .try_borrow_mut_subaccount(subaccount_index)?
            .set_subaccount_mark();

        // Sync the freshly-created subaccount with any load_subaccount slot
        // that holds it. The slot's `vm_account_view_addr` points at the
        // runtime-reserved view buffer the program populated; the slot's
        // `caller_account_metadata` carries the field VM-addresses captured
        // at load time. Dispatched by `account_view_kind`,
        // `CallerAccount::from_account_info` / `from_sol_account_info`
        // verifies the program hasn't drifted those pointers and yields mut
        // handles to the lamports / owner fields the program reads. The data
        // region is re-installed via `data_guard` because
        // `set_data_length(space)` above may have reallocated the underlying
        // `AccountSharedData` buffer.
        sync_subaccount_slot_after_mutation(
            invoke_context,
            memory_mapping,
            check_aligned,
            subaccount_index,
            data_guard,
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

    // F10: mapping a subaccount's data region writable is the point at which
    // the program gains the ability to mutate it in place via direct mapping.
    // In-bounds direct-mapped stores hit `vm_to_host` directly and never run
    // the `access_violation_handler`, so they can't flip the touched flag on
    // their own (only realloc/CoW accesses route through the handler, which
    // does call `touch`). Mark the subaccount touched here so an in-place
    // overwrite of an existing subaccount (e.g. a writable `sol_load_subaccount`
    // followed by a same-length write) is still persisted at tx commit. A
    // read-only load takes the `new_readonly` branch above and is intentionally
    // left untouched so it is not re-stored. Mirrors the "writable ⇒ persist"
    // contract the runtime applies to main accounts.
    if is_writable {
        invoke_context
            .transaction_context
            .accounts()
            .touch(subaccount_index | SUBACCOUNT_MARKER)?;
    }
    Ok(())
}

/// Replaces the slot's data region with a **read-only** one backed by a
/// snapshot-lane entry's storage. Mirrors [`install_subaccount_data_region`]'s
/// read-only branch, but reads from the snapshot lane
/// ([`get_snapshot`](solana_transaction_context::transaction_accounts)) rather
/// than the subaccount lane — the two lanes use independent index spaces, and a
/// snapshot read never write-locks or persists, so it is always read-only and
/// must never be `touch`ed.
///
/// SAFETY: the host pointer captured in the `MemoryRegion` must remain valid
/// for the lifetime of the VM. The snapshot lane is append-only and its
/// `AccountSharedData` storage is pinned to the transaction context for the
/// whole instruction's duration, so the slice address is stable.
fn install_snapshot_data_region(
    invoke_context: &mut InvokeContext,
    memory_mapping: &mut MemoryMapping,
    snapshot_index: solana_transaction_context::IndexOfAccount,
    vm_data_addr: u64,
) -> Result<(), Error> {
    let snapshot = invoke_context
        .transaction_context
        .accounts()
        .get_snapshot(snapshot_index)?;
    let new_region = MemoryRegion::new_readonly(snapshot.data(), vm_data_addr);
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

/// RAII handle around the danger window between detaching the slot's writable
/// data region and re-installing it against the (possibly-relocated) host
/// `AccountSharedData::data` slice.
///
/// While the guard is alive the slot's data region is the empty readonly
/// placeholder, so a VM access cannot read through a stale host pointer if
/// the underlying `Vec<u8>` reallocates during the wrapped mutation.
///
/// Exits:
///   * [`reinstall`](Self::reinstall) — happy path: install a fresh region
///     against the current host slice; consumes the guard.
///   * [`dismiss`](Self::dismiss) — explicitly drop without re-installing;
///     used when the slot is being freed (e.g. unload).
///   * implicit `Drop` — same as `dismiss`. Deliberately a no-op so that an
///     error-path unwind cannot silently re-install a region whose backing
///     slice may already be borrowed elsewhere on the stack.
#[must_use = "SubaccountDataGuard must be reinstalled or explicitly dismissed"]
struct SubaccountDataGuard {
    subaccount_index: solana_transaction_context::IndexOfAccount,
    vm_data_addr: u64,
    is_writable: bool,
}

impl SubaccountDataGuard {
    /// Detach the data region for `subaccount_index` if any slot currently
    /// owns it. Returns `Ok(None)` when no slot owns the subaccount — the
    /// caller can mutate freely with no re-install obligation.
    fn detach_if_loaded(
        invoke_context: &InvokeContext,
        memory_mapping: &mut MemoryMapping,
        subaccount_index: solana_transaction_context::IndexOfAccount,
    ) -> Result<Option<Self>, Error> {
        let slot_info = {
            let syscall_context = invoke_context.get_syscall_context()?;
            syscall_context
                .subaccount_slots
                .iter()
                .find(|s| s.occupied_subaccount_index == OccupiedSubaccountIndex::Subaccount(subaccount_index))
                .map(|s| (s.vm_data_addr, s.is_writable))
        };
        let Some((vm_data_addr, is_writable)) = slot_info else {
            return Ok(None);
        };
        restore_subaccount_data_placeholder(memory_mapping, vm_data_addr)?;
        Ok(Some(Self {
            subaccount_index,
            vm_data_addr,
            is_writable,
        }))
    }

    /// VM address of the slot's data region (captured at detach time).
    fn vm_data_addr(&self) -> u64 {
        self.vm_data_addr
    }

    /// Re-install the slot's data region against the current
    /// `AccountSharedData::data_as_mut_slice()`. Consumes the guard.
    fn reinstall(
        self,
        invoke_context: &mut InvokeContext,
        memory_mapping: &mut MemoryMapping,
    ) -> Result<(), Error> {
        install_subaccount_data_region(
            invoke_context,
            memory_mapping,
            self.subaccount_index,
            self.vm_data_addr,
            self.is_writable,
        )
    }

    /// Discards the guard without re-installing. The slot's data region
    /// stays as the readonly placeholder.
    fn dismiss(self) {}
}

/// Syncs a subaccount's freshly-mutated `AccountSharedData` state back into
/// the matching `SubaccountSlot` (if any). Called from `SyscallCreateSubaccount`
/// after a `system_program::Allocate` push/pop frame mutates the subaccount.
///
/// `data_guard` must have been acquired BEFORE the mutation (see
/// [`SubaccountDataGuard::detach_if_loaded`]) so the slot's data region was
/// detached while the underlying `Vec<u8>` may relocate. The guard is
/// consumed at the end to re-install the region against the moved buffer.
/// A `None` guard means no slot owned the subaccount at detach time —
/// nothing to sync.
///
/// Verification: the program's account-view pointers (in the program-written
/// `AccountInfo` / `SolAccountInfo` at `slot.vm_account_view_addr`) must
/// match the field-addresses captured in `slot.caller_account_metadata` at
/// load time. The slot's `account_view_kind` selects between
/// [`CallerAccount::from_account_info`] (Rust SDK) and
/// [`CallerAccount::from_sol_account_info`] (C ABI); both perform the
/// pointer check and yield mut handles to the VM-side fields.
///
/// State propagation:
///   1. Slot header lamports / owner — updated through the CallerAccount
///      mut handles (which point at `slot_header[72..80]` / `slot_header[40..72]`
///      via the program-written view's pointer fields).
///   2. Program-side `AccountInfo` / `SolAccountInfo` `data_len` — written
///      through `caller_account.ref_to_len_in_vm`.
///   3. Slot header `data_len` field at `vm_data_addr - 8` (== slot header
///      offset 80).
///   4. Slot data region — re-installed via the guard, since `set_data_length`
///      may have reallocated the underlying `AccountSharedData` storage.
fn sync_subaccount_slot_after_mutation(
    invoke_context: &mut InvokeContext,
    memory_mapping: &mut MemoryMapping,
    check_aligned: bool,
    subaccount_index: solana_transaction_context::IndexOfAccount,
    data_guard: Option<SubaccountDataGuard>,
) -> Result<(), Error> {
    // No slot owned this subaccount at detach time → no view to sync.
    let Some(data_guard) = data_guard else {
        return Ok(());
    };
    let vm_data_addr = data_guard.vm_data_addr();

    // Look up the matching slot's view metadata. `vm_data_addr` /
    // `is_writable` come from the guard, captured at detach time and stable
    // for the slot's lifetime.
    let Some((view_addr, kind, metadata)) = ({
        let syscall_context = invoke_context.get_syscall_context()?;
        syscall_context
            .subaccount_slots
            .iter()
            .find(|s| s.occupied_subaccount_index == OccupiedSubaccountIndex::Subaccount(subaccount_index))
            .and_then(
                |s| match (s.caller_account_metadata.as_ref(), s.account_view_kind) {
                    (Some(m), Some(kind)) => Some((s.vm_account_view_addr, kind, m.clone())),
                    _ => None,
                },
            )
    }) else {
        // Defensive: slot was found at detach time but its view metadata is
        // missing now. Re-install the region anyway so VM access continues
        // to work; skip header sync.
        return data_guard.reinstall(invoke_context, memory_mapping);
    };

    // Read fresh state from the AccountSharedData storage.
    let (lamports, owner, data_len) = {
        let borrowed = invoke_context
            .transaction_context
            .accounts()
            .try_borrow_subaccount(subaccount_index)?;
        (
            borrowed.lamports(),
            *borrowed.owner(),
            borrowed.data().len(),
        )
    };

    // Build a CallerAccount from the program-written view, dispatched by the
    // ABI the loader recorded for this slot. Either path verifies (under
    // `stricter_abi_and_runtime_constraints`) that the program hasn't moved
    // the field pointers since load time.
    {
        let caller_account = match kind {
            AccountViewKind::Rust => {
                let view = translate_type::<AccountInfo>(memory_mapping, view_addr, check_aligned)?;
                CallerAccount::from_account_info(
                    invoke_context,
                    memory_mapping,
                    check_aligned,
                    view_addr,
                    view,
                    &metadata,
                )?
            }
            AccountViewKind::C => {
                let view =
                    translate_type::<SolAccountInfo>(memory_mapping, view_addr, check_aligned)?;
                CallerAccount::from_sol_account_info(
                    invoke_context,
                    memory_mapping,
                    check_aligned,
                    view_addr,
                    view,
                    &metadata,
                )?
            }
        };

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
    // host buffer. Consumes the guard.
    data_guard.reinstall(invoke_context, memory_mapping)
}

/// Translate the VM seed array into host slices. Shared by the base-checked
/// load/read path and the base-free snapshot path.
fn translate_subaccount_seed_slices<'a>(
    seeds_addr: u64,
    seeds_len: u64,
    memory_mapping: &'a MemoryMapping,
    check_aligned: bool,
) -> Result<Vec<&'a [u8]>, Error> {
    let untranslated_seeds =
        translate_slice::<VmSlice<u8>>(memory_mapping, seeds_addr, seeds_len, check_aligned)?;
    if untranslated_seeds.len() > MAX_SEEDS {
        return Err(Box::new(InstructionError::MaxSeedLengthExceeded));
    }
    untranslated_seeds
        .iter()
        .map(|untranslated_seed| {
            translate_vm_slice(untranslated_seed, memory_mapping, check_aligned)
        })
        .collect::<Result<Vec<_>, Error>>()
}

/// Translate seeds and derive the subaccount address **without** requiring the
/// base account (the first seed) to be present in the transaction. Used by the
/// read-only `sol_load_subaccount_snapshot` syscalls, which never write-lock a
/// base account because the load is read-only.
fn translate_subaccount_seeds_no_base(
    program_id: &Pubkey,
    seeds_addr: u64,
    seeds_len: u64,
    memory_mapping: &MemoryMapping,
    check_aligned: bool,
) -> Result<Pubkey, Error> {
    let seeds =
        translate_subaccount_seed_slices(seeds_addr, seeds_len, memory_mapping, check_aligned)?;
    let subaccount_pubkey =
        create_subaccount_address(&seeds, program_id).map_err(|_| InstructionError::InvalidSeeds)?;
    Ok(subaccount_pubkey)
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
    let seeds =
        translate_subaccount_seed_slices(seeds_addr, seeds_len, memory_mapping, check_aligned)?;
    let base_seed: [u8; 32] = (*seeds.first().ok_or(InstructionError::InvalidArgument)?)
        .try_into()
        .map_err(|err: std::array::TryFromSliceError| {
            ic_msg!(
                invoke_context,
                "Invalid base account seed length: {:?}",
                err
            );
            InstructionError::InvalidArgument
        })?;
    let base_pubkey = Pubkey::new_from_array(base_seed);
    let base_index_in_transaction = invoke_context
        .transaction_context
        .find_index_of_account(&base_pubkey)
        .ok_or_else(|| {
            ic_msg!(
                invoke_context,
                "Base account {} not in transaction",
                base_pubkey
            );
            InstructionError::InvalidArgument
        })?;
    let base_index_in_instruction = instruction_context
        .get_index_of_account_in_instruction(base_index_in_transaction)
        .map_err(|_| {
            ic_msg!(
                invoke_context,
                "Base account {} not in instruction",
                base_pubkey
            );
            InstructionError::InvalidArgument
        })?;

    let is_writable = instruction_context
        .is_instruction_account_writable(base_index_in_instruction)
        .map_err(|_| {
            ic_msg!(
                invoke_context,
                "Can't get writable for base account {} ",
                base_pubkey
            );
            InstructionError::InvalidArgument
        })?;

    let subaccount_pubkey = create_subaccount_address(&seeds, program_id)
        .map_err(|_| InstructionError::InvalidSeeds)?;

    Ok((subaccount_pubkey, is_writable))
}

/// Shared body of the `sol_load_subaccount_{rust,c}` syscalls.
///
/// Loads an on-chain subaccount into a pre-reserved VM slot, with the slot's
/// data region direct-mapped onto the live `AccountSharedData`.
///
/// The two syscall surfaces differ only in `kind`: the runtime stores the
/// caller's source-language ABI on the slot so a subsequent CPI sync uses
/// the matching `CallerAccount::from_*` decoder. All other behavior —
/// seed translation, on-chain load, slot allocation, header stamping, data
/// region install, metadata capture, and out-pointer writes — is shared.
///
/// `out_account_view_addr` / `out_header_addr` are VM out-pointers. On
/// success the slot's stable view-buffer address (in MM_INPUT, immediately
/// before the slot header, sized to fit either `AccountInfo<'_>` or
/// `SolAccountInfo`) is written to `*out_account_view_addr`, and the slot's
/// stable `vm_header_addr` is written to `*out_header_addr`.
/// `sol_unload_subaccount` takes the `vm_header_addr` to release.
///
/// Loading the same subaccount twice in a single invocation is rejected
/// with [`InstructionError::AccountAlreadyInitialized`].
fn load_subaccount_impl(
    invoke_context: &mut InvokeContext,
    seeds_addr: u64,
    seeds_len: u64,
    out_account_view_addr: u64,
    out_header_addr: u64,
    memory_mapping: &mut MemoryMapping,
    kind: AccountViewKind,
) -> Result<u64, Error> {
    check_subaccount_syscall_enabled(invoke_context, "sol_load_subaccount")?;

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
        let subaccount_index = find_or_add_subaccount(invoke_context, subaccount_pubkey)?;
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
    let (slot_index, vm_header_addr, vm_data_addr, vm_account_view_addr) = {
        let syscall_context = invoke_context.get_syscall_context_mut()?;
        if syscall_context
            .subaccount_slots
            .iter()
            .any(|s| s.occupied_subaccount_index == OccupiedSubaccountIndex::Subaccount(subaccount_index))
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
            .find(|(_, slot)| slot.occupied_subaccount_index.is_empty())
        else {
            ic_msg!(invoke_context, "sol_load_subaccount: all slots in use");
            return Err(InstructionError::MaxAccountsExceeded.into());
        };
        (
            slot_index,
            slot.vm_header_addr,
            slot.vm_data_addr,
            slot.vm_account_view_addr,
        )
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
    // into the program-written view at `vm_account_view_addr`.
    let metadata = SerializedAccountMetadata {
        original_data_len: data_len,
        vm_key_addr: vm_header_addr.saturating_add(SLOT_HEADER_OFFSET_KEY),
        vm_owner_addr: vm_header_addr.saturating_add(SLOT_HEADER_OFFSET_OWNER),
        vm_lamports_addr: vm_header_addr.saturating_add(SLOT_HEADER_OFFSET_LAMPORTS),
        vm_data_addr,
    };

    // Stash the metadata + ABI kind on the slot for later sync.
    let syscall_context = invoke_context.get_syscall_context_mut()?;
    let header_out = translate_type_mut::<u64>(memory_mapping, out_header_addr, check_aligned)?;
    let view_out = translate_type_mut::<u64>(memory_mapping, out_account_view_addr, check_aligned)?;

    if let Some(slot) = syscall_context.subaccount_slots.get_mut(slot_index) {
        slot.occupied_subaccount_index = OccupiedSubaccountIndex::Subaccount(subaccount_index);
        slot.caller_account_metadata = Some(metadata);
        slot.account_view_kind = Some(kind);
        slot.is_writable = is_writable;
    }
    *header_out = vm_header_addr;
    *view_out = vm_account_view_addr;

    load_subaccount_time.stop();
    invoke_context.timings.load_subaccounts_us += load_subaccount_time.as_us();

    Ok(SUCCESS)
}

declare_builtin_function!(
    /// F10: load an on-chain subaccount into a pre-reserved VM slot, treating
    /// the slot's reserved view buffer as a Rust SDK
    /// [`solana_account_info::AccountInfo`]. See [`load_subaccount_impl`] for
    /// the shared semantics.
    SyscallLoadSubaccountRust,
    fn rust(
        invoke_context: &mut InvokeContext,
        seeds_addr: u64,
        seeds_len: u64,
        out_account_view_addr: u64,
        out_header_addr: u64,
        _arg5: u64,
        memory_mapping: &mut MemoryMapping,
    ) -> Result<u64, Error> {
        load_subaccount_impl(
            invoke_context,
            seeds_addr,
            seeds_len,
            out_account_view_addr,
            out_header_addr,
            memory_mapping,
            AccountViewKind::Rust,
        )
    }
);

declare_builtin_function!(
    /// F10: load an on-chain subaccount into a pre-reserved VM slot, treating
    /// the slot's reserved view buffer as a C-ABI
    /// [`solana_program_runtime::cpi::SolAccountInfo`]. See
    /// [`load_subaccount_impl`] for the shared semantics.
    SyscallLoadSubaccountC,
    fn rust(
        invoke_context: &mut InvokeContext,
        seeds_addr: u64,
        seeds_len: u64,
        out_account_view_addr: u64,
        out_header_addr: u64,
        _arg5: u64,
        memory_mapping: &mut MemoryMapping,
    ) -> Result<u64, Error> {
        load_subaccount_impl(
            invoke_context,
            seeds_addr,
            seeds_len,
            out_account_view_addr,
            out_header_addr,
            memory_mapping,
            AccountViewKind::C,
        )
    }
);

/// Shared body of the read-only `sol_load_subaccount_snapshot` and
/// `sol_load_account_snapshot` syscalls.
///
/// Like [`load_subaccount_impl`], but:
///   - it does **not** require a base account in the transaction — the address
///     is derived from the seeds alone ([`translate_subaccount_seeds_no_base`]),
///     because a read-only load never write-locks a base account;
///   - the slot is always **read-only** (`is_writable == false`), so the data
///     region is mapped read-only and the entry is never touched/persisted; and
///   - the on-chain state is read **as of the beginning of the block**
///     (parent-slot state) via `get_account_shared_data_at_block_start`, then
///     stored in a fresh, deduplicated lane entry ([`add_snapshot`]) so the 
///     snapshot is independent of any mid-block load of the same subaccount.
///
/// The slot is released by the same `sol_unload_subaccount`.
fn load_snapshot_impl(
    invoke_context: &mut InvokeContext,
    out_header_addr: u64,
    memory_mapping: &mut MemoryMapping,
    get_snapshot_key: impl FnOnce(&mut InvokeContext, &mut MemoryMapping) -> Result<SnapshotKey, Error>,
) -> Result<u64, Error> {
    check_subaccount_syscall_enabled(invoke_context, "sol_load_subaccount_snapshot")?;

    let mut load_subaccount_time = Measure::start("load_subaccount");
    let syscall_base_cost = invoke_context.get_execution_cost().syscall_base_cost;
    consume_compute_meter(invoke_context, syscall_base_cost)?;
    let check_aligned = invoke_context.get_check_aligned();

    let mut compute_subaccounts_time = Measure::start("compute_subaccounts");
    let snapshot_key = get_snapshot_key(invoke_context, memory_mapping)?;
    compute_subaccounts_time.stop();
    invoke_context.timings.compute_subaccounts_us += compute_subaccounts_time.as_us();
    let is_writable = false;  // snapshot loads are always read-only

    // Load the start-of-block on-chain state and register it in a fresh
    // non-deduplicated lane entry so it is independent of any mid-block load of
    // the same subaccount. Snapshot a copy of the header fields for the slot.
    let (subaccount_index, data_len, lamports, owner_bytes) = {
        let snapshot_index = find_or_add_snapshot(invoke_context, &snapshot_key)?;
        let snapshot = invoke_context
            .transaction_context
            .accounts()
            .get_snapshot(snapshot_index)?;
        let data_len = snapshot.data().len();
        let lamports = snapshot.lamports();
        let owner_bytes = *snapshot.owner();
        (snapshot_index, data_len, lamports, owner_bytes)
    };

    // Pick a free slot. The index-based "already loaded" guard from
    // `load_subaccount_impl` is intentionally omitted: each snapshot uses a
    // fresh index and is read-only, so there is no writable-alias hazard from
    // loading the same subaccount twice.
    let (slot_index, vm_header_addr, vm_data_addr) = {
        let syscall_context = invoke_context.get_syscall_context_mut()?;
        if syscall_context
            .subaccount_slots
            .iter()
            .any(|s| s.occupied_subaccount_index == OccupiedSubaccountIndex::Snapshot(subaccount_index))
        {
            ic_msg!(
                invoke_context,
                "sol_load_subaccount_snapshot: snapshot {} is already loaded",
                snapshot_key.as_pubkey(),
            );
            return Err(InstructionError::AccountAlreadyInitialized.into());
        }
        let Some((slot_index, slot)) = syscall_context
            .subaccount_slots
            .iter_mut()
            .enumerate()
            .find(|(_, slot)| slot.occupied_subaccount_index.is_empty())
        else {
            ic_msg!(invoke_context, "sol_load_subaccount_snapshot: all slots in use");
            return Err(InstructionError::MaxAccountsExceeded.into());
        };
        (
            slot_index,
            slot.vm_header_addr,
            slot.vm_data_addr,
        )
    };

    // Stamp the slot header with the loaded subaccount's metadata.
    write_subaccount_slot_header(
        memory_mapping,
        check_aligned,
        vm_header_addr,
        snapshot_key.as_pubkey(),
        &owner_bytes,
        lamports,
        data_len,
        is_writable,
    )?;

    // Map the slot's data region read-only onto the snapshot-lane storage.
    // Snapshot reads use the independent snapshot lane (not the subaccount
    // lane), are always read-only, and are never touched/persisted.
    install_snapshot_data_region(
        invoke_context,
        memory_mapping,
        subaccount_index,
        vm_data_addr,
    )?;

    let metadata = SerializedAccountMetadata {
        original_data_len: data_len,
        vm_key_addr: vm_header_addr.saturating_add(SLOT_HEADER_OFFSET_KEY),
        vm_owner_addr: vm_header_addr.saturating_add(SLOT_HEADER_OFFSET_OWNER),
        vm_lamports_addr: vm_header_addr.saturating_add(SLOT_HEADER_OFFSET_LAMPORTS),
        vm_data_addr,
    };

    let syscall_context = invoke_context.get_syscall_context_mut()?;
    let header_out = translate_type_mut::<u64>(memory_mapping, out_header_addr, check_aligned)?;

    if let Some(slot) = syscall_context.subaccount_slots.get_mut(slot_index) {
        slot.occupied_subaccount_index = OccupiedSubaccountIndex::Snapshot(subaccount_index);
        slot.caller_account_metadata = Some(metadata);
        slot.account_view_kind = None; // snapshot loads don't have a program-written view because subaccounts are not changed
        slot.is_writable = false;
    }
    *header_out = vm_header_addr;

    load_subaccount_time.stop();
    invoke_context.timings.load_subaccounts_us += load_subaccount_time.as_us();

    Ok(SUCCESS)
}

declare_builtin_function!(
    /// F10: read-only, base-free, start-of-block load of a subaccount into a
    /// pre-reserved VM slot. See [`load_subaccount_snapshot_impl`] for the shared semantics.
    SyscallLoadSubaccountSnapshot,
    fn rust(
        invoke_context: &mut InvokeContext,
        seeds_addr: u64,
        seeds_len: u64,
        out_header_addr: u64,
        _arg4: u64,
        _arg5: u64,
        memory_mapping: &mut MemoryMapping,
    ) -> Result<u64, Error> {
        load_snapshot_impl(
            invoke_context,
            out_header_addr,
            memory_mapping,
            |invoke_context, memory_mapping| {
                // Translate seeds → derive PDA. Read-only snapshot loads never require a
                // base account, so writability is unconditionally false.
                let instruction_context = invoke_context
                    .transaction_context
                    .get_current_instruction_context()?;
                let program_id = *instruction_context.get_program_key()?;
                let snapshot_pubkey = translate_subaccount_seeds_no_base(
                    &program_id,
                    seeds_addr,
                    seeds_len,
                    memory_mapping,
                    invoke_context.get_check_aligned(),
                )?;
                Ok(SnapshotKey::Subaccount(snapshot_pubkey))
            }
        )
    }
);

declare_builtin_function!(
    /// F10: read-only, base-free, start-of-block load of a account into a
    /// pre-reserved VM slot. See [`load_snapshot_impl`] for the shared semantics.
    SyscallLoadAccountSnapshot,
    fn rust(
        invoke_context: &mut InvokeContext,
        pubkey_addr: u64,
        out_header_addr: u64,
        _arg3: u64,
        _arg4: u64,
        _arg5: u64,
        memory_mapping: &mut MemoryMapping,
    ) -> Result<u64, Error> {
        load_snapshot_impl(
            invoke_context,
            out_header_addr,
            memory_mapping,
            |invoke_context, memory_mapping| {
                let pubkey = *translate_type::<Pubkey>(
                    memory_mapping,
                    pubkey_addr,
                    invoke_context.get_check_aligned(),
                )?;
                Ok(SnapshotKey::Account(pubkey))
            }
        )
    }
);

declare_builtin_function!(
    /// F10: read data from a subaccount without loading it into a slot
    SyscallReadSubaccount,
    fn rust(
        invoke_context: &mut InvokeContext,
        seeds_addr: u64,
        seeds_len: u64,
        buff: u64,
        offset: u64,
        length: u64,
        memory_mapping: &mut MemoryMapping,
    ) -> Result<u64, Error> {
        check_subaccount_syscall_enabled(invoke_context, "sol_read_subaccount")?;

        // We use `load_subaccount` measure here to capture the time spent on loading subaccount
        // from the AccountsDB.
        let mut load_subaccount_time = Measure::start("load_subaccount");
        let syscall_base_cost = invoke_context.get_execution_cost().syscall_base_cost;
        consume_compute_meter(invoke_context, syscall_base_cost)?;
        let check_aligned = invoke_context.get_check_aligned();

        let mut compute_subaccounts_time = Measure::start("compute_subaccounts");
        // Translate seeds → derive PDA → inherit base account writable bit.
        let (subaccount_pubkey, _) = {
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

        let subaccount_index = find_or_add_subaccount(invoke_context, subaccount_pubkey)?;
        let borrowed = invoke_context
            .transaction_context
            .accounts()
            .try_borrow_subaccount(subaccount_index)?;

        let data = borrowed.data();
        let start = offset as usize;
        let end = start
            .checked_add(length as usize)
            .ok_or(InstructionError::InvalidArgument)?;
        let src = data
            .get(start..end)
            .ok_or(InstructionError::InvalidArgument)?;
        let dst = translate_slice_mut::<u8>(memory_mapping, buff, length, check_aligned)?;
        dst.copy_from_slice(src);

        // Charge for copying `length` bytes into guest memory.
        let compute_cost = invoke_context.get_execution_cost();
        let copy_cost = compute_cost.mem_op_base_cost.max(
            length
                .checked_div(compute_cost.cpi_bytes_per_unit)
                .unwrap_or(u64::MAX),
        );
        consume_compute_meter(invoke_context, copy_cost)?;

        load_subaccount_time.stop();
        invoke_context.timings.load_subaccounts_us += load_subaccount_time.as_us();

        Ok(0)
    }
);

declare_builtin_function!(
    /// F10: release a subaccount slot previously populated by either
    /// `sol_load_subaccount_rust` or `sol_load_subaccount_c`. The data region
    /// was direct-mapped onto the host `AccountSharedData`, so any program
    /// writes are already in the host storage; this syscall only reconciles
    /// the header (lamports / owner / data_len) back into `AccountSharedData`,
    /// restores the slot's data region to the empty readonly placeholder,
    /// and zeros the header so a stale read after free can't observe prior
    /// state.
    ///
    /// The slot is identified by `vm_header_addr` — the value the load
    /// syscall returned through its `out_header_addr` out-pointer.
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
        check_subaccount_syscall_enabled(invoke_context, "sol_unload_subaccount")?;

        let syscall_base_cost = invoke_context.get_execution_cost().syscall_base_cost;
        consume_compute_meter(invoke_context, syscall_base_cost)?;
        let check_aligned = invoke_context.get_check_aligned();

        let (slot_index, occupied, is_writable, vm_data_addr) = {
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
            if slot.occupied_subaccount_index.is_empty() {
                ic_msg!(
                    invoke_context,
                    "sol_unload_subaccount: slot at {:#x} is not loaded",
                    vm_header_addr,
                );
                return Err(InstructionError::InvalidArgument.into());
            }
            (
                slot_index,
                slot.occupied_subaccount_index,
                slot.is_writable,
                slot.vm_data_addr,
            )
        };

        // Read-only snapshot slots never mutated any lane and were direct-mapped
        // read-only, so there is nothing to reconcile back into storage. Just
        // restore the data placeholder, zero the header, and free the slot.
        if let OccupiedSubaccountIndex::Snapshot(_) = occupied {
            restore_subaccount_data_placeholder(memory_mapping, vm_data_addr)?;
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
            if let Some(slot) = syscall_context.subaccount_slots.get_mut(slot_index) {
                slot.occupied_subaccount_index = OccupiedSubaccountIndex::Empty;
                slot.caller_account_metadata = None;
                slot.account_view_kind = None;
                slot.is_writable = false;
            }
            return Ok(SUCCESS);
        }

        let subaccount_index = occupied
            .get_subaccount_index()
            .ok_or(InstructionError::InvalidArgument)?;

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
        let new_data_len = *translate_type_mut::<u64>(
            memory_mapping,
            vm_header_addr.saturating_add(data_len_offset),
            check_aligned,
        )? as usize;

        // F10 host-pinning: detach the slot's data region BEFORE mutating
        // `AccountSharedData` so we don't leave a writable region pointing
        // at a buffer that the upcoming `resize` may reallocate. The slot
        // is being freed below, so the guard is explicitly dismissed —
        // never re-installed — leaving the readonly placeholder in place.
        let data_guard = SubaccountDataGuard::detach_if_loaded(
            invoke_context,
            memory_mapping,
            subaccount_index,
        )?;

        let instruction_context = invoke_context
            .transaction_context
            .get_current_instruction_context()?;
        let mut borrowed = instruction_context
            .try_borrow_subaccount_by_tx_index(subaccount_index, is_writable)?;

        if borrowed.get_lamports() != lamports {
            borrowed.set_lamports(lamports)?;
        }
        if borrowed.get_data().len() != new_data_len {
            borrowed.set_data_length(new_data_len)?;
        }
        if *borrowed.get_owner() != Pubkey::new_from_array(owner_bytes) {
            borrowed.set_owner(&owner_bytes)?;
        }
        drop(borrowed);

        if let Some(guard) = data_guard {
            guard.dismiss();
        }

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
            slot.occupied_subaccount_index = OccupiedSubaccountIndex::Empty;
            slot.caller_account_metadata = None;
            slot.account_view_kind = None;
            slot.is_writable = false;
        }

        Ok(SUCCESS)
    }
);
