use {
    super::*,
    solana_account_info::AccountInfo,
    solana_instruction::Instruction,
    solana_program_runtime::cpi::{
        cpi_common, translate_account_infos, translate_accounts_c, translate_accounts_rust,
        translate_instruction_c, translate_instruction_rust, translate_signers_c,
        translate_signers_rust, translate_subaccount_slots, translate_subaccounts_common,
        CallerAccount, SolAccountInfo, SyscallInvokeSigned, TranslatedAccount,
    },
};

declare_builtin_function!(
    /// Cross-program invocation called from Rust
    SyscallInvokeSignedRust,
    fn rust(
        invoke_context: &mut InvokeContext,
        instruction_addr: u64,
        account_infos_addr: u64,
        account_infos_len: u64,
        signers_seeds_addr: u64,
        signers_seeds_len: u64,
        memory_mapping: &mut MemoryMapping,
    ) -> Result<u64, Error> {
        cpi_common::<Self>(
            invoke_context,
            instruction_addr,
            account_infos_addr,
            account_infos_len,
            signers_seeds_addr,
            signers_seeds_len,
            memory_mapping,
            Vec::new(),
        )
    }
);

impl SyscallInvokeSigned for SyscallInvokeSignedRust {
    fn translate_instruction(
        addr: u64,
        memory_mapping: &MemoryMapping,
        invoke_context: &mut InvokeContext,
        check_aligned: bool,
    ) -> Result<Instruction, Error> {
        translate_instruction_rust(addr, memory_mapping, invoke_context, check_aligned)
    }

    fn translate_accounts<'a>(
        account_infos_addr: u64,
        account_infos_len: u64,
        memory_mapping: &MemoryMapping<'_>,
        invoke_context: &mut InvokeContext,
        check_aligned: bool,
    ) -> Result<Vec<TranslatedAccount<'a>>, Error> {
        translate_accounts_rust(
            account_infos_addr,
            account_infos_len,
            memory_mapping,
            invoke_context,
            check_aligned,
        )
    }

    fn translate_subaccounts<'a>(
        subaccount_infos_addr: u64,
        subaccount_infos_len: u64,
        memory_mapping: &MemoryMapping<'_>,
        invoke_context: &mut InvokeContext,
        check_aligned: bool,
    ) -> Result<Vec<TranslatedAccount<'a>>, Error> {
        let (subaccount_infos, _keys) = translate_account_infos(
            subaccount_infos_addr,
            subaccount_infos_len,
            |subaccount_info: &AccountInfo| subaccount_info.key as *const _ as u64,
            memory_mapping,
            invoke_context,
            check_aligned,
        )?;
        let mut subaccounts = translate_subaccounts_common(
            subaccount_infos,
            subaccount_infos_addr,
            invoke_context,
            memory_mapping,
            check_aligned,
            CallerAccount::from_account_info,
        )?;
        // F10: append `sol_load_subaccount` slot entries so the CPI machinery
        // syncs them across the call boundary. Pre-CPI sync (program view →
        // AccountSharedData) runs inside `translate_subaccount_slots`; post-CPI
        // sync runs through `cpi_common` via `subaccount_slot` dispatch.
        let mut slot_entries = translate_subaccount_slots::<AccountInfo, _>(
            invoke_context,
            memory_mapping,
            check_aligned,
            CallerAccount::from_account_info,
        )?;
        subaccounts.append(&mut slot_entries);
        Ok(subaccounts)
    }

    fn translate_signers(
        program_id: &Pubkey,
        signers_seeds_addr: u64,
        signers_seeds_len: u64,
        memory_mapping: &MemoryMapping,
        check_aligned: bool,
    ) -> Result<Vec<Pubkey>, Error> {
        translate_signers_rust(
            program_id,
            signers_seeds_addr,
            signers_seeds_len,
            memory_mapping,
            check_aligned,
        )
    }
}

declare_builtin_function!(
    /// Cross-program invocation called from C
    SyscallInvokeSignedC,
    fn rust(
        invoke_context: &mut InvokeContext,
        instruction_addr: u64,
        account_infos_addr: u64,
        account_infos_len: u64,
        signers_seeds_addr: u64,
        signers_seeds_len: u64,
        memory_mapping: &mut MemoryMapping,
    ) -> Result<u64, Error> {
        cpi_common::<Self>(
            invoke_context,
            instruction_addr,
            account_infos_addr,
            account_infos_len,
            signers_seeds_addr,
            signers_seeds_len,
            memory_mapping,
            Vec::new(),
        )
    }
);

impl SyscallInvokeSigned for SyscallInvokeSignedC {
    fn translate_instruction(
        addr: u64,
        memory_mapping: &MemoryMapping,
        invoke_context: &mut InvokeContext,
        check_aligned: bool,
    ) -> Result<Instruction, Error> {
        translate_instruction_c(addr, memory_mapping, invoke_context, check_aligned)
    }

    fn translate_accounts<'a>(
        account_infos_addr: u64,
        account_infos_len: u64,
        memory_mapping: &MemoryMapping<'_>,
        invoke_context: &mut InvokeContext,
        check_aligned: bool,
    ) -> Result<Vec<TranslatedAccount<'a>>, Error> {
        translate_accounts_c(
            account_infos_addr,
            account_infos_len,
            memory_mapping,
            invoke_context,
            check_aligned,
        )
    }

    fn translate_subaccounts<'a>(
        subaccount_infos_addr: u64,
        subaccount_infos_len: u64,
        memory_mapping: &MemoryMapping<'_>,
        invoke_context: &mut InvokeContext,
        check_aligned: bool,
    ) -> Result<Vec<TranslatedAccount<'a>>, Error> {
        let (subaccount_infos, _keys) = translate_account_infos(
            subaccount_infos_addr,
            subaccount_infos_len,
            |subaccount_info: &SolAccountInfo| subaccount_info.key_addr,
            memory_mapping,
            invoke_context,
            check_aligned,
        )?;
        let mut subaccounts = translate_subaccounts_common(
            subaccount_infos,
            subaccount_infos_addr,
            invoke_context,
            memory_mapping,
            check_aligned,
            CallerAccount::from_sol_account_info,
        )?;
        // F10: append `sol_load_subaccount` slot entries — see Rust impl
        // above for the details.
        let mut slot_entries = translate_subaccount_slots::<SolAccountInfo, _>(
            invoke_context,
            memory_mapping,
            check_aligned,
            CallerAccount::from_sol_account_info,
        )?;
        subaccounts.append(&mut slot_entries);
        Ok(subaccounts)
    }

    fn translate_signers(
        program_id: &Pubkey,
        signers_seeds_addr: u64,
        signers_seeds_len: u64,
        memory_mapping: &MemoryMapping,
        check_aligned: bool,
    ) -> Result<Vec<Pubkey>, Error> {
        translate_signers_c(
            program_id,
            signers_seeds_addr,
            signers_seeds_len,
            memory_mapping,
            check_aligned,
        )
    }
}
