#![cfg(any(feature = "sbf_c", feature = "sbf_rust"))]
#![allow(clippy::clone_on_copy)]
#![allow(clippy::needless_range_loop)]
#![allow(clippy::needless_borrow)]
#![allow(clippy::cmp_owned)]
#![allow(clippy::match_like_matches_macro)]
#![allow(clippy::unnecessary_cast)]
#![allow(clippy::uninlined_format_args)]

#[cfg(all(feature = "sbf_rust", not(feature = "sbpf-v3")))]
use solana_loader_v4_interface::state::{LoaderV4State, LoaderV4Status};
#[cfg(feature = "sbf_rust")]
use {
    agave_feature_set::{self as feature_set, FeatureSet},
    agave_reserved_account_keys::ReservedAccountKeys,
    borsh::{BorshDeserialize, BorshSerialize, from_slice, to_vec},
    solana_account::{AccountSharedData, ReadableAccount},
    solana_account_info::MAX_PERMITTED_DATA_INCREASE,
    solana_client_traits::SyncClient,
    solana_clock::{MAX_PROCESSING_AGE, UnixTimestamp},
    solana_cluster_type::ClusterType,
    solana_compute_budget::compute_budget::ComputeBudget,
    solana_compute_budget_instruction::instructions_processor::process_compute_budget_instructions,
    solana_compute_budget_interface::ComputeBudgetInstruction,
    solana_fee_calculator::FeeRateGovernor,
    solana_fee_structure::{FeeBin, FeeBudgetLimits, FeeStructure},
    solana_hash::Hash,
    solana_instruction::{AccountMeta, Instruction, error::InstructionError},
    solana_keypair::Keypair,
    solana_loader_v3_interface::instruction as loader_v3_instruction,
    solana_loader_v4_interface::instruction as loader_v4_instruction,
    solana_message::{Message, SanitizedMessage, inner_instruction::InnerInstruction},
    solana_pubkey::Pubkey,
    solana_rent::Rent,
    solana_runtime::{
        bank::Bank,
        bank_client::BankClient,
        bank_forks::BankForks,
        genesis_utils::{
            GenesisConfigInfo, bootstrap_validator_stake_lamports, create_genesis_config,
            create_genesis_config_with_leader_ex,
        },
        loader_utils::{
            create_program, instructions_to_load_program_of_loader_v4, load_program_of_loader_v4,
            load_upgradeable_buffer,
        },
    },
    solana_sbf_rust_invoke_dep::*,
    solana_sbf_rust_realloc_dep::*,
    solana_sbf_rust_realloc_invoke_dep::*,
    solana_sdk_ids::sysvar::{self as sysvar, clock},
    solana_sdk_ids::{bpf_loader, bpf_loader_deprecated, bpf_loader_upgradeable, loader_v4},
    solana_signer::Signer,
    solana_svm::{
        transaction_commit_result::{CommittedTransaction, TransactionCommitResult},
        transaction_processor::ExecutionRecordingConfig,
    },
    solana_svm_feature_set::SVMFeatureSet,
    solana_svm_timings::ExecuteTimings,
    solana_svm_transaction::svm_message::SVMStaticMessage,
    solana_svm_type_overrides::rand,
    solana_system_interface::{MAX_PERMITTED_DATA_LENGTH, program as system_program},
    solana_transaction::Transaction,
    solana_transaction_error::TransactionError,
    std::{
        assert_eq,
        str::FromStr,
        sync::{Arc, RwLock},
        time::Duration,
    },
    test_case::test_matrix,
};
#[cfg(any(feature = "sbf_c", feature = "sbf_rust"))]
use {
    solana_account::Account,
    solana_program_runtime::sysvar_cache::SysvarCache,
    solana_sdk_ids::sysvar::rent,
    solana_svm_test_harness_instr::{
        self as harness, fixture::instr_context::InstrContext,
        keyed_account::keyed_account_for_system_program,
    },
};

#[cfg(feature = "sbf_rust")]
fn default_program_cache() -> solana_program_runtime::loaded_programs::ProgramCacheForTxBatch {
    harness::program_cache::new_with_builtins(/* slot */ 0)
}

fn default_program_cache_with_program(
    program_id: &Pubkey,
    program_elf: &[u8],
    feature_set: &SVMFeatureSet,
    compute_budget: &ComputeBudget,
) -> solana_program_runtime::loaded_programs::ProgramCacheForTxBatch {
    let mut program_cache = default_program_cache();
    harness::program_cache::add_program(
        &mut program_cache,
        program_id,
        &loader_v4::id(),
        program_elf,
        feature_set,
        compute_budget,
    );
    program_cache
}

#[cfg(feature = "sbf_rust")]
fn default_sysvar_cache() -> SysvarCache {
    let mut sysvar_cache = SysvarCache::default();
    sysvar_cache.fill_missing_entries(|pubkey, callback| {
        if pubkey == &rent::id() {
            let rent = Rent::default();
            let rent_data = bincode::serialize(&rent).unwrap();
            callback(&rent_data);
        } else if pubkey == &clock::id() {
            let clock = solana_clock::Clock::default();
            let clock_data = bincode::serialize(&clock).unwrap();
            callback(&clock_data);
        }
    });
    sysvar_cache
}

#[cfg(feature = "sbf_rust")]
fn process_transaction_and_record_inner(
    bank: &Bank,
    tx: Transaction,
) -> (
    Result<(), TransactionError>,
    Vec<Vec<InnerInstruction>>,
    Vec<String>,
    u64,
) {
    let commit_result = load_execute_and_commit_transaction(bank, tx);
    let CommittedTransaction {
        inner_instructions,
        log_messages,
        status,
        executed_units,
        ..
    } = commit_result.unwrap();
    let inner_instructions = inner_instructions.expect("cpi recording should be enabled");
    let log_messages = log_messages.expect("log recording should be enabled");
    (status, inner_instructions, log_messages, executed_units)
}

#[cfg(feature = "sbf_rust")]
fn load_execute_and_commit_transaction(bank: &Bank, tx: Transaction) -> TransactionCommitResult {
    let txs = vec![tx];
    let tx_batch = bank.prepare_batch_for_tests(txs);
    let mut commit_results = bank
        .load_execute_and_commit_transactions(
            &tx_batch,
            MAX_PROCESSING_AGE,
            ExecutionRecordingConfig {
                enable_cpi_recording: true,
                enable_log_recording: true,
                enable_return_data_recording: false,
                enable_transaction_balance_recording: false,
            },
            &mut ExecuteTimings::default(),
            None,
        )
        .0;
    commit_results.pop().unwrap()
}

#[cfg(feature = "sbf_rust")]
fn bank_with_feature_activated(
    bank_forks: &RwLock<BankForks>,
    parent: Arc<Bank>,
    feature_id: &Pubkey,
) -> Arc<Bank> {
    let slot = parent.slot().saturating_add(1);
    let mut bank = Bank::new_from_parent(parent, &Pubkey::new_unique(), slot);
    bank.activate_feature(feature_id);
    bank_forks
        .write()
        .unwrap()
        .insert(bank)
        .clone_without_scheduler()
}

#[cfg(feature = "sbf_rust")]
fn bank_with_feature_deactivated(
    bank_forks: &RwLock<BankForks>,
    parent: Arc<Bank>,
    feature_id: &Pubkey,
) -> Arc<Bank> {
    let slot = parent.slot().saturating_add(1);
    let mut bank = Bank::new_from_parent(parent, &Pubkey::new_unique(), slot);
    bank.deactivate_feature(feature_id);
    bank_forks
        .write()
        .unwrap()
        .insert(bank)
        .clone_without_scheduler()
}

#[cfg(feature = "sbf_rust")]
const LOADED_ACCOUNTS_DATA_SIZE_LIMIT_FOR_TEST: u32 = 64 * 1024 * 1024;

#[test]
#[cfg(any(feature = "sbf_c", feature = "sbf_rust"))]
fn test_program_sbf_sanity() {
    agave_logger::setup();

    let mut programs = Vec::new();
    #[cfg(feature = "sbf_c")]
    {
        programs.extend_from_slice(&[
            ("alloc", true),
            ("alt_bn128", true),
            ("alt_bn128_compression", true),
            ("sbf_to_sbf", true),
            ("float", true),
            ("multiple_static", true),
            ("noop", true),
            ("noop++", true),
            ("panic", false),
            ("poseidon", true),
            ("relative_call", true),
            ("return_data", true),
            ("sanity", true),
            ("sanity++", true),
            ("secp256k1_recover", true),
            ("sha", true),
            ("stdlib", true),
            ("struct_pass", true),
            ("struct_ret", true),
        ]);
    }
    #[cfg(feature = "sbf_rust")]
    {
        programs.extend_from_slice(&[
            ("solana_sbf_rust_128bit", true),
            ("solana_sbf_rust_alloc", true),
            ("solana_sbf_rust_alt_bn128", true),
            ("solana_sbf_rust_alt_bn128_compression", true),
            ("solana_sbf_rust_curve25519", true),
            ("solana_sbf_rust_custom_heap", true),
            ("solana_sbf_rust_dep_crate", true),
            ("solana_sbf_rust_external_spend", false),
            ("solana_sbf_rust_iter", true),
            ("solana_sbf_rust_many_args", true),
            ("solana_sbf_rust_mem", true),
            ("solana_sbf_rust_membuiltins", true),
            ("solana_sbf_rust_noop", true),
            ("solana_sbf_rust_panic", false),
            ("solana_sbf_rust_param_passing", true),
            ("solana_sbf_rust_poseidon", true),
            ("solana_sbf_rust_rand", true),
            ("solana_sbf_rust_remaining_compute_units", true),
            ("solana_sbf_rust_sanity", true),
            ("solana_sbf_rust_secp256k1_recover", true),
            ("solana_sbf_rust_sha", true),
        ]);
    }

    #[cfg(all(feature = "sbf_rust", feature = "sbf_sanity_list"))]
    {
        // This code generates the list of sanity programs for a CI job to build with
        // cargo-build-sbf and ensure it is working correctly.
        use std::{env, fs::File, io::Write};
        let current_dir = env::current_dir().unwrap();
        let mut file =
            File::create(current_dir.join("target").join("sanity_programs.txt")).unwrap();
        for program in programs.iter() {
            writeln!(file, "{}", program.0.trim_start_matches("solana_sbf_rust_"))
                .expect("Failed to write to file");
        }
    }

    #[cfg(not(feature = "sbf_sanity_list"))]
    for program in programs.iter() {
        println!("Test program: {:?}", program.0);

        let program_elf = harness::file::load_program_elf(program.0);
        let program_id = Pubkey::new_unique();

        let feature_set = SVMFeatureSet::all_enabled();

        let pubkey1 = Pubkey::new_unique();
        let pubkey2 = Pubkey::new_unique();

        let account_metas = vec![
            AccountMeta::new(pubkey1, true),
            AccountMeta::new(pubkey2, false),
        ];
        let instruction = Instruction::new_with_bytes(program_id, &[1], account_metas);

        let accounts = vec![(pubkey1, Account::default()), (pubkey2, Account::default())];

        let compute_budget = ComputeBudget::new_with_defaults(false, false);
        let mut program_cache = default_program_cache_with_program(
            &program_id,
            &program_elf,
            &feature_set,
            &compute_budget,
        );
        let sysvar_cache = default_sysvar_cache();

        let context = InstrContext {
            feature_set,
            accounts,
            instruction,
        };

        let effects =
            harness::execute_instr(context, &compute_budget, &mut program_cache, &sysvar_cache)
                .unwrap();

        let result = match effects.result {
            Some(err) => Err(err),
            None => Ok(()),
        };

        if program.1 {
            assert!(result.is_ok(), "{result:?}");
        } else {
            assert!(result.is_err(), "{result:?}");
        }
    }
}

#[test]
#[cfg(any(feature = "sbf_c", feature = "sbf_rust"))]
fn test_program_sbf_loader_deprecated() {
    agave_logger::setup();

    let mut programs = Vec::new();
    #[cfg(feature = "sbf_c")]
    {
        programs.extend_from_slice(&[("deprecated_loader")]);
    }
    #[cfg(feature = "sbf_rust")]
    {
        programs.extend_from_slice(&[("solana_sbf_rust_deprecated_loader")]);
    }

    for program in programs.iter() {
        println!("Test program: {:?}", program);

        let program_elf = harness::file::load_program_elf(program);
        let program_id = Pubkey::new_unique();

        let feature_set = SVMFeatureSet {
            disable_deploy_of_alloc_free_syscall: false,
            ..SVMFeatureSet::all_enabled()
        };

        let compute_budget = ComputeBudget::new_with_defaults(false, false);

        let pubkey = Pubkey::new_unique();
        let accounts = vec![
            (program_id, Account::new(0, 0, &bpf_loader_deprecated::id())),
            (pubkey, Account::default()),
        ];

        let mut program_cache = default_program_cache();
        harness::program_cache::add_program(
            &mut program_cache,
            &program_id,
            &bpf_loader_deprecated::id(),
            &program_elf,
            &feature_set,
            &compute_budget,
        );
        let sysvar_cache = default_sysvar_cache();

        let account_metas = vec![AccountMeta::new(pubkey, true)];
        let instruction = Instruction::new_with_bytes(program_id, &[255], account_metas);

        let context = InstrContext {
            feature_set,
            accounts,
            instruction,
        };

        let effects =
            harness::execute_instr(context, &compute_budget, &mut program_cache, &sysvar_cache)
                .unwrap();

        assert!(effects.result.is_none());
    }
}

#[test]
#[cfg(all(feature = "sbf_rust", not(feature = "sbpf-v3")))]
// In SBPFv3, we don't have a verification step for undefined syscalls, and we don't do dynamic
// symbol resolution, so this test would pass.
fn test_sol_alloc_free_no_longer_deployable_with_upgradeable_loader() {
    agave_logger::setup();

    // Populate loader account with `solana_sbf_rust_deprecated_loader` elf, which
    // depends on `sol_alloc_free_` syscall. This can be verified with
    // $ elfdump solana_sbf_rust_deprecated_loader.so
    // : 0000000000001ab8  000000070000000a R_BPF_64_32            0000000000000000 sol_alloc_free_
    // In the symbol table, there is `sol_alloc_free_`.
    // In fact, `sol_alloc_free_` is called from sbf allocator, which is originated from
    // AccountInfo::realloc() in the program code.

    let program_elf = harness::file::load_program_elf("solana_sbf_rust_deprecated_loader");
    let program_id = Pubkey::new_unique();
    let authority_pubkey = Pubkey::new_unique();

    let feature_set = SVMFeatureSet::all_enabled();
    let compute_budget = ComputeBudget::new_with_defaults(false, false);

    // Create a retracted program account with LoaderV4State header + ELF bytes.
    let loader_state = LoaderV4State {
        slot: 0,
        authority_address_or_next_version: authority_pubkey,
        status: LoaderV4Status::Retracted,
    };
    let state_bytes: &[u8; LoaderV4State::program_data_offset()] = unsafe {
        std::mem::transmute::<&LoaderV4State, &[u8; LoaderV4State::program_data_offset()]>(
            &loader_state,
        )
    };
    let mut program_data = state_bytes.to_vec();
    program_data.extend_from_slice(&program_elf);

    let accounts = vec![
        (
            program_id,
            Account {
                lamports: 1_000_000_000,
                data: program_data,
                owner: loader_v4::id(),
                executable: false,
                rent_epoch: u64::MAX,
            },
        ),
        (authority_pubkey, Account::default()),
    ];

    let mut program_cache = default_program_cache();
    let sysvar_cache = default_sysvar_cache();

    // Build the deploy instruction
    let instruction = loader_v4_instruction::deploy(&program_id, &authority_pubkey);

    let context = InstrContext {
        feature_set,
        accounts,
        instruction,
    };

    // Expect that deployment to fail. B/C during deployment, there is an elf
    // verification step, which uses the runtime to look up relocatable symbols
    // in elf inside syscall table. In this case, `sol_alloc_free_` can't be
    // found in syscall table. Hence, the verification fails and the deployment
    // fails.
    let effects =
        harness::execute_instr(context, &compute_budget, &mut program_cache, &sysvar_cache)
            .unwrap();

    assert_eq!(effects.result, Some(InstructionError::InvalidAccountData));
}

#[test]
#[cfg(feature = "sbf_rust")]
fn test_program_sbf_duplicate_accounts() {
    agave_logger::setup();

    let mut programs = Vec::new();
    #[cfg(feature = "sbf_c")]
    {
        programs.extend_from_slice(&[("dup_accounts")]);
    }
    #[cfg(feature = "sbf_rust")]
    {
        programs.extend_from_slice(&[("solana_sbf_rust_dup_accounts")]);
    }

    for program in programs.iter() {
        println!("Test program: {:?}", program);

        let program_elf = harness::file::load_program_elf(program);
        let program_id = Pubkey::new_unique();
        let feature_set = SVMFeatureSet::all_enabled();
        let compute_budget = ComputeBudget::new_with_defaults(false, false);
        let mut program_cache = default_program_cache_with_program(
            &program_id,
            &program_elf,
            &feature_set,
            &compute_budget,
        );
        let sysvar_cache = default_sysvar_cache();

        let payer_pubkey = Pubkey::new_unique();
        let payee_pubkey = Pubkey::new_unique();
        let pubkey = Pubkey::new_unique();
        let account = Account::new(10, 1, &program_id);

        let account_metas = vec![
            AccountMeta::new(payer_pubkey, true),
            AccountMeta::new(payee_pubkey, false),
            AccountMeta::new(pubkey, false),
            AccountMeta::new(pubkey, false),
        ];

        let mut execute = |data: &[u8]| {
            let accounts = vec![
                (payer_pubkey, Account::new(100, 0, &Pubkey::default())),
                (payee_pubkey, Account::new(10, 1, &program_id)),
                (pubkey, account.clone()),
            ];
            let instruction = Instruction::new_with_bytes(program_id, data, account_metas.clone());
            let context = InstrContext {
                feature_set,
                accounts,
                instruction,
            };
            harness::execute_instr(context, &compute_budget, &mut program_cache, &sysvar_cache)
                .unwrap()
        };

        let effects = execute(&[1]);
        assert!(effects.result.is_none());
        let data = effects.get_account(&pubkey).unwrap().data.clone();
        assert_eq!(data[0], 1);

        let effects = execute(&[2]);
        assert!(effects.result.is_none());
        let data = effects.get_account(&pubkey).unwrap().data.clone();
        assert_eq!(data[0], 2);

        let effects = execute(&[3]);
        assert!(effects.result.is_none());
        let data = effects.get_account(&pubkey).unwrap().data.clone();
        assert_eq!(data[0], 3);

        let effects = execute(&[4]);
        assert!(effects.result.is_none());
        let lamports = effects.get_account(&pubkey).unwrap().lamports;
        assert_eq!(lamports, 11);

        let effects = execute(&[5]);
        assert!(effects.result.is_none());
        let lamports = effects.get_account(&pubkey).unwrap().lamports;
        assert_eq!(lamports, 12);

        let effects = execute(&[6]);
        assert!(effects.result.is_none());
        let lamports = effects.get_account(&pubkey).unwrap().lamports;
        assert_eq!(lamports, 13);

        let pubkey = Pubkey::new_unique();
        let account_metas = vec![
            AccountMeta::new(payer_pubkey, true),
            AccountMeta::new(payee_pubkey, false),
            AccountMeta::new(pubkey, false),
            AccountMeta::new_readonly(pubkey, true),
            AccountMeta::new_readonly(program_id, false),
        ];
        let accounts = vec![
            (payer_pubkey, Account::new(100, 0, &Pubkey::default())),
            (payee_pubkey, Account::new(10, 1, &program_id)),
            (pubkey, Account::new(10, 1, &program_id)),
        ];
        let instruction = Instruction::new_with_bytes(program_id, &[7], account_metas);
        let context = InstrContext {
            feature_set,
            accounts,
            instruction,
        };
        let effects =
            harness::execute_instr(context, &compute_budget, &mut program_cache, &sysvar_cache)
                .unwrap();
        assert!(effects.result.is_none());
    }
}

#[test]
#[cfg(feature = "sbf_rust")]
fn test_program_sbf_error_handling() {
    agave_logger::setup();

    let mut programs = Vec::new();
    #[cfg(feature = "sbf_c")]
    {
        programs.extend_from_slice(&[("error_handling")]);
    }
    #[cfg(feature = "sbf_rust")]
    {
        programs.extend_from_slice(&[("solana_sbf_rust_error_handling")]);
    }

    for program in programs.iter() {
        println!("Test program: {:?}", program);

        let program_elf = harness::file::load_program_elf(program);
        let program_id = Pubkey::new_unique();

        let feature_set = SVMFeatureSet::all_enabled();

        let pubkey1 = Pubkey::new_unique();

        let accounts = vec![(pubkey1, Account::default())];

        let compute_budget = ComputeBudget::new_with_defaults(false, false);
        let mut program_cache = default_program_cache_with_program(
            &program_id,
            &program_elf,
            &feature_set,
            &compute_budget,
        );
        let sysvar_cache = default_sysvar_cache();

        // Helper to execute an instruction with the given data byte.
        let mut execute = |data: &[u8]| {
            let account_metas = vec![AccountMeta::new(pubkey1, true)];
            let instruction = Instruction::new_with_bytes(program_id, data, account_metas);

            let context = InstrContext {
                feature_set,
                accounts: accounts.clone(),
                instruction,
            };

            harness::execute_instr(context, &compute_budget, &mut program_cache, &sysvar_cache)
                .unwrap()
        };

        let effects = execute(&[1]);
        assert!(effects.result.is_none());

        let effects = execute(&[2]);
        assert_eq!(effects.result, Some(InstructionError::InvalidAccountData));

        let effects = execute(&[3]);
        assert_eq!(effects.result, Some(InstructionError::Custom(0)));

        let effects = execute(&[4]);
        assert_eq!(effects.result, Some(InstructionError::Custom(42)));

        let effects = execute(&[5]);
        assert!(
            effects.result == Some(InstructionError::InvalidInstructionData)
                || effects.result == Some(InstructionError::InvalidError)
        );

        let effects = execute(&[6]);
        assert!(
            effects.result == Some(InstructionError::InvalidInstructionData)
                || effects.result == Some(InstructionError::InvalidError)
        );

        let effects = execute(&[7]);
        assert!(
            effects.result == Some(InstructionError::InvalidInstructionData)
                || effects.result == Some(InstructionError::AccountBorrowFailed)
        );

        let effects = execute(&[8]);
        assert_eq!(
            effects.result,
            Some(InstructionError::InvalidInstructionData)
        );

        let effects = execute(&[9]);
        assert_eq!(
            effects.result,
            Some(InstructionError::MaxSeedLengthExceeded)
        );
    }
}

#[test]
#[cfg(any(feature = "sbf_c", feature = "sbf_rust"))]
fn test_return_data_and_log_data_syscall() {
    agave_logger::setup();

    let mut programs = Vec::new();
    #[cfg(feature = "sbf_c")]
    {
        programs.extend_from_slice(&[("log_data")]);
    }
    #[cfg(feature = "sbf_rust")]
    {
        programs.extend_from_slice(&[("solana_sbf_rust_log_data")]);
    }

    for program in programs.iter() {
        println!("Test program: {:?}", program);

        let program_elf = harness::file::load_program_elf(program);
        let program_id = Pubkey::new_unique();

        let feature_set = SVMFeatureSet::all_enabled();
        let compute_budget = ComputeBudget::new_with_defaults(false, false);

        let pubkey = Pubkey::new_unique();
        let accounts = vec![(pubkey, Account::default())];

        let mut program_cache = default_program_cache_with_program(
            &program_id,
            &program_elf,
            &feature_set,
            &compute_budget,
        );
        let sysvar_cache = default_sysvar_cache();

        let account_metas = vec![AccountMeta::new(pubkey, true)];
        let instruction =
            Instruction::new_with_bytes(program_id, &[1, 2, 3, 0, 4, 5, 6], account_metas);

        let context = InstrContext {
            feature_set,
            accounts,
            instruction,
        };

        let effects =
            harness::execute_instr(context, &compute_budget, &mut program_cache, &sysvar_cache)
                .unwrap();

        assert!(effects.result.is_none());

        assert!(
            effects
                .logs
                .iter()
                .any(|log| log == "Program data: AQID BAUG")
        );

        assert_eq!(effects.return_data, vec![0x08, 0x01, 0x44]);

        assert!(
            effects
                .logs
                .iter()
                .any(|log| log == &format!("Program return: {} CAFE", program_id))
        );
    }
}

#[test]
#[cfg(feature = "sbf_rust")]
fn test_program_sbf_invoke_sanity() {
    agave_logger::setup();

    #[derive(Debug)]
    #[allow(dead_code)]
    enum Languages {
        C,
        Rust,
    }
    let mut programs = Vec::new();
    #[cfg(feature = "sbf_c")]
    {
        programs.push((Languages::C, "invoke", "invoked", "noop"));
    }
    #[cfg(feature = "sbf_rust")]
    {
        programs.push((
            Languages::Rust,
            "solana_sbf_rust_invoke",
            "solana_sbf_rust_invoked",
            "solana_sbf_rust_noop",
        ));
    }
    for program in programs.iter() {
        println!("Test program: {:?}", program);

        let GenesisConfigInfo {
            genesis_config,
            mint_keypair,
            ..
        } = create_genesis_config(50);

        let (bank, bank_forks) = Bank::new_with_bank_forks_for_tests(&genesis_config);
        let mut bank_client = BankClient::new_shared(bank.clone());
        let authority_keypair = Keypair::new();

        let (_bank, invoke_program_id) = load_program_of_loader_v4(
            &mut bank_client,
            &bank_forks,
            &mint_keypair,
            &authority_keypair,
            program.1,
        );
        let (_bank, invoked_program_id) = load_program_of_loader_v4(
            &mut bank_client,
            &bank_forks,
            &mint_keypair,
            &authority_keypair,
            program.2,
        );
        let (bank, noop_program_id) = load_program_of_loader_v4(
            &mut bank_client,
            &bank_forks,
            &mint_keypair,
            &authority_keypair,
            program.3,
        );

        let argument_keypair = Keypair::new();
        let account = AccountSharedData::new(42, 100, &invoke_program_id);
        bank.store_account(&argument_keypair.pubkey(), &account);

        let invoked_argument_keypair = Keypair::new();
        let account = AccountSharedData::new(20, 10, &invoked_program_id);
        bank.store_account(&invoked_argument_keypair.pubkey(), &account);

        let from_keypair = Keypair::new();
        let account = AccountSharedData::new(84, 0, &system_program::id());
        bank.store_account(&from_keypair.pubkey(), &account);

        let unexecutable_program_keypair = Keypair::new();
        let account = AccountSharedData::new(1, 0, &bpf_loader::id());
        bank.store_account(&unexecutable_program_keypair.pubkey(), &account);

        let noop_program_keypair = Keypair::new();
        let account = AccountSharedData::new(42, 5, &noop_program_id);
        bank.store_account(&noop_program_keypair.pubkey(), &account);

        let (derived_key1, bump_seed1) =
            Pubkey::find_program_address(&[b"You pass butter"], &invoke_program_id);
        let (derived_key2, bump_seed2) =
            Pubkey::find_program_address(&[b"Lil'", b"Bits"], &invoked_program_id);
        let (derived_key3, bump_seed3) =
            Pubkey::find_program_address(&[derived_key2.as_ref()], &invoked_program_id);

        let mint_pubkey = mint_keypair.pubkey();
        let account_metas = vec![
            AccountMeta::new(mint_pubkey, true),
            AccountMeta::new(argument_keypair.pubkey(), true),
            AccountMeta::new_readonly(invoked_program_id, false),
            AccountMeta::new(invoked_argument_keypair.pubkey(), true),
            AccountMeta::new_readonly(invoked_program_id, false),
            AccountMeta::new(argument_keypair.pubkey(), true),
            AccountMeta::new(derived_key1, false),
            AccountMeta::new(derived_key2, false),
            AccountMeta::new_readonly(derived_key3, false),
            AccountMeta::new_readonly(system_program::id(), false),
            AccountMeta::new(from_keypair.pubkey(), true),
            AccountMeta::new_readonly(solana_sdk_ids::ed25519_program::id(), false),
            AccountMeta::new_readonly(invoke_program_id, false),
            AccountMeta::new_readonly(unexecutable_program_keypair.pubkey(), false),
            AccountMeta::new_readonly(noop_program_id, false),
        ];

        let do_invoke = |test: u8, additional_instructions: &[Instruction], bank: &Bank| {
            let instruction_data = &[test, bump_seed1, bump_seed2, bump_seed3];
            let signers = vec![
                &mint_keypair,
                &argument_keypair,
                &invoked_argument_keypair,
                &from_keypair,
            ];
            let mut instructions = vec![Instruction::new_with_bytes(
                invoke_program_id,
                instruction_data,
                account_metas.clone(),
            )];
            instructions.extend_from_slice(additional_instructions);
            let message = Message::new(&instructions, Some(&mint_pubkey));
            let tx = Transaction::new(&signers, message.clone(), bank.last_blockhash());
            let (result, inner_instructions, log_messages, executed_units) =
                process_transaction_and_record_inner(bank, tx);

            let invoked_programs: Vec<Pubkey> = inner_instructions
                .first()
                .map(|instructions| {
                    instructions
                        .iter()
                        .filter_map(|ix| {
                            message
                                .account_keys
                                .get(ix.instruction.program_id_index as usize)
                        })
                        .cloned()
                        .collect()
                })
                .unwrap_or_default();

            let no_invoked_programs: Vec<Pubkey> = inner_instructions
                .get(1)
                .map(|instructions| {
                    instructions
                        .iter()
                        .filter_map(|ix| {
                            message
                                .account_keys
                                .get(ix.instruction.program_id_index as usize)
                        })
                        .cloned()
                        .collect()
                })
                .unwrap_or_default();

            (
                result,
                log_messages,
                executed_units,
                invoked_programs,
                no_invoked_programs,
            )
        };

        // success cases

        let do_invoke_success = |test: u8,
                                 additional_instructions: &[Instruction],
                                 expected_invoked_programs: &[Pubkey],
                                 bank: &Bank| {
            println!("Running success test #{:?}", test);

            let (result, _log_messages, _executed_units, invoked_programs, no_invoked_programs) =
                do_invoke(test, additional_instructions, bank);

            assert_eq!(result, Ok(()));
            assert_eq!(invoked_programs.len(), expected_invoked_programs.len());
            assert_eq!(invoked_programs, expected_invoked_programs);
            assert_eq!(no_invoked_programs.len(), 0);
        };

        do_invoke_success(
            TEST_SUCCESS,
            &[Instruction::new_with_bytes(noop_program_id, &[], vec![])],
            match program.0 {
                Languages::C => vec![
                    system_program::id(),
                    system_program::id(),
                    invoked_program_id.clone(),
                    invoked_program_id.clone(),
                    invoked_program_id.clone(),
                    invoked_program_id.clone(),
                    invoked_program_id.clone(),
                    invoked_program_id.clone(),
                    invoked_program_id.clone(),
                    invoked_program_id.clone(),
                    invoked_program_id.clone(),
                    invoked_program_id.clone(),
                    invoked_program_id.clone(),
                    invoked_program_id.clone(),
                    invoked_program_id.clone(),
                    invoked_program_id.clone(),
                    invoked_program_id.clone(),
                    invoked_program_id.clone(),
                    invoked_program_id.clone(),
                ],
                Languages::Rust => vec![
                    system_program::id(),
                    system_program::id(),
                    invoked_program_id.clone(),
                    invoked_program_id.clone(),
                    invoked_program_id.clone(),
                    invoked_program_id.clone(),
                    invoked_program_id.clone(),
                    invoked_program_id.clone(),
                    invoked_program_id.clone(),
                    invoked_program_id.clone(),
                    invoked_program_id.clone(),
                    invoked_program_id.clone(),
                    invoked_program_id.clone(),
                    invoked_program_id.clone(),
                    invoked_program_id.clone(),
                    invoked_program_id.clone(),
                    invoked_program_id.clone(),
                    invoked_program_id.clone(),
                    invoked_program_id.clone(),
                    invoked_program_id.clone(),
                    invoked_program_id.clone(),
                    system_program::id(),
                    invoked_program_id.clone(),
                    invoked_program_id.clone(),
                ],
            }
            .as_ref(),
            &bank,
        );

        // With SIMD-0268 enabled, eight nested invokes should pass.
        let bank = bank_with_feature_activated(
            &bank_forks,
            bank,
            &feature_set::raise_cpi_nesting_limit_to_8::id(),
        );
        assert!(
            bank.feature_set
                .is_active(&feature_set::raise_cpi_nesting_limit_to_8::id())
        );
        {
            // Reset the account balances for `ARGUMENT` and `INVOKED_ARGUMENT`
            let account = AccountSharedData::new(42, 100, &invoke_program_id);
            bank.store_account(&argument_keypair.pubkey(), &account);

            let account = AccountSharedData::new(20, 10, &invoked_program_id);
            bank.store_account(&invoked_argument_keypair.pubkey(), &account);
        }
        do_invoke_success(
            TEST_NESTED_INVOKE_SIMD_0268_OK,
            &[],
            &[invoked_program_id.clone(); 16], // 16, 8 for each invoke
            &bank,
        );
        do_invoke_success(
            TEST_MAX_ACCOUNT_INFOS_OK,
            &[],
            std::slice::from_ref(&invoked_program_id),
            &bank,
        );

        do_invoke_success(
            TEST_CU_USAGE_MINIMUM,
            &[],
            std::slice::from_ref(&noop_program_id),
            &bank,
        );

        do_invoke_success(
            TEST_CU_USAGE_BASELINE,
            &[],
            std::slice::from_ref(&noop_program_id),
            &bank,
        );

        do_invoke_success(
            TEST_CU_USAGE_MAX,
            &[],
            std::slice::from_ref(&noop_program_id),
            &bank,
        );

        let bank = bank_with_feature_deactivated(
            &bank_forks,
            bank,
            &feature_set::increase_cpi_account_info_limit::id(),
        );

        assert!(
            !bank
                .feature_set
                .is_active(&feature_set::increase_cpi_account_info_limit::id())
        );

        do_invoke_success(
            TEST_MAX_ACCOUNT_INFOS_OK_BEFORE_SIMD_0339,
            &[],
            std::slice::from_ref(&invoked_program_id),
            &bank,
        );

        let bank = bank_with_feature_deactivated(
            &bank_forks,
            bank,
            &feature_set::increase_tx_account_lock_limit::id(),
        );
        assert!(
            !bank
                .feature_set
                .is_active(&feature_set::increase_tx_account_lock_limit::id())
        );

        do_invoke_success(
            TEST_MAX_ACCOUNT_INFOS_OK_BEFORE_INCREASE_TX_ACCOUNT_LOCK_BEFORE_SIMD_0339,
            &[],
            std::slice::from_ref(&invoked_program_id),
            &bank,
        );
        let bank = bank_with_feature_activated(
            &bank_forks,
            bank,
            &feature_set::increase_tx_account_lock_limit::id(),
        );

        assert!(
            bank.feature_set
                .is_active(&feature_set::increase_tx_account_lock_limit::id())
        );

        let bank = bank_with_feature_activated(
            &bank_forks,
            bank,
            &feature_set::increase_cpi_account_info_limit::id(),
        );

        assert!(
            bank.feature_set
                .is_active(&feature_set::increase_cpi_account_info_limit::id())
        );
        // failure cases

        let do_invoke_failure_test_local_with_compute_check =
            |test: u8,
             expected_error: TransactionError,
             expected_invoked_programs: &[Pubkey],
             expected_log_messages: Option<Vec<String>>,
             should_deplete_compute_meter: bool,
             bank: &Bank| {
                println!("Running failure test #{:?}", test);

                let compute_unit_limit = 1_000_000;
                let (result, log_messages, executed_units, invoked_programs, _) = do_invoke(
                    test,
                    &[ComputeBudgetInstruction::set_compute_unit_limit(
                        compute_unit_limit,
                    )],
                    bank,
                );

                assert_eq!(result, Err(expected_error));
                assert_eq!(invoked_programs, expected_invoked_programs);
                if should_deplete_compute_meter {
                    assert_eq!(executed_units, compute_unit_limit as u64);
                } else {
                    assert!(executed_units < compute_unit_limit as u64);
                }
                if let Some(expected_log_messages) = expected_log_messages {
                    assert_eq!(log_messages.len(), expected_log_messages.len());
                    expected_log_messages
                        .into_iter()
                        .zip(log_messages)
                        .for_each(|(expected_log_message, log_message)| {
                            if expected_log_message != String::from("skip") {
                                assert_eq!(log_message, expected_log_message);
                            }
                        });
                }
            };

        let do_invoke_failure_test_local =
            |test: u8,
             expected_error: TransactionError,
             expected_invoked_programs: &[Pubkey],
             expected_log_messages: Option<Vec<String>>,
             bank: &Bank| {
                do_invoke_failure_test_local_with_compute_check(
                    test,
                    expected_error,
                    expected_invoked_programs,
                    expected_log_messages,
                    false, // should_deplete_compute_meter
                    bank,
                )
            };

        let program_lang = match program.0 {
            Languages::Rust => "Rust",
            Languages::C => "C",
        };

        do_invoke_failure_test_local(
            TEST_PRIVILEGE_ESCALATION_SIGNER,
            TransactionError::InstructionError(0, InstructionError::PrivilegeEscalation),
            std::slice::from_ref(&invoked_program_id),
            None,
            &bank,
        );

        do_invoke_failure_test_local(
            TEST_PRIVILEGE_ESCALATION_WRITABLE,
            TransactionError::InstructionError(0, InstructionError::PrivilegeEscalation),
            std::slice::from_ref(&invoked_program_id),
            None,
            &bank,
        );

        do_invoke_failure_test_local(
            TEST_PPROGRAM_NOT_OWNED_BY_LOADER,
            TransactionError::InstructionError(0, InstructionError::UnsupportedProgramId),
            &[argument_keypair.pubkey()],
            None,
            &bank,
        );

        do_invoke_failure_test_local(
            TEST_PPROGRAM_NOT_EXECUTABLE,
            TransactionError::InstructionError(0, InstructionError::UnsupportedProgramId),
            &[unexecutable_program_keypair.pubkey()],
            None,
            &bank,
        );

        do_invoke_failure_test_local(
            TEST_EMPTY_ACCOUNTS_SLICE,
            TransactionError::InstructionError(0, InstructionError::MissingAccount),
            &[],
            None,
            &bank,
        );

        do_invoke_failure_test_local(
            TEST_CAP_SEEDS,
            TransactionError::InstructionError(0, InstructionError::MaxSeedLengthExceeded),
            &[],
            None,
            &bank,
        );

        do_invoke_failure_test_local(
            TEST_CAP_SIGNERS,
            TransactionError::InstructionError(0, InstructionError::ProgramFailedToComplete),
            &[],
            None,
            &bank,
        );

        do_invoke_failure_test_local(
            TEST_MAX_INSTRUCTION_DATA_LEN_EXCEEDED,
            TransactionError::InstructionError(0, InstructionError::ProgramFailedToComplete),
            &[],
            Some(vec![
                format!("Program {invoke_program_id} invoke [1]"),
                format!("Program log: invoke {program_lang} program"),
                "Program log: Test max instruction data len exceeded".into(),
                "skip".into(), // don't compare compute consumption logs
                format!(
                    "Program {invoke_program_id} failed: Invoked an instruction with data that is \
                     too large (10241 > 10240)"
                ),
            ]),
            &bank,
        );

        do_invoke_failure_test_local(
            TEST_MAX_INSTRUCTION_ACCOUNTS_EXCEEDED,
            TransactionError::InstructionError(0, InstructionError::ProgramFailedToComplete),
            &[],
            Some(vec![
                format!("Program {invoke_program_id} invoke [1]"),
                format!("Program log: invoke {program_lang} program"),
                "Program log: Test max instruction accounts exceeded".into(),
                "skip".into(), // don't compare compute consumption logs
                format!(
                    "Program {invoke_program_id} failed: Invoked an instruction with too many \
                     accounts (256 > 255)"
                ),
            ]),
            &bank,
        );

        do_invoke_failure_test_local(
            TEST_MAX_ACCOUNT_INFOS_EXCEEDED,
            TransactionError::InstructionError(0, InstructionError::ProgramFailedToComplete),
            &[],
            Some(vec![
                format!("Program {invoke_program_id} invoke [1]"),
                format!("Program log: invoke {program_lang} program"),
                "Program log: Test max account infos exceeded".into(),
                "skip".into(), // don't compare compute consumption logs
                format!(
                    "Program {invoke_program_id} failed: Invoked an instruction with too many \
                     account info's (256 > 255)"
                ),
            ]),
            &bank,
        );

        let bank = bank_with_feature_deactivated(
            &bank_forks,
            bank,
            &feature_set::increase_cpi_account_info_limit::id(),
        );

        assert!(
            !bank
                .feature_set
                .is_active(&feature_set::increase_cpi_account_info_limit::id())
        );

        do_invoke_failure_test_local(
            TEST_MAX_ACCOUNT_INFOS_EXCEEDED_BEFORE_SIMD_0339,
            TransactionError::InstructionError(0, InstructionError::ProgramFailedToComplete),
            &[],
            Some(vec![
                format!("Program {invoke_program_id} invoke [1]"),
                format!("Program log: invoke {program_lang} program"),
                "Program log: Test max account infos exceeded before SIMD-0339".into(),
                "skip".into(), // don't compare compute consumption logs
                format!(
                    "Program {invoke_program_id} failed: Invoked an instruction with too many \
                     account info's (129 > 128)"
                ),
            ]),
            &bank,
        );

        let bank = bank_with_feature_deactivated(
            &bank_forks,
            bank,
            &feature_set::increase_tx_account_lock_limit::id(),
        );

        assert!(
            !bank
                .feature_set
                .is_active(&feature_set::increase_tx_account_lock_limit::id())
        );

        do_invoke_failure_test_local(
            TEST_MAX_ACCOUNT_INFOS_EXCEEDED_BEFORE_INCREASE_TX_ACCOUNT_LOCK_BEFORE_SIMD_0339,
            TransactionError::InstructionError(0, InstructionError::ProgramFailedToComplete),
            &[],
            Some(vec![
                format!("Program {invoke_program_id} invoke [1]"),
                format!("Program log: invoke {program_lang} program"),
                "Program log: Test max account infos exceeded before SIMD-0339 and before \
                 increase cpi info"
                    .into(),
                "skip".into(), // don't compare compute consumption logs
                format!(
                    "Program {invoke_program_id} failed: Invoked an instruction with too many \
                     account info's (65 > 64)"
                ),
            ]),
            &bank,
        );

        do_invoke_failure_test_local(
            TEST_RETURN_ERROR,
            TransactionError::InstructionError(0, InstructionError::Custom(42)),
            std::slice::from_ref(&invoked_program_id),
            None,
            &bank,
        );

        do_invoke_failure_test_local(
            TEST_PRIVILEGE_DEESCALATION_ESCALATION_SIGNER,
            TransactionError::InstructionError(0, InstructionError::PrivilegeEscalation),
            std::slice::from_ref(&invoked_program_id),
            None,
            &bank,
        );

        do_invoke_failure_test_local(
            TEST_PRIVILEGE_DEESCALATION_ESCALATION_WRITABLE,
            TransactionError::InstructionError(0, InstructionError::PrivilegeEscalation),
            std::slice::from_ref(&invoked_program_id),
            None,
            &bank,
        );

        do_invoke_failure_test_local_with_compute_check(
            TEST_WRITABLE_DEESCALATION_WRITABLE,
            TransactionError::InstructionError(0, InstructionError::ReadonlyDataModified),
            std::slice::from_ref(&invoked_program_id),
            None,
            true, // should_deplete_compute_meter
            &bank,
        );

        // With SIMD-0268 disabled, five nested invokes is too deep.
        let bank = bank_with_feature_deactivated(
            &bank_forks,
            bank,
            &feature_set::raise_cpi_nesting_limit_to_8::id(),
        );
        assert!(
            !bank
                .feature_set
                .is_active(&feature_set::raise_cpi_nesting_limit_to_8::id())
        );
        do_invoke_failure_test_local(
            TEST_NESTED_INVOKE_TOO_DEEP,
            TransactionError::InstructionError(0, InstructionError::CallDepth),
            &[
                invoked_program_id.clone(),
                invoked_program_id.clone(),
                invoked_program_id.clone(),
                invoked_program_id.clone(),
                invoked_program_id.clone(),
            ],
            None,
            &bank,
        );

        // With SIMD-0268 enabled, nine nested invokes is too deep.
        let bank = bank_with_feature_activated(
            &bank_forks,
            bank,
            &feature_set::raise_cpi_nesting_limit_to_8::id(),
        );
        assert!(
            bank.feature_set
                .is_active(&feature_set::raise_cpi_nesting_limit_to_8::id())
        );
        do_invoke_failure_test_local(
            TEST_NESTED_INVOKE_SIMD_0268_TOO_DEEP,
            TransactionError::InstructionError(0, InstructionError::CallDepth),
            &[
                invoked_program_id.clone(),
                invoked_program_id.clone(),
                invoked_program_id.clone(),
                invoked_program_id.clone(),
                invoked_program_id.clone(),
                invoked_program_id.clone(),
                invoked_program_id.clone(),
                invoked_program_id.clone(),
                invoked_program_id.clone(),
            ],
            None,
            &bank,
        );

        do_invoke_failure_test_local(
            TEST_RETURN_DATA_TOO_LARGE,
            TransactionError::InstructionError(0, InstructionError::ProgramFailedToComplete),
            &[],
            None,
            &bank,
        );

        do_invoke_failure_test_local(
            TEST_DUPLICATE_PRIVILEGE_ESCALATION_SIGNER,
            TransactionError::InstructionError(0, InstructionError::PrivilegeEscalation),
            std::slice::from_ref(&invoked_program_id),
            None,
            &bank,
        );

        do_invoke_failure_test_local(
            TEST_DUPLICATE_PRIVILEGE_ESCALATION_WRITABLE,
            TransactionError::InstructionError(0, InstructionError::PrivilegeEscalation),
            std::slice::from_ref(&invoked_program_id),
            None,
            &bank,
        );

        // Check resulting state

        assert_eq!(43, bank.get_balance(&derived_key1));
        let account = bank.get_account(&derived_key1).unwrap();
        assert_eq!(&invoke_program_id, account.owner());
        assert_eq!(
            MAX_PERMITTED_DATA_INCREASE,
            bank.get_account(&derived_key1).unwrap().data().len()
        );
        for i in 0..20 {
            assert_eq!(i as u8, account.data()[i]);
        }

        // Attempt to realloc into unauthorized address space
        let account = AccountSharedData::new(84, 0, &system_program::id());
        bank.store_account(&from_keypair.pubkey(), &account);
        bank.store_account(&derived_key1, &AccountSharedData::default());
        let instruction = Instruction::new_with_bytes(
            invoke_program_id,
            &[
                TEST_ALLOC_ACCESS_VIOLATION,
                bump_seed1,
                bump_seed2,
                bump_seed3,
            ],
            account_metas.clone(),
        );
        let message = Message::new(&[instruction], Some(&mint_pubkey));
        let tx = Transaction::new(
            &[
                &mint_keypair,
                &argument_keypair,
                &invoked_argument_keypair,
                &from_keypair,
            ],
            message.clone(),
            bank.last_blockhash(),
        );
        let (result, inner_instructions, _log_messages, _executed_units) =
            process_transaction_and_record_inner(&bank, tx);
        let invoked_programs: Vec<Pubkey> = inner_instructions[0]
            .iter()
            .map(|ix| &message.account_keys[ix.instruction.program_id_index as usize])
            .cloned()
            .collect();
        assert_eq!(invoked_programs, vec![]);
        assert_eq!(
            result.unwrap_err(),
            TransactionError::InstructionError(0, InstructionError::ProgramFailedToComplete)
        );
    }
}

#[test]
#[cfg(feature = "sbf_rust")]
fn test_program_sbf_program_id_spoofing() {
    let spoof1_elf = harness::file::load_program_elf("solana_sbf_rust_spoof1");
    let spoof1_system_elf = harness::file::load_program_elf("solana_sbf_rust_spoof1_system");

    let malicious_swap_pubkey = Pubkey::new_unique();
    let malicious_system_pubkey = Pubkey::new_unique();

    let feature_set = SVMFeatureSet::all_enabled();
    let compute_budget = ComputeBudget::new_with_defaults(false, false);

    let from_pubkey = Pubkey::new_unique();
    let to_pubkey = Pubkey::new_unique();

    let accounts = vec![
        keyed_account_for_system_program(),
        (
            malicious_system_pubkey,
            Account::new(0, 0, &loader_v4::id()),
        ),
        (from_pubkey, Account::new(10, 0, &system_program::id())),
        (to_pubkey, Account::new(0, 0, &system_program::id())),
    ];

    let mut program_cache = harness::program_cache::new_with_builtins(0);
    harness::program_cache::add_program(
        &mut program_cache,
        &malicious_swap_pubkey,
        &loader_v4::id(),
        &spoof1_elf,
        &feature_set,
        &compute_budget,
    );
    harness::program_cache::add_program(
        &mut program_cache,
        &malicious_system_pubkey,
        &loader_v4::id(),
        &spoof1_system_elf,
        &feature_set,
        &compute_budget,
    );
    let sysvar_cache = default_sysvar_cache();

    let account_metas = vec![
        AccountMeta::new_readonly(system_program::id(), false),
        AccountMeta::new_readonly(malicious_system_pubkey, false),
        AccountMeta::new(from_pubkey, false),
        AccountMeta::new(to_pubkey, false),
    ];

    let instruction =
        Instruction::new_with_bytes(malicious_swap_pubkey, &[], account_metas.clone());

    let context = InstrContext {
        feature_set,
        accounts,
        instruction,
    };

    let effects =
        harness::execute_instr(context, &compute_budget, &mut program_cache, &sysvar_cache)
            .unwrap();

    assert_eq!(
        effects.result,
        Some(InstructionError::MissingRequiredSignature)
    );

    let from_account = effects.get_account(&from_pubkey).unwrap();
    assert_eq!(10, from_account.lamports);

    let to_account = effects.get_account(&to_pubkey).unwrap();
    assert_eq!(0, to_account.lamports);
}

#[test]
#[cfg(feature = "sbf_rust")]
fn test_program_sbf_caller_has_access_to_cpi_program() {
    let caller_access_elf = harness::file::load_program_elf("solana_sbf_rust_caller_access");

    let caller_pubkey = Pubkey::new_unique();
    let caller2_pubkey = Pubkey::new_unique();

    let feature_set = SVMFeatureSet::all_enabled();
    let compute_budget = ComputeBudget::new_with_defaults(false, false);

    let accounts = vec![
        (caller_pubkey, Account::new(0, 0, &loader_v4::id())),
        (caller2_pubkey, Account::new(0, 0, &loader_v4::id())),
    ];

    let mut program_cache = harness::program_cache::new_with_builtins(0);
    harness::program_cache::add_program(
        &mut program_cache,
        &caller_pubkey,
        &loader_v4::id(),
        &caller_access_elf,
        &feature_set,
        &compute_budget,
    );
    harness::program_cache::add_program(
        &mut program_cache,
        &caller2_pubkey,
        &loader_v4::id(),
        &caller_access_elf,
        &feature_set,
        &compute_budget,
    );
    let sysvar_cache = default_sysvar_cache();

    let account_metas = vec![
        AccountMeta::new_readonly(caller_pubkey, false),
        AccountMeta::new_readonly(caller2_pubkey, false),
    ];
    let instruction = Instruction::new_with_bytes(caller_pubkey, &[1], account_metas);

    let context = InstrContext {
        feature_set,
        accounts,
        instruction,
    };

    let effects =
        harness::execute_instr(context, &compute_budget, &mut program_cache, &sysvar_cache)
            .unwrap();

    assert_eq!(effects.result, Some(InstructionError::MissingAccount));
}

#[test]
#[cfg(feature = "sbf_rust")]
fn test_program_sbf_ro_modify() {
    agave_logger::setup();

    let program_elf = harness::file::load_program_elf("solana_sbf_rust_ro_modify");
    let program_id = Pubkey::new_unique();

    let feature_set = SVMFeatureSet::all_enabled();
    let compute_budget = ComputeBudget::new_with_defaults(false, false);

    let test_pubkey = Pubkey::new_unique();
    let accounts = vec![
        keyed_account_for_system_program(),
        (test_pubkey, Account::new(10, 0, &system_program::id())),
    ];

    let mut program_cache = default_program_cache_with_program(
        &program_id,
        &program_elf,
        &feature_set,
        &compute_budget,
    );
    let sysvar_cache = default_sysvar_cache();

    let account_metas = vec![
        AccountMeta::new_readonly(system_program::id(), false),
        AccountMeta::new(test_pubkey, true),
    ];

    for instr_data in [1u8, 3, 4] {
        let instruction =
            Instruction::new_with_bytes(program_id, &[instr_data], account_metas.clone());

        let context = InstrContext {
            feature_set,
            accounts: accounts.clone(),
            instruction,
        };

        let effects =
            harness::execute_instr(context, &compute_budget, &mut program_cache, &sysvar_cache)
                .unwrap();

        assert_eq!(
            effects.result,
            Some(InstructionError::ProgramFailedToComplete)
        );
    }
}

#[test]
#[cfg(feature = "sbf_rust")]
fn test_program_sbf_call_depth() {
    agave_logger::setup();

    let program_elf = harness::file::load_program_elf("solana_sbf_rust_call_depth");
    let program_id = Pubkey::new_unique();

    let feature_set = SVMFeatureSet::all_enabled();
    let compute_budget = ComputeBudget::new_with_defaults(false, false);

    let mut program_cache = default_program_cache_with_program(
        &program_id,
        &program_elf,
        &feature_set,
        &compute_budget,
    );
    let sysvar_cache = default_sysvar_cache();

    let mut execute = |depth: usize| {
        let instruction = Instruction::new_with_bincode(program_id, &depth, vec![]);

        let context = InstrContext {
            feature_set,
            accounts: vec![],
            instruction,
        };

        harness::execute_instr(context, &compute_budget, &mut program_cache, &sysvar_cache).unwrap()
    };

    let effects = execute(compute_budget.max_call_depth - 1);
    assert!(effects.result.is_none());

    let effects = execute(compute_budget.max_call_depth);
    assert!(effects.result.is_some());
}

#[test]
#[cfg(feature = "sbf_rust")]
fn test_program_sbf_compute_budget() {
    agave_logger::setup();

    let program_elf = harness::file::load_program_elf("solana_sbf_rust_noop");
    let program_id = Pubkey::new_unique();

    let feature_set = SVMFeatureSet::all_enabled();
    let mut compute_budget = ComputeBudget::new_with_defaults(false, false);
    compute_budget.compute_unit_limit = 0;

    let accounts = vec![(program_id, Account::new(0, 0, &loader_v4::id()))];

    let mut program_cache = default_program_cache_with_program(
        &program_id,
        &program_elf,
        &feature_set,
        &compute_budget,
    );
    let sysvar_cache = default_sysvar_cache();

    let instruction = Instruction::new_with_bincode(program_id, &0, vec![]);

    let context = InstrContext {
        feature_set,
        accounts,
        instruction,
    };

    let effects =
        harness::execute_instr(context, &compute_budget, &mut program_cache, &sysvar_cache)
            .unwrap();

    assert_eq!(
        effects.result,
        Some(InstructionError::ProgramFailedToComplete),
    );
}

#[test]
fn assert_instruction_count() {
    agave_logger::setup();

    let mut programs = Vec::new();
    #[cfg(feature = "sbf_c")]
    {
        programs.extend_from_slice(&[
            ("alloc", 18572),
            ("sbf_to_sbf", 316),
            ("multiple_static", 210),
            ("noop", 5),
            ("noop++", 5),
            ("relative_call", 212),
            ("return_data", 1026),
            ("sanity", 2371),
            ("sanity++", 2271),
            ("secp256k1_recover", 25421),
            ("sha", 1446),
            ("struct_pass", 108),
            ("struct_ret", 122),
        ]);
    }
    #[cfg(feature = "sbf_rust")]
    {
        programs.extend_from_slice(&[
            ("solana_sbf_rust_128bit", 784),
            ("solana_sbf_rust_alloc", 4940),
            ("solana_sbf_rust_custom_heap", 343),
            ("solana_sbf_rust_dep_crate", 22),
            ("solana_sbf_rust_iter", 1514),
            ("solana_sbf_rust_many_args", 1287),
            ("solana_sbf_rust_mem", 1326),
            ("solana_sbf_rust_membuiltins", 329),
            ("solana_sbf_rust_noop", 342),
            ("solana_sbf_rust_param_passing", 108),
            ("solana_sbf_rust_rand", 315),
            ("solana_sbf_rust_sanity", 14228),
            ("solana_sbf_rust_secp256k1_recover", 88615),
            ("solana_sbf_rust_sha", 21998),
        ]);
    }

    let feature_set = SVMFeatureSet::all_enabled();
    let compute_budget = ComputeBudget::new_with_defaults(false, false);
    let sysvar_cache = default_sysvar_cache();

    println!("\n  {:36} expected actual  diff", "SBF program");
    for (program_name, expected_consumption) in programs.iter() {
        let program_elf = harness::file::load_program_elf(program_name);
        let program_id = Pubkey::new_unique();

        let mut program_cache = default_program_cache_with_program(
            &program_id,
            &program_elf,
            &feature_set,
            &compute_budget,
        );

        let account_pubkey = Pubkey::new_unique();
        let accounts = vec![(account_pubkey, Account::new(0, 0, &program_id))];

        let instruction_accounts = vec![AccountMeta {
            pubkey: account_pubkey,
            is_signer: false,
            is_writable: false,
        }];
        let instruction = Instruction::new_with_bytes(program_id, &[], instruction_accounts);

        let context = InstrContext {
            feature_set,
            accounts,
            instruction,
        };

        let effects =
            harness::execute_instr(context, &compute_budget, &mut program_cache, &sysvar_cache)
                .unwrap();

        let consumption = compute_budget
            .compute_unit_limit
            .saturating_sub(effects.cu_avail);
        let diff: i64 = consumption as i64 - *expected_consumption as i64;
        println!(
            "  {:36} {:8}{:6} {:+5} ({:+3.0}%)",
            program_name,
            *expected_consumption,
            consumption,
            diff,
            100.0_f64 * consumption as f64 / *expected_consumption as f64 - 100.0_f64,
        );
        assert!(consumption <= *expected_consumption);
    }
}

#[test]
#[cfg(feature = "sbf_rust")]
fn test_program_sbf_instruction_introspection() {
    agave_logger::setup();

    let GenesisConfigInfo {
        genesis_config,
        mint_keypair,
        ..
    } = create_genesis_config(50_000);

    let (bank, bank_forks) = Bank::new_with_bank_forks_for_tests(&genesis_config);
    let mut bank_client = BankClient::new_shared(bank.clone());
    let authority_keypair = Keypair::new();

    let (_bank, program_id) = load_program_of_loader_v4(
        &mut bank_client,
        &bank_forks,
        &mint_keypair,
        &authority_keypair,
        "solana_sbf_rust_instruction_introspection",
    );

    // Passing transaction
    let account_metas = vec![
        AccountMeta::new_readonly(program_id, false),
        AccountMeta::new_readonly(sysvar::instructions::id(), false),
    ];
    let instruction0 = Instruction::new_with_bytes(program_id, &[0u8, 0u8], account_metas.clone());
    let instruction1 = Instruction::new_with_bytes(program_id, &[0u8, 1u8], account_metas.clone());
    let instruction2 = Instruction::new_with_bytes(program_id, &[0u8, 2u8], account_metas);
    let message = Message::new(
        &[instruction0, instruction1, instruction2],
        Some(&mint_keypair.pubkey()),
    );
    let result = bank_client.send_and_confirm_message(&[&mint_keypair], message);
    assert!(result.is_ok());

    // writable special instructions11111 key, should not be allowed
    let account_metas = vec![AccountMeta::new(sysvar::instructions::id(), false)];
    let instruction = Instruction::new_with_bytes(program_id, &[0], account_metas);
    let result = bank_client.send_and_confirm_instruction(&mint_keypair, instruction);
    assert_eq!(
        result.unwrap_err().unwrap(),
        // sysvar write locks are demoted to read only. So this will no longer
        // cause InvalidAccountIndex error.
        TransactionError::InstructionError(0, InstructionError::ProgramFailedToComplete),
    );

    // No accounts, should error
    let instruction = Instruction::new_with_bytes(program_id, &[0], vec![]);
    let result = bank_client.send_and_confirm_instruction(&mint_keypair, instruction);
    #[allow(deprecated)]
    let expected_error =
        TransactionError::InstructionError(0, InstructionError::NotEnoughAccountKeys);
    assert!(result.is_err());
    assert_eq!(result.unwrap_err().unwrap(), expected_error,);
    assert!(bank.get_account(&sysvar::instructions::id()).is_none());
}

#[test_matrix(
    [0, 1, 2, 5, 10, 15, 20],
    [1, 10, 50, 100, 255, 500, 1000, 1024]  // MAX_RETURN_DATA = 1024
)]
#[allow(clippy::arithmetic_side_effects)]
#[cfg(feature = "sbf_rust")]
fn test_program_sbf_r2_instruction_data_pointer(num_accounts: usize, input_data_len: usize) {
    agave_logger::setup();

    let program_elf =
        harness::file::load_program_elf("solana_sbf_rust_r2_instruction_data_pointer");
    let program_id = Pubkey::new_unique();

    let feature_set = SVMFeatureSet::all_enabled();
    let compute_budget = ComputeBudget::new_with_defaults(false, false);

    let mut program_cache = default_program_cache_with_program(
        &program_id,
        &program_elf,
        &feature_set,
        &compute_budget,
    );
    let sysvar_cache = default_sysvar_cache();

    let mut accounts = Vec::new();
    let mut account_metas = Vec::new();

    for i in 0..num_accounts {
        let pubkey = Pubkey::new_unique();

        // Mixed account sizes.
        accounts.push((pubkey, Account::new(0, 100 + (i * 50), &program_id)));

        // Mixed account roles.
        if i % 2 == 0 {
            account_metas.push(AccountMeta::new(pubkey, false));
        } else {
            account_metas.push(AccountMeta::new_readonly(pubkey, false));
        }
    }

    // The provided instruction data will be set to the return data.
    let input_data: Vec<u8> = (0..input_data_len).map(|i| (i % 256) as u8).collect();

    let instruction = Instruction::new_with_bytes(program_id, &input_data, account_metas);

    let context = InstrContext {
        feature_set,
        accounts,
        instruction,
    };

    let effects =
        harness::execute_instr(context, &compute_budget, &mut program_cache, &sysvar_cache)
            .unwrap();

    assert!(effects.result.is_none());
    assert_eq!(input_data, effects.return_data);
}

fn get_stable_genesis_config() -> GenesisConfigInfo {
    let validator_pubkey =
        Pubkey::from_str("GLh546CXmtZdvpEzL8sxzqhhUf7KPvmGaRpFHB5W1sjV").unwrap();
    let mint_keypair = Keypair::from_base58_string(
        "4YTH9JSRgZocmK9ezMZeJCCV2LVeR2NatTBA8AFXkg2x83fqrt8Vwyk91961E7ns4vee9yUBzuDfztb8i9iwTLFd",
    );
    let voting_keypair = Keypair::from_base58_string(
        "4EPWEn72zdNY1JSKkzyZ2vTZcKdPW3jM5WjAgUadnoz83FR5cDFApbo7s5mwBcYXn8afVe2syReJaqBi4fkhG3mH",
    );
    let stake_pubkey = Pubkey::from_str("HGq9JF77xFXRgWRJy8VQuhdbdugrT856RvQDzr1KJo6E").unwrap();

    let mut genesis_config = create_genesis_config_with_leader_ex(
        123,
        &mint_keypair.pubkey(),
        &validator_pubkey,
        &voting_keypair.pubkey(),
        &stake_pubkey,
        None,
        bootstrap_validator_stake_lamports(),
        42,
        FeeRateGovernor::new(0, 0), // most tests can't handle transaction fees
        Rent::free(),               // most tests don't expect rent
        ClusterType::Development,
        &FeatureSet::all_enabled(),
        vec![],
    );
    genesis_config.creation_time = Duration::ZERO.as_secs() as UnixTimestamp;

    GenesisConfigInfo {
        genesis_config,
        mint_keypair,
        voting_keypair,
        validator_pubkey,
    }
}

#[test]
#[ignore]
#[cfg(feature = "sbf_rust")]
fn test_program_sbf_invoke_stable_genesis_and_bank() {
    // The purpose of this test is to exercise various code branches of runtime/VM and
    // assert that the resulting bank hash matches with the expected value.
    // The assert check is commented out by default. Please refer to the last few lines
    // of the test to enable the assertion.
    agave_logger::setup();

    let GenesisConfigInfo {
        genesis_config,
        mint_keypair,
        ..
    } = get_stable_genesis_config();
    let bank = Bank::new_for_tests(&genesis_config);
    let bank = Arc::new(bank);
    let bank_client = BankClient::new_shared(bank.clone());

    // Deploy upgradeable program
    let buffer_keypair = Keypair::from_base58_string(
        "4q4UvWxh2oMifTGbChDeWCbdN8eJEUQ1E6cuNnmymJ6AN5CMUT2VW5A1RKnG9dy7ypLczB9inMUAafh5TkpXrtxg",
    );
    let program_keypair = Keypair::from_base58_string(
        "3LQpBxgpaFNJPit5a8t51pJKMkUmNUn5PhSTcuuhuuBxe43cTeqVPhMtKkFNr5VpFzCExf4ihibvuZgGxmjy6t8n",
    );
    let program_id = program_keypair.pubkey();
    let authority_keypair = Keypair::from_base58_string(
        "285XFW2NTWd6CMvtHzvYYS1kWzmzcGBnyEXbH1v8hq6YJqJsLMTYMPkbEQqeE7m7UqhoMeK5V3HMJLf9DdxwU2Gy",
    );

    let instruction =
        Instruction::new_with_bytes(program_id, &[0], vec![AccountMeta::new(clock::id(), false)]);

    // Call program before its deployed
    let result = bank_client.send_and_confirm_instruction(&mint_keypair, instruction.clone());
    assert_eq!(
        result.unwrap_err().unwrap(),
        TransactionError::ProgramAccountNotFound
    );

    #[allow(deprecated)]
    solana_runtime::loader_utils::load_upgradeable_program(
        &bank_client,
        &mint_keypair,
        &buffer_keypair,
        &program_keypair,
        &authority_keypair,
        "solana_sbf_rust_noop",
    );

    // Deploy indirect invocation program
    let indirect_program_keypair = Keypair::from_base58_string(
        "2BgE4gD5wUCwiAVPYbmWd2xzXSsD9W2fWgNjwmVkm8WL7i51vK9XAXNnX1VB6oKQZmjaUPRd5RzE6RggB9DeKbZC",
    );
    #[allow(deprecated)]
    solana_runtime::loader_utils::load_upgradeable_program(
        &bank_client,
        &mint_keypair,
        &buffer_keypair,
        &indirect_program_keypair,
        &authority_keypair,
        "solana_sbf_rust_invoke_and_return",
    );

    let invoke_instruction =
        Instruction::new_with_bytes(program_id, &[0], vec![AccountMeta::new(clock::id(), false)]);
    let indirect_invoke_instruction = Instruction::new_with_bytes(
        indirect_program_keypair.pubkey(),
        &[0],
        vec![
            AccountMeta::new_readonly(program_id, false),
            AccountMeta::new_readonly(clock::id(), false),
        ],
    );

    // Prepare redeployment
    let buffer_keypair = Keypair::from_base58_string(
        "5T5L31FiUphXh4N6mxiWhEKPrdLhvMJSbaHo1Ne7zZYkw6YT1fVkqsWdA6pHMtqATiMTc4sfx5yTV9M9AnWDoBkW",
    );
    load_upgradeable_buffer(
        &bank_client,
        &mint_keypair,
        &buffer_keypair,
        &authority_keypair,
        "solana_sbf_rust_panic",
    );
    let redeployment_instruction = loader_v3_instruction::upgrade(
        &program_id,
        &buffer_keypair.pubkey(),
        &authority_keypair.pubkey(),
        &mint_keypair.pubkey(),
    );

    // Redeployment causes programs to be unavailable to both top-level-instructions and CPI instructions
    for invoke_instruction in [invoke_instruction, indirect_invoke_instruction] {
        // Call upgradeable program
        let result =
            bank_client.send_and_confirm_instruction(&mint_keypair, invoke_instruction.clone());
        assert!(result.is_ok());

        // Upgrade the program and invoke in same tx
        let message = Message::new(
            &[redeployment_instruction.clone(), invoke_instruction],
            Some(&mint_keypair.pubkey()),
        );
        let tx = Transaction::new(
            &[&mint_keypair, &authority_keypair],
            message.clone(),
            bank.last_blockhash(),
        );
        let (result, _, _, _) = process_transaction_and_record_inner(&bank, tx);
        assert_eq!(
            result.unwrap_err(),
            TransactionError::InstructionError(1, InstructionError::InvalidAccountData),
        );
    }

    // Prepare undeployment
    let (programdata_address, _) = Pubkey::find_program_address(
        &[program_keypair.pubkey().as_ref()],
        &bpf_loader_upgradeable::id(),
    );
    let undeployment_instruction = loader_v3_instruction::close_any(
        &programdata_address,
        &mint_keypair.pubkey(),
        Some(&authority_keypair.pubkey()),
        Some(&program_id),
    );

    let invoke_instruction =
        Instruction::new_with_bytes(program_id, &[1], vec![AccountMeta::new(clock::id(), false)]);
    let indirect_invoke_instruction = Instruction::new_with_bytes(
        indirect_program_keypair.pubkey(),
        &[1],
        vec![
            AccountMeta::new_readonly(program_id, false),
            AccountMeta::new_readonly(clock::id(), false),
        ],
    );

    // Undeployment is visible to both top-level-instructions and CPI instructions
    for invoke_instruction in [invoke_instruction, indirect_invoke_instruction] {
        // Call upgradeable program
        let result =
            bank_client.send_and_confirm_instruction(&mint_keypair, invoke_instruction.clone());
        assert!(result.is_ok());

        // Undeploy the program and invoke in same tx
        let message = Message::new(
            &[undeployment_instruction.clone(), invoke_instruction],
            Some(&mint_keypair.pubkey()),
        );
        let tx = Transaction::new(
            &[&mint_keypair, &authority_keypair],
            message.clone(),
            bank.last_blockhash(),
        );
        let (result, _, _, _) = process_transaction_and_record_inner(&bank, tx);
        assert_eq!(
            result.unwrap_err(),
            TransactionError::InstructionError(1, InstructionError::InvalidAccountData),
        );
    }

    bank.freeze();
    let expected_hash = Hash::from_str("2A2vqbUKExRbnaAzSnDFXdsBZRZSpCjGZCAA3mFZG2sV")
        .expect("Failed to generate hash");
    println!("Stable test produced bank hash: {}", bank.hash());
    println!("Expected hash: {}", expected_hash);

    // Enable the following code to match the bank hash with the expected bank hash.
    // Follow these steps.
    // 1. Run this test on the baseline/master commit, and get the expected bank hash.
    // 2. Update the `expected_hash` to match the expected bank hash.
    // 3. Run the test in the PR branch that's being tested.
    // If the hash doesn't match, the PR likely has runtime changes that can lead to
    // consensus failure.
    //  assert_eq!(bank.hash(), expected_hash);
}

#[test]
#[cfg(feature = "sbf_rust")]
fn test_program_sbf_invoke_in_same_tx_as_deployment() {
    agave_logger::setup();

    let GenesisConfigInfo {
        genesis_config,
        mint_keypair,
        ..
    } = create_genesis_config(50);
    let (bank, bank_forks) = Bank::new_with_bank_forks_for_tests(&genesis_config);
    let mut bank_client = BankClient::new_shared(bank.clone());

    // Deploy upgradeable program
    let authority_keypair = Keypair::new();
    let (program_keypair, deployment_instructions) = instructions_to_load_program_of_loader_v4(
        &bank_client,
        &mint_keypair,
        &authority_keypair,
        "solana_sbf_rust_noop",
        None,
        None,
    );
    let program_id = program_keypair.pubkey();

    // Deploy indirect invocation program
    let (bank, indirect_program_id) = load_program_of_loader_v4(
        &mut bank_client,
        &bank_forks,
        &mint_keypair,
        &authority_keypair,
        "solana_sbf_rust_invoke_and_return",
    );

    // Prepare invocations
    let invoke_instruction =
        Instruction::new_with_bytes(program_id, &[0], vec![AccountMeta::new(clock::id(), false)]);
    let indirect_invoke_instruction = Instruction::new_with_bytes(
        indirect_program_id,
        &[0],
        vec![
            AccountMeta::new_readonly(program_id, false),
            AccountMeta::new_readonly(clock::id(), false),
        ],
    );

    // Deployment is invisible to both top-level-instructions and CPI instructions
    for (index, invoke_instruction) in [invoke_instruction, indirect_invoke_instruction]
        .into_iter()
        .enumerate()
    {
        let mut instructions = deployment_instructions.clone();
        instructions.push(invoke_instruction);
        let tx = Transaction::new(
            &[&mint_keypair, &program_keypair, &authority_keypair],
            Message::new(&instructions, Some(&mint_keypair.pubkey())),
            bank.last_blockhash(),
        );
        if index == 0 {
            let result = load_execute_and_commit_transaction(&bank, tx);
            assert_eq!(
                result.unwrap().status,
                Err(TransactionError::ProgramAccountNotFound),
            );
        } else {
            let (result, _, _, _) = process_transaction_and_record_inner(&bank, tx);
            if let TransactionError::InstructionError(instr_no, ty) = result.unwrap_err() {
                // Asserting the instruction number as an upper bound, since the quantity of
                // instructions depends on the program size, which in turn depends on the SBPF
                // versions.
                assert!(instr_no <= 40);
                assert_eq!(ty, InstructionError::UnsupportedProgramId);
            } else {
                panic!("Invalid error type");
            }
        }
    }
}

#[test]
#[cfg(feature = "sbf_rust")]
fn test_program_sbf_invoke_in_same_tx_as_redeployment() {
    agave_logger::setup();

    let GenesisConfigInfo {
        genesis_config,
        mint_keypair,
        ..
    } = create_genesis_config(50);
    let (bank, bank_forks) = Bank::new_with_bank_forks_for_tests(&genesis_config);
    let mut bank_client = BankClient::new_shared(bank.clone());

    // Deploy upgradeable program
    let authority_keypair = Keypair::new();
    let (_bank, program_id) = load_program_of_loader_v4(
        &mut bank_client,
        &bank_forks,
        &mint_keypair,
        &authority_keypair,
        "solana_sbf_rust_noop",
    );
    let (source_program_keypair, mut deployment_instructions) =
        instructions_to_load_program_of_loader_v4(
            &bank_client,
            &mint_keypair,
            &authority_keypair,
            "solana_sbf_rust_panic",
            None,
            Some(&program_id),
        );
    let undeployment_instruction =
        loader_v4_instruction::retract(&program_id, &authority_keypair.pubkey());
    let redeployment_instructions =
        deployment_instructions.split_off(deployment_instructions.len() - 3);
    let signers: &[&[&Keypair]] = &[
        &[&mint_keypair, &source_program_keypair],
        &[&mint_keypair, &authority_keypair],
    ];
    let signers = std::iter::once(signers[0]).chain(std::iter::repeat(signers[1]));
    for (instruction, signers) in deployment_instructions.into_iter().zip(signers) {
        let message = Message::new(&[instruction], Some(&mint_keypair.pubkey()));
        bank_client
            .send_and_confirm_message(signers, message)
            .unwrap();
    }

    // Deploy indirect invocation program
    let (bank, indirect_program_id) = load_program_of_loader_v4(
        &mut bank_client,
        &bank_forks,
        &mint_keypair,
        &authority_keypair,
        "solana_sbf_rust_invoke_and_return",
    );

    // Prepare invocations
    let invoke_instruction =
        Instruction::new_with_bytes(program_id, &[0], vec![AccountMeta::new(clock::id(), false)]);
    let indirect_invoke_instruction = Instruction::new_with_bytes(
        indirect_program_id,
        &[0],
        vec![
            AccountMeta::new_readonly(program_id, false),
            AccountMeta::new_readonly(clock::id(), false),
        ],
    );

    // Redeployment fails when top-level-instructions invoke the program because of write lock demotion
    // and the program becomes unavailable to CPI instructions
    for (invoke_instruction, expected_error) in [
        (
            invoke_instruction,
            TransactionError::InstructionError(0, InstructionError::InvalidArgument),
        ),
        (
            indirect_invoke_instruction,
            TransactionError::InstructionError(4, InstructionError::UnsupportedProgramId),
        ),
    ] {
        // Call upgradeable program
        let result =
            bank_client.send_and_confirm_instruction(&mint_keypair, invoke_instruction.clone());
        assert!(result.is_ok());

        // Upgrade the program and invoke in same tx
        let message = Message::new(
            &[
                undeployment_instruction.clone(),
                redeployment_instructions[0].clone(),
                redeployment_instructions[1].clone(),
                redeployment_instructions[2].clone(),
                invoke_instruction,
            ],
            Some(&mint_keypair.pubkey()),
        );
        let tx = Transaction::new(
            &[&mint_keypair, &authority_keypair],
            message.clone(),
            bank.last_blockhash(),
        );
        let (result, _, _, _) = process_transaction_and_record_inner(&bank, tx);
        assert_eq!(result.unwrap_err(), expected_error,);
    }
}

#[test]
#[cfg(feature = "sbf_rust")]
fn test_program_sbf_invoke_in_same_tx_as_undeployment() {
    agave_logger::setup();

    let GenesisConfigInfo {
        genesis_config,
        mint_keypair,
        ..
    } = create_genesis_config(50);
    let (bank, bank_forks) = Bank::new_with_bank_forks_for_tests(&genesis_config);
    let mut bank_client = BankClient::new_shared(bank.clone());

    // Deploy upgradeable program
    let authority_keypair = Keypair::new();
    let (_bank, program_id) = load_program_of_loader_v4(
        &mut bank_client,
        &bank_forks,
        &mint_keypair,
        &authority_keypair,
        "solana_sbf_rust_noop",
    );

    // Deploy indirect invocation program
    let (bank, indirect_program_id) = load_program_of_loader_v4(
        &mut bank_client,
        &bank_forks,
        &mint_keypair,
        &authority_keypair,
        "solana_sbf_rust_invoke_and_return",
    );

    // Prepare invocations
    let invoke_instruction =
        Instruction::new_with_bytes(program_id, &[0], vec![AccountMeta::new(clock::id(), false)]);
    let indirect_invoke_instruction = Instruction::new_with_bytes(
        indirect_program_id,
        &[0],
        vec![
            AccountMeta::new_readonly(program_id, false),
            AccountMeta::new_readonly(clock::id(), false),
        ],
    );

    // Prepare undeployment
    let undeployment_instruction =
        loader_v4_instruction::retract(&program_id, &authority_keypair.pubkey());

    // Undeployment fails when top-level-instructions invoke the program because of write lock demotion
    // and the program becomes unavailable to CPI instructions
    for (invoke_instruction, expected_error) in [
        (
            invoke_instruction,
            TransactionError::InstructionError(0, InstructionError::InvalidArgument),
        ),
        (
            indirect_invoke_instruction,
            TransactionError::InstructionError(1, InstructionError::UnsupportedProgramId),
        ),
    ] {
        // Call upgradeable program
        let result =
            bank_client.send_and_confirm_instruction(&mint_keypair, invoke_instruction.clone());
        assert!(result.is_ok());

        // Upgrade the program and invoke in same tx
        let message = Message::new(
            &[undeployment_instruction.clone(), invoke_instruction],
            Some(&mint_keypair.pubkey()),
        );
        let tx = Transaction::new(
            &[&mint_keypair, &authority_keypair],
            message.clone(),
            bank.last_blockhash(),
        );
        let (result, _, _, _) = process_transaction_and_record_inner(&bank, tx);
        assert_eq!(result.unwrap_err(), expected_error,);
    }
}

#[test]
#[cfg(any(feature = "sbf_c", feature = "sbf_rust"))]
fn test_program_sbf_disguised_as_sbf_loader() {
    agave_logger::setup();

    let mut programs = Vec::new();
    #[cfg(feature = "sbf_c")]
    {
        programs.extend_from_slice(&[("noop")]);
    }
    #[cfg(feature = "sbf_rust")]
    {
        programs.extend_from_slice(&[("solana_sbf_rust_noop")]);
    }

    for program in programs.iter() {
        let program_elf = harness::file::load_program_elf(program);
        let program_id = Pubkey::new_unique();

        let feature_set = SVMFeatureSet {
            remove_bpf_loader_incorrect_program_id: false,
            ..SVMFeatureSet::all_enabled()
        };
        let compute_budget = ComputeBudget::new_with_defaults(false, false);

        let mut program_cache = default_program_cache();
        harness::program_cache::add_program(
            &mut program_cache,
            &program_id,
            &bpf_loader::id(),
            &program_elf,
            &feature_set,
            &compute_budget,
        );
        let sysvar_cache = default_sysvar_cache();

        let account_metas = vec![AccountMeta::new_readonly(program_id, false)];
        let instruction = Instruction::new_with_bytes(bpf_loader::id(), &[1], account_metas);

        let context = InstrContext {
            feature_set,
            accounts: vec![],
            instruction,
        };

        let effects =
            harness::execute_instr(context, &compute_budget, &mut program_cache, &sysvar_cache)
                .unwrap();
        assert_eq!(effects.result, Some(InstructionError::UnsupportedProgramId));
    }
}

#[test]
#[cfg(feature = "sbf_c")]
fn test_program_reads_from_program_account() {
    use solana_loader_v4_interface::state::LoaderV4State;
    agave_logger::setup();

    let program_elf = harness::file::load_program_elf("read_program");
    let program_id = Pubkey::new_unique();

    let feature_set = SVMFeatureSet::all_enabled();
    let compute_budget = ComputeBudget::new_with_defaults(false, false);

    let mut program_cache = harness::program_cache::new_with_builtins(0);
    harness::program_cache::add_program(
        &mut program_cache,
        &program_id,
        &solana_sdk_ids::loader_v4::id(),
        &program_elf,
        &feature_set,
        &compute_budget,
    );

    let sysvar_cache = default_sysvar_cache();

    // Build the program account data: LoaderV4State header + ELF bytes
    let loader_state = LoaderV4State {
        slot: 0,
        authority_address_or_next_version: Pubkey::default(),
        status: solana_loader_v4_interface::state::LoaderV4Status::Deployed,
    };
    let state_bytes: &[u8; LoaderV4State::program_data_offset()] = unsafe {
        std::mem::transmute::<&LoaderV4State, &[u8; LoaderV4State::program_data_offset()]>(
            &loader_state,
        )
    };
    let mut program_account_data = state_bytes.to_vec();
    program_account_data.extend_from_slice(&program_elf);

    let program_account = Account {
        lamports: 1,
        data: program_account_data,
        owner: solana_sdk_ids::loader_v4::id(),
        executable: true,
        rent_epoch: u64::MAX,
    };

    let account_metas = vec![AccountMeta::new_readonly(program_id, false)];
    let instruction = Instruction::new_with_bytes(program_id, state_bytes, account_metas);

    let context = InstrContext {
        feature_set,
        accounts: vec![(program_id, program_account)],
        instruction,
    };

    let effects =
        harness::execute_instr(context, &compute_budget, &mut program_cache, &sysvar_cache)
            .unwrap();
    assert!(effects.result.is_none());
}

#[test]
#[cfg(feature = "sbf_c")]
fn test_program_sbf_c_dup() {
    agave_logger::setup();

    let program_elf = harness::file::load_program_elf("ser");
    let program_id = Pubkey::new_unique();

    let feature_set = SVMFeatureSet::all_enabled();
    let compute_budget = ComputeBudget::new_with_defaults(false, false);
    let mut program_cache = harness::program_cache::new_with_builtins(0);
    harness::program_cache::add_program(
        &mut program_cache,
        &program_id,
        &solana_sdk_ids::loader_v4::id(),
        &program_elf,
        &feature_set,
        &compute_budget,
    );

    let sysvar_cache = default_sysvar_cache();

    let account_address = Pubkey::new_unique();
    let account =
        Account::new_data(42, &[1_u8, 2, 3], &solana_sdk_ids::system_program::id()).unwrap();

    let account_metas = vec![
        AccountMeta::new_readonly(account_address, false),
        AccountMeta::new_readonly(account_address, false),
    ];
    let instruction = Instruction::new_with_bytes(program_id, &[4, 5, 6, 7], account_metas);

    let context = InstrContext {
        feature_set,
        accounts: vec![(account_address, account)],
        instruction,
    };

    let effects =
        harness::execute_instr(context, &compute_budget, &mut program_cache, &sysvar_cache)
            .unwrap();
    assert!(effects.result.is_none());
}

#[test]
#[cfg(feature = "sbf_rust")]
fn test_program_sbf_upgrade() {
    agave_logger::setup();

    let GenesisConfigInfo {
        genesis_config,
        mint_keypair,
        ..
    } = create_genesis_config(50);
    let (bank, bank_forks) = Bank::new_with_bank_forks_for_tests(&genesis_config);
    let mut bank_client = BankClient::new_shared(bank);

    // Deploy upgrade program
    let authority_keypair = Keypair::new();
    let (_bank, program_id) = load_program_of_loader_v4(
        &mut bank_client,
        &bank_forks,
        &mint_keypair,
        &authority_keypair,
        "solana_sbf_rust_upgradeable",
    );

    // Call upgradeable program
    let mut instruction =
        Instruction::new_with_bytes(program_id, &[0], vec![AccountMeta::new(clock::id(), false)]);
    let result = bank_client.send_and_confirm_instruction(&mint_keypair, instruction.clone());
    assert_eq!(
        result.unwrap_err().unwrap(),
        TransactionError::InstructionError(0, InstructionError::Custom(42))
    );

    // Set authority
    let new_authority_keypair = Keypair::new();
    let authority_instruction = loader_v4_instruction::transfer_authority(
        &program_id,
        &authority_keypair.pubkey(),
        &new_authority_keypair.pubkey(),
    );
    let message = Message::new(&[authority_instruction], Some(&mint_keypair.pubkey()));
    bank_client
        .send_and_confirm_message(
            &[&mint_keypair, &authority_keypair, &new_authority_keypair],
            message,
        )
        .unwrap();

    // Upgrade program
    let (source_program_keypair, mut deployment_instructions) =
        instructions_to_load_program_of_loader_v4(
            &bank_client,
            &mint_keypair,
            &new_authority_keypair,
            "solana_sbf_rust_upgraded",
            None,
            Some(&program_id),
        );
    deployment_instructions.insert(
        deployment_instructions.len() - 3,
        loader_v4_instruction::retract(&program_id, &new_authority_keypair.pubkey()),
    );
    let signers: &[&[&Keypair]] = &[
        &[&mint_keypair, &source_program_keypair],
        &[&mint_keypair, &new_authority_keypair],
    ];
    let signers = std::iter::once(signers[0]).chain(std::iter::repeat(signers[1]));
    for (instruction, signers) in deployment_instructions.into_iter().zip(signers) {
        let message = Message::new(&[instruction], Some(&mint_keypair.pubkey()));
        bank_client
            .send_and_confirm_message(signers, message)
            .unwrap();
    }
    bank_client
        .advance_slot(1, &bank_forks, &Pubkey::default())
        .expect("Failed to advance the slot");

    // Call upgraded program
    instruction.data[0] += 1;
    let result = bank_client.send_and_confirm_instruction(&mint_keypair, instruction.clone());
    assert_eq!(
        result.unwrap_err().unwrap(),
        TransactionError::InstructionError(0, InstructionError::Custom(43))
    );
}

#[test]
#[cfg(feature = "sbf_rust")]
fn test_program_sbf_upgrade_via_cpi() {
    agave_logger::setup();

    let GenesisConfigInfo {
        genesis_config,
        mint_keypair,
        ..
    } = create_genesis_config(50);

    let (bank, bank_forks) = Bank::new_with_bank_forks_for_tests(&genesis_config);
    let mut bank_client = BankClient::new_shared(bank);
    let authority_keypair = Keypair::new();

    let (_bank, invoke_and_return) = load_program_of_loader_v4(
        &mut bank_client,
        &bank_forks,
        &mint_keypair,
        &authority_keypair,
        "solana_sbf_rust_invoke_and_return",
    );

    // Deploy upgradeable program
    let authority_keypair = Keypair::new();
    let (_bank, program_id) = load_program_of_loader_v4(
        &mut bank_client,
        &bank_forks,
        &mint_keypair,
        &authority_keypair,
        "solana_sbf_rust_upgradeable",
    );

    // Call the upgradable program via CPI
    let mut instruction = Instruction::new_with_bytes(
        invoke_and_return,
        &[0],
        vec![
            AccountMeta::new_readonly(program_id, false),
            AccountMeta::new_readonly(clock::id(), false),
        ],
    );
    instruction.data[0] += 1;
    let result = bank_client.send_and_confirm_instruction(&mint_keypair, instruction.clone());
    assert_eq!(
        result.unwrap_err().unwrap(),
        TransactionError::InstructionError(0, InstructionError::Custom(42))
    );

    // Set authority via CPI
    let new_authority_keypair = Keypair::new();
    let mut authority_instruction = loader_v4_instruction::transfer_authority(
        &program_id,
        &authority_keypair.pubkey(),
        &new_authority_keypair.pubkey(),
    );
    authority_instruction.program_id = invoke_and_return;
    authority_instruction
        .accounts
        .insert(0, AccountMeta::new(loader_v4::id(), false));
    let message = Message::new(&[authority_instruction], Some(&mint_keypair.pubkey()));
    bank_client
        .send_and_confirm_message(
            &[&mint_keypair, &authority_keypair, &new_authority_keypair],
            message,
        )
        .unwrap();

    // Upgrade program via CPI
    let (source_program_keypair, mut deployment_instructions) =
        instructions_to_load_program_of_loader_v4(
            &bank_client,
            &mint_keypair,
            &new_authority_keypair,
            "solana_sbf_rust_upgraded",
            None,
            Some(&program_id),
        );
    deployment_instructions.insert(
        deployment_instructions.len() - 3,
        loader_v4_instruction::retract(&program_id, &new_authority_keypair.pubkey()),
    );
    let mut upgrade_instruction = deployment_instructions.pop().unwrap();
    let signers: &[&[&Keypair]] = &[
        &[&mint_keypair, &source_program_keypair],
        &[&mint_keypair, &new_authority_keypair],
    ];
    let signers = std::iter::once(signers[0]).chain(std::iter::repeat(signers[1]));
    for (instruction, signers) in deployment_instructions.into_iter().zip(signers) {
        let message = Message::new(&[instruction], Some(&mint_keypair.pubkey()));
        bank_client
            .send_and_confirm_message(signers, message)
            .unwrap();
    }
    upgrade_instruction.program_id = invoke_and_return;
    upgrade_instruction
        .accounts
        .insert(0, AccountMeta::new(loader_v4::id(), false));
    let message = Message::new(&[upgrade_instruction], Some(&mint_keypair.pubkey()));
    bank_client
        .send_and_confirm_message(&[&mint_keypair, &new_authority_keypair], message)
        .unwrap();
    bank_client
        .advance_slot(1, &bank_forks, &Pubkey::default())
        .expect("Failed to advance the slot");

    // Call the upgraded program via CPI
    instruction.data[0] += 1;
    let result = bank_client.send_and_confirm_instruction(&mint_keypair, instruction.clone());
    assert_eq!(
        result.unwrap_err().unwrap(),
        TransactionError::InstructionError(0, InstructionError::Custom(43))
    );
}

#[test]
#[cfg(feature = "sbf_rust")]
fn test_program_sbf_ro_account_modify() {
    agave_logger::setup();

    let program_elf = harness::file::load_program_elf("solana_sbf_rust_ro_account_modify");
    let program_id = Pubkey::new_unique();

    let feature_set = SVMFeatureSet::all_enabled();
    let compute_budget = ComputeBudget::new_with_defaults(false, false);

    let mut program_cache = default_program_cache_with_program(
        &program_id,
        &program_elf,
        &feature_set,
        &compute_budget,
    );
    let sysvar_cache = default_sysvar_cache();

    let argument_pubkey = Pubkey::new_unique();
    let accounts = vec![(argument_pubkey, Account::new(42, 100, &program_id))];

    let account_metas = vec![
        AccountMeta::new_readonly(argument_pubkey, false),
        AccountMeta::new_readonly(program_id, false),
    ];

    for case in [0, 1, 2] {
        let instruction = Instruction::new_with_bytes(program_id, &[case], account_metas.clone());

        let context = InstrContext {
            feature_set,
            accounts: accounts.clone(),
            instruction,
        };

        let effects =
            harness::execute_instr(context, &compute_budget, &mut program_cache, &sysvar_cache)
                .unwrap();

        assert_eq!(effects.result, Some(InstructionError::ReadonlyDataModified));
    }
}

#[test]
#[cfg(feature = "sbf_rust")]
fn test_program_sbf_realloc() {
    agave_logger::setup();

    const START_BALANCE: u64 = 100_000_000_000;

    let GenesisConfigInfo {
        genesis_config,
        mint_keypair,
        ..
    } = create_genesis_config(1_000_000_000_000);

    let mint_pubkey = mint_keypair.pubkey();
    let signer = &[&mint_keypair];
    for virtual_address_space_adjustments in [false, true] {
        let mut bank = Bank::new_for_tests(&genesis_config);
        let feature_set = Arc::make_mut(&mut bank.feature_set);
        // by default test banks have all features enabled, so we only need to
        // disable when needed
        if !virtual_address_space_adjustments {
            feature_set.deactivate(&feature_set::syscall_parameter_address_restrictions::id());
            feature_set.deactivate(&feature_set::virtual_address_space_adjustments::id());
            feature_set.deactivate(&feature_set::account_data_direct_mapping::id());
        }
        let (bank, bank_forks) = bank.wrap_with_bank_forks_for_tests();
        let mut bank_client = BankClient::new_shared(bank.clone());
        let authority_keypair = Keypair::new();

        let (bank, program_id) = load_program_of_loader_v4(
            &mut bank_client,
            &bank_forks,
            &mint_keypair,
            &authority_keypair,
            "solana_sbf_rust_realloc",
        );

        let mut bump = 0;
        let keypair = Keypair::new();
        let pubkey = keypair.pubkey();
        let account = AccountSharedData::new(START_BALANCE, 5, &program_id);
        bank.store_account(&pubkey, &account);

        // Realloc RO account
        let mut instruction = realloc(&program_id, &pubkey, 0, &mut bump);
        instruction.accounts[0].is_writable = false;
        assert_eq!(
            bank_client
                .send_and_confirm_message(
                    signer,
                    Message::new(
                        &[
                            instruction,
                            ComputeBudgetInstruction::set_loaded_accounts_data_size_limit(
                                LOADED_ACCOUNTS_DATA_SIZE_LIMIT_FOR_TEST
                            ),
                        ],
                        Some(&mint_pubkey),
                    ),
                )
                .unwrap_err()
                .unwrap(),
            TransactionError::InstructionError(0, InstructionError::ReadonlyDataModified)
        );

        // Realloc account to overflow
        assert_eq!(
            bank_client
                .send_and_confirm_message(
                    signer,
                    Message::new(
                        &[
                            realloc(&program_id, &pubkey, usize::MAX, &mut bump),
                            ComputeBudgetInstruction::set_loaded_accounts_data_size_limit(
                                LOADED_ACCOUNTS_DATA_SIZE_LIMIT_FOR_TEST
                            ),
                        ],
                        Some(&mint_pubkey),
                    ),
                )
                .unwrap_err()
                .unwrap(),
            TransactionError::InstructionError(0, InstructionError::InvalidRealloc)
        );

        // Realloc account to 0
        bank_client
            .send_and_confirm_message(
                signer,
                Message::new(
                    &[
                        realloc(&program_id, &pubkey, 0, &mut bump),
                        ComputeBudgetInstruction::set_loaded_accounts_data_size_limit(
                            LOADED_ACCOUNTS_DATA_SIZE_LIMIT_FOR_TEST,
                        ),
                    ],
                    Some(&mint_pubkey),
                ),
            )
            .unwrap();
        let data = bank_client.get_account_data(&pubkey).unwrap().unwrap();
        assert_eq!(0, data.len());

        // Realloc account to max then undo
        bank_client
            .send_and_confirm_message(
                signer,
                Message::new(
                    &[
                        realloc_extend_and_undo(
                            &program_id,
                            &pubkey,
                            MAX_PERMITTED_DATA_INCREASE,
                            &mut bump,
                        ),
                        ComputeBudgetInstruction::set_loaded_accounts_data_size_limit(
                            LOADED_ACCOUNTS_DATA_SIZE_LIMIT_FOR_TEST,
                        ),
                    ],
                    Some(&mint_pubkey),
                ),
            )
            .unwrap();
        let data = bank_client.get_account_data(&pubkey).unwrap().unwrap();
        assert_eq!(0, data.len());

        // Realloc account to max + 1 then undo
        assert_eq!(
            bank_client
                .send_and_confirm_message(
                    signer,
                    Message::new(
                        &[
                            realloc_extend_and_undo(
                                &program_id,
                                &pubkey,
                                MAX_PERMITTED_DATA_INCREASE + 1,
                                &mut bump,
                            ),
                            ComputeBudgetInstruction::set_loaded_accounts_data_size_limit(
                                LOADED_ACCOUNTS_DATA_SIZE_LIMIT_FOR_TEST
                            ),
                        ],
                        Some(&mint_pubkey),
                    ),
                )
                .unwrap_err()
                .unwrap(),
            TransactionError::InstructionError(0, InstructionError::InvalidRealloc)
        );

        // Realloc to max + 1
        assert_eq!(
            bank_client
                .send_and_confirm_message(
                    signer,
                    Message::new(
                        &[
                            realloc(
                                &program_id,
                                &pubkey,
                                MAX_PERMITTED_DATA_INCREASE + 1,
                                &mut bump
                            ),
                            ComputeBudgetInstruction::set_loaded_accounts_data_size_limit(
                                LOADED_ACCOUNTS_DATA_SIZE_LIMIT_FOR_TEST
                            ),
                        ],
                        Some(&mint_pubkey),
                    ),
                )
                .unwrap_err()
                .unwrap(),
            TransactionError::InstructionError(0, InstructionError::InvalidRealloc)
        );

        // Realloc to max length in max increase increments
        for i in 0..MAX_PERMITTED_DATA_LENGTH as usize / MAX_PERMITTED_DATA_INCREASE {
            let mut bump = i as u64;
            bank_client
                .send_and_confirm_message(
                    signer,
                    Message::new(
                        &[
                            realloc_extend_and_fill(
                                &program_id,
                                &pubkey,
                                MAX_PERMITTED_DATA_INCREASE,
                                1,
                                &mut bump,
                            ),
                            ComputeBudgetInstruction::set_loaded_accounts_data_size_limit(
                                LOADED_ACCOUNTS_DATA_SIZE_LIMIT_FOR_TEST,
                            ),
                        ],
                        Some(&mint_pubkey),
                    ),
                )
                .unwrap();
            let data = bank_client.get_account_data(&pubkey).unwrap().unwrap();
            assert_eq!((i + 1) * MAX_PERMITTED_DATA_INCREASE, data.len());
        }
        for i in 0..data.len() {
            assert_eq!(data[i], 1);
        }

        // and one more time should fail
        assert_eq!(
            bank_client
                .send_and_confirm_message(
                    signer,
                    Message::new(
                        &[
                            realloc_extend(
                                &program_id,
                                &pubkey,
                                MAX_PERMITTED_DATA_INCREASE,
                                &mut bump
                            ),
                            ComputeBudgetInstruction::set_loaded_accounts_data_size_limit(
                                LOADED_ACCOUNTS_DATA_SIZE_LIMIT_FOR_TEST
                            ),
                        ],
                        Some(&mint_pubkey),
                    )
                )
                .unwrap_err()
                .unwrap(),
            TransactionError::InstructionError(0, InstructionError::InvalidRealloc)
        );

        // Realloc to 6 bytes
        bank_client
            .send_and_confirm_message(
                signer,
                Message::new(
                    &[
                        realloc(&program_id, &pubkey, 6, &mut bump),
                        ComputeBudgetInstruction::set_loaded_accounts_data_size_limit(
                            LOADED_ACCOUNTS_DATA_SIZE_LIMIT_FOR_TEST,
                        ),
                    ],
                    Some(&mint_pubkey),
                ),
            )
            .unwrap();
        let data = bank_client.get_account_data(&pubkey).unwrap().unwrap();
        assert_eq!(6, data.len());

        // Extend by 2 bytes and write a u64. This ensures that we can do writes that span the original
        // account length (6 bytes) and the realloc data (2 bytes).
        bank_client
            .send_and_confirm_message(
                signer,
                Message::new(
                    &[
                        extend_and_write_u64(&program_id, &pubkey, 0x1122334455667788),
                        ComputeBudgetInstruction::set_loaded_accounts_data_size_limit(
                            LOADED_ACCOUNTS_DATA_SIZE_LIMIT_FOR_TEST,
                        ),
                    ],
                    Some(&mint_pubkey),
                ),
            )
            .unwrap();
        let data = bank_client.get_account_data(&pubkey).unwrap().unwrap();
        assert_eq!(8, data.len());
        assert_eq!(0x1122334455667788, unsafe { *data.as_ptr().cast::<u64>() });

        // Realloc to 0
        bank_client
            .send_and_confirm_message(
                signer,
                Message::new(
                    &[
                        realloc(&program_id, &pubkey, 0, &mut bump),
                        ComputeBudgetInstruction::set_loaded_accounts_data_size_limit(
                            LOADED_ACCOUNTS_DATA_SIZE_LIMIT_FOR_TEST,
                        ),
                    ],
                    Some(&mint_pubkey),
                ),
            )
            .unwrap();
        let data = bank_client.get_account_data(&pubkey).unwrap().unwrap();
        assert_eq!(0, data.len());

        // Realloc and assign
        bank_client
            .send_and_confirm_message(
                signer,
                Message::new(
                    &[
                        Instruction::new_with_bytes(
                            program_id,
                            &[REALLOC_AND_ASSIGN],
                            vec![AccountMeta::new(pubkey, false)],
                        ),
                        ComputeBudgetInstruction::set_loaded_accounts_data_size_limit(
                            LOADED_ACCOUNTS_DATA_SIZE_LIMIT_FOR_TEST,
                        ),
                    ],
                    Some(&mint_pubkey),
                ),
            )
            .unwrap();
        let account = bank.get_account(&pubkey).unwrap();
        assert_eq!(&solana_system_interface::program::id(), account.owner());
        let data = bank_client.get_account_data(&pubkey).unwrap().unwrap();
        assert_eq!(MAX_PERMITTED_DATA_INCREASE, data.len());

        // Realloc to 0 with wrong owner
        assert_eq!(
            bank_client
                .send_and_confirm_message(
                    signer,
                    Message::new(
                        &[
                            realloc(&program_id, &pubkey, 0, &mut bump),
                            ComputeBudgetInstruction::set_loaded_accounts_data_size_limit(
                                LOADED_ACCOUNTS_DATA_SIZE_LIMIT_FOR_TEST
                            ),
                        ],
                        Some(&mint_pubkey),
                    ),
                )
                .unwrap_err()
                .unwrap(),
            TransactionError::InstructionError(0, InstructionError::AccountDataSizeChanged)
        );

        // realloc and assign to self via cpi
        assert_eq!(
            bank_client
                .send_and_confirm_message(
                    &[&mint_keypair, &keypair],
                    Message::new(
                        &[
                            Instruction::new_with_bytes(
                                program_id,
                                &[REALLOC_AND_ASSIGN_TO_SELF_VIA_SYSTEM_PROGRAM],
                                vec![
                                    AccountMeta::new(pubkey, true),
                                    AccountMeta::new(solana_system_interface::program::id(), false),
                                ],
                            ),
                            ComputeBudgetInstruction::set_loaded_accounts_data_size_limit(
                                LOADED_ACCOUNTS_DATA_SIZE_LIMIT_FOR_TEST
                            ),
                        ],
                        Some(&mint_pubkey),
                    )
                )
                .unwrap_err()
                .unwrap(),
            TransactionError::InstructionError(0, InstructionError::AccountDataSizeChanged)
        );

        // Assign to self and realloc via cpi
        bank_client
            .send_and_confirm_message(
                &[&mint_keypair, &keypair],
                Message::new(
                    &[
                        Instruction::new_with_bytes(
                            program_id,
                            &[ASSIGN_TO_SELF_VIA_SYSTEM_PROGRAM_AND_REALLOC],
                            vec![
                                AccountMeta::new(pubkey, true),
                                AccountMeta::new(solana_system_interface::program::id(), false),
                            ],
                        ),
                        ComputeBudgetInstruction::set_loaded_accounts_data_size_limit(
                            LOADED_ACCOUNTS_DATA_SIZE_LIMIT_FOR_TEST,
                        ),
                    ],
                    Some(&mint_pubkey),
                ),
            )
            .unwrap();
        let account = bank.get_account(&pubkey).unwrap();
        assert_eq!(&program_id, account.owner());
        let data = bank_client.get_account_data(&pubkey).unwrap().unwrap();
        assert_eq!(2 * MAX_PERMITTED_DATA_INCREASE, data.len());

        // Realloc to 0
        bank_client
            .send_and_confirm_message(
                signer,
                Message::new(
                    &[
                        realloc(&program_id, &pubkey, 0, &mut bump),
                        ComputeBudgetInstruction::set_loaded_accounts_data_size_limit(
                            LOADED_ACCOUNTS_DATA_SIZE_LIMIT_FOR_TEST,
                        ),
                    ],
                    Some(&mint_pubkey),
                ),
            )
            .unwrap();
        let data = bank_client.get_account_data(&pubkey).unwrap().unwrap();
        assert_eq!(0, data.len());

        // zero-init
        bank_client
            .send_and_confirm_message(
                &[&mint_keypair, &keypair],
                Message::new(
                    &[
                        Instruction::new_with_bytes(
                            program_id,
                            &[ZERO_INIT],
                            vec![AccountMeta::new(pubkey, true)],
                        ),
                        ComputeBudgetInstruction::set_loaded_accounts_data_size_limit(
                            LOADED_ACCOUNTS_DATA_SIZE_LIMIT_FOR_TEST,
                        ),
                    ],
                    Some(&mint_pubkey),
                ),
            )
            .unwrap();
    }
}

#[test]
#[cfg(feature = "sbf_rust")]
fn test_program_sbf_realloc_invoke() {
    agave_logger::setup();

    const START_BALANCE: u64 = 100_000_000_000;

    let GenesisConfigInfo {
        mut genesis_config,
        mint_keypair,
        ..
    } = create_genesis_config(1_000_000_000_000);
    genesis_config.rent = Rent::default();

    let mint_pubkey = mint_keypair.pubkey();
    let signer = &[&mint_keypair];

    let (bank, bank_forks) = Bank::new_with_bank_forks_for_tests(&genesis_config);
    let mut bank_client = BankClient::new_shared(bank.clone());
    let authority_keypair = Keypair::new();

    let (_bank, realloc_program_id) = load_program_of_loader_v4(
        &mut bank_client,
        &bank_forks,
        &mint_keypair,
        &authority_keypair,
        "solana_sbf_rust_realloc",
    );
    let (bank, realloc_invoke_program_id) = load_program_of_loader_v4(
        &mut bank_client,
        &bank_forks,
        &mint_keypair,
        &authority_keypair,
        "solana_sbf_rust_realloc_invoke",
    );

    let mut bump = 0;
    let keypair = Keypair::new();
    let pubkey = keypair.pubkey().clone();
    let account = AccountSharedData::new(START_BALANCE, 5, &realloc_program_id);
    bank.store_account(&pubkey, &account);
    let invoke_keypair = Keypair::new();
    let invoke_pubkey = invoke_keypair.pubkey().clone();

    // Realloc RO account
    assert_eq!(
        bank_client
            .send_and_confirm_message(
                signer,
                Message::new(
                    &[
                        Instruction::new_with_bytes(
                            realloc_invoke_program_id,
                            &[INVOKE_REALLOC_ZERO_RO],
                            vec![
                                AccountMeta::new_readonly(pubkey, false),
                                AccountMeta::new_readonly(realloc_program_id, false),
                            ],
                        ),
                        ComputeBudgetInstruction::set_loaded_accounts_data_size_limit(
                            LOADED_ACCOUNTS_DATA_SIZE_LIMIT_FOR_TEST
                        ),
                    ],
                    Some(&mint_pubkey),
                )
            )
            .unwrap_err()
            .unwrap(),
        TransactionError::InstructionError(0, InstructionError::ReadonlyDataModified)
    );
    let account = bank.get_account(&pubkey).unwrap();
    assert_eq!(account.lamports(), START_BALANCE);

    // Realloc account to 0
    bank_client
        .send_and_confirm_message(
            signer,
            Message::new(
                &[
                    realloc(&realloc_program_id, &pubkey, 0, &mut bump),
                    ComputeBudgetInstruction::set_loaded_accounts_data_size_limit(
                        LOADED_ACCOUNTS_DATA_SIZE_LIMIT_FOR_TEST,
                    ),
                ],
                Some(&mint_pubkey),
            ),
        )
        .unwrap();
    let account = bank.get_account(&pubkey).unwrap();
    assert_eq!(account.lamports(), START_BALANCE);
    let data = bank_client.get_account_data(&pubkey).unwrap().unwrap();
    assert_eq!(0, data.len());

    // Realloc to max + 1
    assert_eq!(
        bank_client
            .send_and_confirm_message(
                signer,
                Message::new(
                    &[
                        Instruction::new_with_bytes(
                            realloc_invoke_program_id,
                            &[INVOKE_REALLOC_MAX_PLUS_ONE],
                            vec![
                                AccountMeta::new(pubkey, false),
                                AccountMeta::new_readonly(realloc_program_id, false),
                            ],
                        ),
                        ComputeBudgetInstruction::set_loaded_accounts_data_size_limit(
                            LOADED_ACCOUNTS_DATA_SIZE_LIMIT_FOR_TEST
                        ),
                    ],
                    Some(&mint_pubkey),
                )
            )
            .unwrap_err()
            .unwrap(),
        TransactionError::InstructionError(0, InstructionError::InvalidRealloc)
    );

    // Realloc to max twice
    assert_eq!(
        bank_client
            .send_and_confirm_message(
                signer,
                Message::new(
                    &[
                        Instruction::new_with_bytes(
                            realloc_invoke_program_id,
                            &[INVOKE_REALLOC_MAX_TWICE],
                            vec![
                                AccountMeta::new(pubkey, false),
                                AccountMeta::new_readonly(realloc_program_id, false),
                            ],
                        ),
                        ComputeBudgetInstruction::set_loaded_accounts_data_size_limit(
                            LOADED_ACCOUNTS_DATA_SIZE_LIMIT_FOR_TEST
                        ),
                    ],
                    Some(&mint_pubkey),
                )
            )
            .unwrap_err()
            .unwrap(),
        TransactionError::InstructionError(0, InstructionError::InvalidRealloc)
    );

    // Realloc account to 0
    bank_client
        .send_and_confirm_message(
            signer,
            Message::new(
                &[
                    realloc(&realloc_program_id, &pubkey, 0, &mut bump),
                    ComputeBudgetInstruction::set_loaded_accounts_data_size_limit(
                        LOADED_ACCOUNTS_DATA_SIZE_LIMIT_FOR_TEST,
                    ),
                ],
                Some(&mint_pubkey),
            ),
        )
        .unwrap();
    let account = bank.get_account(&pubkey).unwrap();
    assert_eq!(account.lamports(), START_BALANCE);
    let data = bank_client.get_account_data(&pubkey).unwrap().unwrap();
    assert_eq!(0, data.len());

    // Realloc and assign
    bank_client
        .send_and_confirm_message(
            signer,
            Message::new(
                &[
                    Instruction::new_with_bytes(
                        realloc_invoke_program_id,
                        &[INVOKE_REALLOC_AND_ASSIGN],
                        vec![
                            AccountMeta::new(pubkey, false),
                            AccountMeta::new_readonly(realloc_program_id, false),
                        ],
                    ),
                    ComputeBudgetInstruction::set_loaded_accounts_data_size_limit(
                        LOADED_ACCOUNTS_DATA_SIZE_LIMIT_FOR_TEST,
                    ),
                ],
                Some(&mint_pubkey),
            ),
        )
        .unwrap();
    let account = bank.get_account(&pubkey).unwrap();
    assert_eq!(&solana_system_interface::program::id(), account.owner());
    let data = bank_client.get_account_data(&pubkey).unwrap().unwrap();
    assert_eq!(MAX_PERMITTED_DATA_INCREASE, data.len());

    // Realloc to 0 with wrong owner
    assert_eq!(
        bank_client
            .send_and_confirm_message(
                signer,
                Message::new(
                    &[
                        realloc(&realloc_program_id, &pubkey, 0, &mut bump),
                        ComputeBudgetInstruction::set_loaded_accounts_data_size_limit(
                            LOADED_ACCOUNTS_DATA_SIZE_LIMIT_FOR_TEST
                        ),
                    ],
                    Some(&mint_pubkey),
                )
            )
            .unwrap_err()
            .unwrap(),
        TransactionError::InstructionError(0, InstructionError::AccountDataSizeChanged)
    );

    // realloc and assign to self via system program
    assert_eq!(
        bank_client
            .send_and_confirm_message(
                &[&mint_keypair, &keypair],
                Message::new(
                    &[
                        Instruction::new_with_bytes(
                            realloc_invoke_program_id,
                            &[INVOKE_REALLOC_AND_ASSIGN_TO_SELF_VIA_SYSTEM_PROGRAM],
                            vec![
                                AccountMeta::new(pubkey, true),
                                AccountMeta::new_readonly(realloc_program_id, false),
                                AccountMeta::new_readonly(
                                    solana_system_interface::program::id(),
                                    false
                                ),
                            ],
                        ),
                        ComputeBudgetInstruction::set_loaded_accounts_data_size_limit(
                            LOADED_ACCOUNTS_DATA_SIZE_LIMIT_FOR_TEST
                        ),
                    ],
                    Some(&mint_pubkey),
                )
            )
            .unwrap_err()
            .unwrap(),
        TransactionError::InstructionError(0, InstructionError::AccountDataSizeChanged)
    );

    // Assign to self and realloc via system program
    bank_client
        .send_and_confirm_message(
            &[&mint_keypair, &keypair],
            Message::new(
                &[
                    Instruction::new_with_bytes(
                        realloc_invoke_program_id,
                        &[INVOKE_ASSIGN_TO_SELF_VIA_SYSTEM_PROGRAM_AND_REALLOC],
                        vec![
                            AccountMeta::new(pubkey, true),
                            AccountMeta::new_readonly(realloc_program_id, false),
                            AccountMeta::new_readonly(
                                solana_system_interface::program::id(),
                                false,
                            ),
                        ],
                    ),
                    ComputeBudgetInstruction::set_loaded_accounts_data_size_limit(
                        LOADED_ACCOUNTS_DATA_SIZE_LIMIT_FOR_TEST,
                    ),
                ],
                Some(&mint_pubkey),
            ),
        )
        .unwrap();
    let account = bank.get_account(&pubkey).unwrap();
    assert_eq!(&realloc_program_id, account.owner());
    let data = bank_client.get_account_data(&pubkey).unwrap().unwrap();
    assert_eq!(2 * MAX_PERMITTED_DATA_INCREASE, data.len());

    // Realloc to 0
    bank_client
        .send_and_confirm_message(
            signer,
            Message::new(
                &[
                    realloc(&realloc_program_id, &pubkey, 0, &mut bump),
                    ComputeBudgetInstruction::set_loaded_accounts_data_size_limit(
                        LOADED_ACCOUNTS_DATA_SIZE_LIMIT_FOR_TEST,
                    ),
                ],
                Some(&mint_pubkey),
            ),
        )
        .unwrap();
    let data = bank_client.get_account_data(&pubkey).unwrap().unwrap();
    assert_eq!(0, data.len());

    // Realloc to 100 and check via CPI
    let invoke_account = AccountSharedData::new(START_BALANCE, 5, &realloc_invoke_program_id);
    bank.store_account(&invoke_pubkey, &invoke_account);
    bank_client
        .send_and_confirm_message(
            signer,
            Message::new(
                &[
                    Instruction::new_with_bytes(
                        realloc_invoke_program_id,
                        &[INVOKE_REALLOC_INVOKE_CHECK],
                        vec![
                            AccountMeta::new(invoke_pubkey, false),
                            AccountMeta::new_readonly(realloc_program_id, false),
                        ],
                    ),
                    ComputeBudgetInstruction::set_loaded_accounts_data_size_limit(
                        LOADED_ACCOUNTS_DATA_SIZE_LIMIT_FOR_TEST,
                    ),
                ],
                Some(&mint_pubkey),
            ),
        )
        .unwrap();
    let data = bank_client
        .get_account_data(&invoke_pubkey)
        .unwrap()
        .unwrap();
    assert_eq!(100, data.len());
    for i in 0..5 {
        assert_eq!(data[i], 0);
    }
    for i in 5..data.len() {
        assert_eq!(data[i], 2);
    }

    // Create account, realloc, check
    let new_keypair = Keypair::new();
    let new_pubkey = new_keypair.pubkey().clone();
    let mut instruction_data = vec![];
    instruction_data.extend_from_slice(&[INVOKE_CREATE_ACCOUNT_REALLOC_CHECK, 1]);
    instruction_data.extend_from_slice(&100_usize.to_le_bytes());
    bank_client
        .send_and_confirm_message(
            &[&mint_keypair, &new_keypair],
            Message::new(
                &[
                    Instruction::new_with_bytes(
                        realloc_invoke_program_id,
                        &instruction_data,
                        vec![
                            AccountMeta::new(mint_pubkey, true),
                            AccountMeta::new(new_pubkey, true),
                            AccountMeta::new(solana_system_interface::program::id(), false),
                            AccountMeta::new_readonly(realloc_invoke_program_id, false),
                        ],
                    ),
                    ComputeBudgetInstruction::set_loaded_accounts_data_size_limit(
                        LOADED_ACCOUNTS_DATA_SIZE_LIMIT_FOR_TEST,
                    ),
                ],
                Some(&mint_pubkey),
            ),
        )
        .unwrap();
    let data = bank_client.get_account_data(&new_pubkey).unwrap().unwrap();
    assert_eq!(200, data.len());
    let account = bank.get_account(&new_pubkey).unwrap();
    assert_eq!(&realloc_invoke_program_id, account.owner());

    // Invoke, dealloc, and assign
    let pre_len = 100;
    let new_len = pre_len * 2;
    let mut invoke_account = AccountSharedData::new(START_BALANCE, pre_len, &realloc_program_id);
    invoke_account.set_data_from_slice(&vec![1; pre_len]);
    bank.store_account(&invoke_pubkey, &invoke_account);
    let mut instruction_data = vec![];
    instruction_data.extend_from_slice(&[INVOKE_DEALLOC_AND_ASSIGN, 1]);
    instruction_data.extend_from_slice(&pre_len.to_le_bytes());
    bank_client
        .send_and_confirm_message(
            signer,
            Message::new(
                &[
                    Instruction::new_with_bytes(
                        realloc_invoke_program_id,
                        &instruction_data,
                        vec![
                            AccountMeta::new(invoke_pubkey, false),
                            AccountMeta::new_readonly(realloc_invoke_program_id, false),
                            AccountMeta::new_readonly(realloc_program_id, false),
                        ],
                    ),
                    ComputeBudgetInstruction::set_loaded_accounts_data_size_limit(
                        LOADED_ACCOUNTS_DATA_SIZE_LIMIT_FOR_TEST,
                    ),
                ],
                Some(&mint_pubkey),
            ),
        )
        .unwrap();
    let data = bank_client
        .get_account_data(&invoke_pubkey)
        .unwrap()
        .unwrap();
    assert_eq!(new_len, data.len());
    for i in 0..new_len {
        assert_eq!(data[i], 0);
    }

    // Realloc to max invoke max
    let invoke_account = AccountSharedData::new(42, 0, &realloc_invoke_program_id);
    bank.store_account(&invoke_pubkey, &invoke_account);
    assert_eq!(
        bank_client
            .send_and_confirm_message(
                signer,
                Message::new(
                    &[
                        Instruction::new_with_bytes(
                            realloc_invoke_program_id,
                            &[INVOKE_REALLOC_MAX_INVOKE_MAX],
                            vec![
                                AccountMeta::new(invoke_pubkey, false),
                                AccountMeta::new_readonly(realloc_program_id, false),
                            ],
                        ),
                        ComputeBudgetInstruction::set_loaded_accounts_data_size_limit(
                            LOADED_ACCOUNTS_DATA_SIZE_LIMIT_FOR_TEST
                        ),
                    ],
                    Some(&mint_pubkey),
                )
            )
            .unwrap_err()
            .unwrap(),
        TransactionError::InstructionError(0, InstructionError::InvalidRealloc)
    );

    // CPI realloc extend then local realloc extend
    for (cpi_extend_bytes, local_extend_bytes, should_succeed) in [
        (0, 0, true),
        (MAX_PERMITTED_DATA_INCREASE, 0, true),
        (0, MAX_PERMITTED_DATA_INCREASE, true),
        (MAX_PERMITTED_DATA_INCREASE, 1, false),
        (1, MAX_PERMITTED_DATA_INCREASE, false),
    ] {
        let invoke_account = AccountSharedData::new(100_000_000, 0, &realloc_invoke_program_id);
        bank.store_account(&invoke_pubkey, &invoke_account);
        let mut instruction_data = vec![];
        instruction_data.extend_from_slice(&[INVOKE_REALLOC_TO_THEN_LOCAL_REALLOC_EXTEND, 1]);
        instruction_data.extend_from_slice(&cpi_extend_bytes.to_le_bytes());
        instruction_data.extend_from_slice(&local_extend_bytes.to_le_bytes());

        let result = bank_client.send_and_confirm_message(
            signer,
            Message::new(
                &[
                    Instruction::new_with_bytes(
                        realloc_invoke_program_id,
                        &instruction_data,
                        vec![
                            AccountMeta::new(invoke_pubkey, false),
                            AccountMeta::new_readonly(realloc_invoke_program_id, false),
                        ],
                    ),
                    ComputeBudgetInstruction::set_loaded_accounts_data_size_limit(
                        LOADED_ACCOUNTS_DATA_SIZE_LIMIT_FOR_TEST,
                    ),
                ],
                Some(&mint_pubkey),
            ),
        );

        if should_succeed {
            assert!(
                result.is_ok(),
                "cpi: {cpi_extend_bytes} local: {local_extend_bytes}, err: {:?}",
                result.err()
            );
        } else {
            assert_eq!(
                result.unwrap_err().unwrap(),
                TransactionError::InstructionError(0, InstructionError::InvalidRealloc),
                "cpi: {cpi_extend_bytes} local: {local_extend_bytes}",
            );
        }
    }

    // Realloc shrink, then CPI, then realloc extend
    let mut invoke_account = AccountSharedData::new(100_000_000, 10, &realloc_invoke_program_id);
    invoke_account.set_data(vec![0, 1, 2, 3, 4, 5, 6, 7, 8, 9]);
    bank.store_account(&invoke_pubkey, &invoke_account);
    let mut instruction_data = vec![];
    instruction_data.extend_from_slice(&[INVOKE_REALLOC_SHRINK_THEN_CPI_THEN_REALLOC_EXTEND, 1]);
    instruction_data.extend_from_slice(&5_u64.to_le_bytes());
    instruction_data.extend_from_slice(&10_u64.to_le_bytes());
    let result = bank_client.send_and_confirm_message(
        signer,
        Message::new(
            &[
                Instruction::new_with_bytes(
                    realloc_invoke_program_id,
                    &instruction_data,
                    vec![
                        AccountMeta::new(invoke_pubkey, false),
                        AccountMeta::new_readonly(realloc_invoke_program_id, false),
                    ],
                ),
                ComputeBudgetInstruction::set_loaded_accounts_data_size_limit(
                    LOADED_ACCOUNTS_DATA_SIZE_LIMIT_FOR_TEST,
                ),
            ],
            Some(&mint_pubkey),
        ),
    );
    assert!(result.is_ok());
    let data = bank_client
        .get_account_data(&invoke_pubkey)
        .unwrap()
        .unwrap();
    assert_eq!(data, &[0, 1, 2, 3, 4, 0, 0, 0, 0, 0]);

    // Realloc invoke max twice
    let invoke_account = AccountSharedData::new(42, 0, &realloc_program_id);
    bank.store_account(&invoke_pubkey, &invoke_account);
    assert_eq!(
        bank_client
            .send_and_confirm_message(
                signer,
                Message::new(
                    &[
                        Instruction::new_with_bytes(
                            realloc_invoke_program_id,
                            &[INVOKE_INVOKE_MAX_TWICE],
                            vec![
                                AccountMeta::new(invoke_pubkey, false),
                                AccountMeta::new_readonly(realloc_invoke_program_id, false),
                                AccountMeta::new_readonly(realloc_program_id, false),
                            ],
                        ),
                        ComputeBudgetInstruction::set_loaded_accounts_data_size_limit(
                            LOADED_ACCOUNTS_DATA_SIZE_LIMIT_FOR_TEST
                        ),
                    ],
                    Some(&mint_pubkey),
                )
            )
            .unwrap_err()
            .unwrap(),
        TransactionError::InstructionError(0, InstructionError::InvalidRealloc)
    );

    // Realloc to 0
    bank_client
        .send_and_confirm_message(
            signer,
            Message::new(
                &[realloc(&realloc_program_id, &pubkey, 0, &mut bump)],
                Some(&mint_pubkey),
            ),
        )
        .unwrap();
    let data = bank_client.get_account_data(&pubkey).unwrap().unwrap();
    assert_eq!(0, data.len());

    // Realloc to max length in max increase increments
    for i in 0..MAX_PERMITTED_DATA_LENGTH as usize / MAX_PERMITTED_DATA_INCREASE {
        bank_client
            .send_and_confirm_message(
                signer,
                Message::new(
                    &[
                        Instruction::new_with_bytes(
                            realloc_invoke_program_id,
                            &[INVOKE_REALLOC_EXTEND_MAX, 1, i as u8, (i / 255) as u8],
                            vec![
                                AccountMeta::new(pubkey, false),
                                AccountMeta::new_readonly(realloc_program_id, false),
                            ],
                        ),
                        ComputeBudgetInstruction::set_loaded_accounts_data_size_limit(
                            LOADED_ACCOUNTS_DATA_SIZE_LIMIT_FOR_TEST,
                        ),
                    ],
                    Some(&mint_pubkey),
                ),
            )
            .unwrap();
        let data = bank_client.get_account_data(&pubkey).unwrap().unwrap();
        assert_eq!((i + 1) * MAX_PERMITTED_DATA_INCREASE, data.len());
    }
    for i in 0..data.len() {
        assert_eq!(data[i], 1);
    }

    // and one more time should fail
    assert_eq!(
        bank_client
            .send_and_confirm_message(
                signer,
                Message::new(
                    &[
                        Instruction::new_with_bytes(
                            realloc_invoke_program_id,
                            &[INVOKE_REALLOC_EXTEND_MAX, 2, 1, 1],
                            vec![
                                AccountMeta::new(pubkey, false),
                                AccountMeta::new_readonly(realloc_program_id, false),
                            ],
                        ),
                        ComputeBudgetInstruction::set_loaded_accounts_data_size_limit(
                            LOADED_ACCOUNTS_DATA_SIZE_LIMIT_FOR_TEST
                        ),
                    ],
                    Some(&mint_pubkey),
                )
            )
            .unwrap_err()
            .unwrap(),
        TransactionError::InstructionError(0, InstructionError::InvalidRealloc)
    );

    // Realloc recursively and fill data
    let invoke_keypair = Keypair::new();
    let invoke_pubkey = invoke_keypair.pubkey().clone();
    let invoke_account = AccountSharedData::new(START_BALANCE, 0, &realloc_invoke_program_id);
    bank.store_account(&invoke_pubkey, &invoke_account);
    let mut instruction_data = vec![];
    instruction_data.extend_from_slice(&[INVOKE_REALLOC_RECURSIVE, 1]);
    instruction_data.extend_from_slice(&100_usize.to_le_bytes());
    bank_client
        .send_and_confirm_message(
            signer,
            Message::new(
                &[
                    Instruction::new_with_bytes(
                        realloc_invoke_program_id,
                        &instruction_data,
                        vec![
                            AccountMeta::new(invoke_pubkey, false),
                            AccountMeta::new_readonly(realloc_invoke_program_id, false),
                        ],
                    ),
                    ComputeBudgetInstruction::set_loaded_accounts_data_size_limit(
                        LOADED_ACCOUNTS_DATA_SIZE_LIMIT_FOR_TEST,
                    ),
                ],
                Some(&mint_pubkey),
            ),
        )
        .unwrap();
    let data = bank_client
        .get_account_data(&invoke_pubkey)
        .unwrap()
        .unwrap();
    assert_eq!(200, data.len());
    for i in 0..100 {
        assert_eq!(data[i], 1);
    }
    for i in 100..200 {
        assert_eq!(data[i], 2);
    }
}

#[test]
#[cfg(feature = "sbf_rust")]
fn test_program_sbf_processed_inner_instruction() {
    agave_logger::setup();

    let GenesisConfigInfo {
        genesis_config,
        mint_keypair,
        ..
    } = create_genesis_config(50);

    let (bank, bank_forks) = Bank::new_with_bank_forks_for_tests(&genesis_config);
    let mut bank_client = BankClient::new_shared(bank.clone());
    let authority_keypair = Keypair::new();

    let (_bank, sibling_program_id) = load_program_of_loader_v4(
        &mut bank_client,
        &bank_forks,
        &mint_keypair,
        &authority_keypair,
        "solana_sbf_rust_sibling_instructions",
    );
    let (_bank, sibling_inner_program_id) = load_program_of_loader_v4(
        &mut bank_client,
        &bank_forks,
        &mint_keypair,
        &authority_keypair,
        "solana_sbf_rust_sibling_inner_instructions",
    );
    let (_bank, noop_program_id) = load_program_of_loader_v4(
        &mut bank_client,
        &bank_forks,
        &mint_keypair,
        &authority_keypair,
        "solana_sbf_rust_noop",
    );
    let (_bank, invoke_and_return_program_id) = load_program_of_loader_v4(
        &mut bank_client,
        &bank_forks,
        &mint_keypair,
        &authority_keypair,
        "solana_sbf_rust_invoke_and_return",
    );

    let instruction2 = Instruction::new_with_bytes(
        noop_program_id,
        &[43],
        vec![
            AccountMeta::new_readonly(noop_program_id, false),
            AccountMeta::new(mint_keypair.pubkey(), true),
        ],
    );
    let instruction1 = Instruction::new_with_bytes(
        noop_program_id,
        &[42],
        vec![
            AccountMeta::new(mint_keypair.pubkey(), true),
            AccountMeta::new_readonly(noop_program_id, false),
        ],
    );
    let instruction0 = Instruction::new_with_bytes(
        sibling_program_id,
        &[1, 2, 3, 0, 4, 5, 6],
        vec![
            AccountMeta::new(mint_keypair.pubkey(), true),
            AccountMeta::new_readonly(noop_program_id, false),
            AccountMeta::new_readonly(invoke_and_return_program_id, false),
            AccountMeta::new_readonly(sibling_inner_program_id, false),
        ],
    );
    let message = Message::new(
        &[instruction2, instruction1, instruction0],
        Some(&mint_keypair.pubkey()),
    );
    assert!(
        bank_client
            .send_and_confirm_message(&[&mint_keypair], message)
            .is_ok()
    );
}

#[test]
#[cfg(feature = "sbf_rust")]
fn test_program_fees() {
    agave_logger::setup();

    let congestion_multiplier = 1;

    let GenesisConfigInfo {
        mut genesis_config,
        mint_keypair,
        ..
    } = create_genesis_config(500_000_000);

    genesis_config.fee_rate_governor = FeeRateGovernor::new(congestion_multiplier, 0);
    let mut bank = Bank::new_for_tests(&genesis_config);
    let fee_structure = FeeStructure {
        lamports_per_signature: 5000,
        lamports_per_write_lock: 0,
        compute_fee_bins: vec![
            FeeBin {
                limit: 200,
                fee: 500,
            },
            FeeBin {
                limit: 1400000,
                fee: 5000,
            },
        ],
    };
    bank.set_fee_structure(&fee_structure);
    let (bank, bank_forks) = bank.wrap_with_bank_forks_for_tests();
    let feature_set = bank.feature_set.clone();
    let mut bank_client = BankClient::new_shared(bank.clone());
    let authority_keypair = Keypair::new();

    let (_bank, program_id) = load_program_of_loader_v4(
        &mut bank_client,
        &bank_forks,
        &mint_keypair,
        &authority_keypair,
        "solana_sbf_rust_noop",
    );

    let pre_balance = bank_client.get_balance(&mint_keypair.pubkey()).unwrap();
    let message = Message::new(
        &[Instruction::new_with_bytes(program_id, &[], vec![])],
        Some(&mint_keypair.pubkey()),
    );

    let sanitized_message = SanitizedMessage::try_from_legacy_message(
        message.clone(),
        &ReservedAccountKeys::empty_key_set(),
    )
    .unwrap();
    let fee_budget_limits = FeeBudgetLimits::from(
        process_compute_budget_instructions(
            SVMStaticMessage::program_instructions_iter(&sanitized_message),
            &feature_set,
        )
        .unwrap_or_default(),
    );
    let expected_normal_fee = solana_fee::calculate_fee(
        &sanitized_message,
        congestion_multiplier == 0,
        fee_structure.lamports_per_signature,
        fee_budget_limits.prioritization_fee,
        bank.feature_set.as_ref().into(),
    );
    bank_client
        .send_and_confirm_message(&[&mint_keypair], message)
        .unwrap();
    let post_balance = bank_client.get_balance(&mint_keypair.pubkey()).unwrap();
    assert_eq!(pre_balance - post_balance, expected_normal_fee);

    let pre_balance = bank_client.get_balance(&mint_keypair.pubkey()).unwrap();
    let message = Message::new(
        &[
            ComputeBudgetInstruction::set_compute_unit_price(1),
            Instruction::new_with_bytes(program_id, &[], vec![]),
        ],
        Some(&mint_keypair.pubkey()),
    );
    let sanitized_message = SanitizedMessage::try_from_legacy_message(
        message.clone(),
        &ReservedAccountKeys::empty_key_set(),
    )
    .unwrap();
    let fee_budget_limits = FeeBudgetLimits::from(
        process_compute_budget_instructions(
            SVMStaticMessage::program_instructions_iter(&sanitized_message),
            &feature_set,
        )
        .unwrap_or_default(),
    );
    let expected_prioritized_fee = solana_fee::calculate_fee(
        &sanitized_message,
        congestion_multiplier == 0,
        fee_structure.lamports_per_signature,
        fee_budget_limits.prioritization_fee,
        bank.feature_set.as_ref().into(),
    );
    assert!(expected_normal_fee < expected_prioritized_fee);

    bank_client
        .send_and_confirm_message(&[&mint_keypair], message)
        .unwrap();
    let post_balance = bank_client.get_balance(&mint_keypair.pubkey()).unwrap();
    assert_eq!(pre_balance - post_balance, expected_prioritized_fee);
}

#[test]
#[cfg(feature = "sbf_rust")]
fn test_program_sbf_inner_instruction_alignment_checks() {
    agave_logger::setup();

    let GenesisConfigInfo {
        genesis_config,
        mint_keypair,
        ..
    } = create_genesis_config(50);
    let (bank, bank_forks) = Bank::new_with_bank_forks_for_tests(&genesis_config);
    let noop = create_program(&bank, &bpf_loader_deprecated::id(), "solana_sbf_rust_noop");
    let inner_instruction_alignment_check = create_program(
        &bank,
        &bpf_loader_deprecated::id(),
        "solana_sbf_rust_inner_instruction_alignment_check",
    );

    // invoke unaligned program, which will call aligned program twice,
    // unaligned should be allowed once invoke completes
    let mut bank_client = BankClient::new_shared(bank);
    bank_client
        .advance_slot(1, bank_forks.as_ref(), &Pubkey::default())
        .expect("Failed to advance the slot");
    let mut instruction = Instruction::new_with_bytes(
        inner_instruction_alignment_check,
        &[0],
        vec![
            AccountMeta::new_readonly(noop, false),
            AccountMeta::new_readonly(mint_keypair.pubkey(), false),
        ],
    );

    instruction.data[0] += 1;
    let result = bank_client.send_and_confirm_instruction(&mint_keypair, instruction.clone());
    assert!(result.is_ok(), "{result:?}");
}

#[test]
#[cfg(feature = "sbf_rust")]
fn test_cpi_account_ownership_writability() {
    agave_logger::setup();

    for virtual_address_space_adjustments in [false, true] {
        let GenesisConfigInfo {
            genesis_config,
            mint_keypair,
            ..
        } = create_genesis_config(100_123_456_789);

        let mut bank = Bank::new_for_tests(&genesis_config);
        let mut feature_set = FeatureSet::all_enabled();
        if !virtual_address_space_adjustments {
            feature_set.deactivate(&feature_set::syscall_parameter_address_restrictions::id());
            feature_set.deactivate(&feature_set::virtual_address_space_adjustments::id());
            feature_set.deactivate(&feature_set::account_data_direct_mapping::id());
        }

        bank.feature_set = Arc::new(feature_set);
        let (bank, bank_forks) = bank.wrap_with_bank_forks_for_tests();
        let mut bank_client = BankClient::new_shared(bank);
        let authority_keypair = Keypair::new();

        let (_bank, invoke_program_id) = load_program_of_loader_v4(
            &mut bank_client,
            &bank_forks,
            &mint_keypair,
            &authority_keypair,
            "solana_sbf_rust_invoke",
        );
        let (_bank, invoked_program_id) = load_program_of_loader_v4(
            &mut bank_client,
            &bank_forks,
            &mint_keypair,
            &authority_keypair,
            "solana_sbf_rust_invoked",
        );
        let (bank, realloc_program_id) = load_program_of_loader_v4(
            &mut bank_client,
            &bank_forks,
            &mint_keypair,
            &authority_keypair,
            "solana_sbf_rust_realloc",
        );

        let account_keypair = Keypair::new();

        let mint_pubkey = mint_keypair.pubkey();
        let account_metas = vec![
            AccountMeta::new(mint_pubkey, true),
            AccountMeta::new(account_keypair.pubkey(), false),
            AccountMeta::new_readonly(invoked_program_id, false),
            AccountMeta::new_readonly(invoke_program_id, false),
            AccountMeta::new_readonly(realloc_program_id, false),
        ];

        for (account_size, byte_index) in [
            (0, 0),                                   // first realloc byte
            (0, MAX_PERMITTED_DATA_INCREASE - 1),     // last realloc byte
            (2, 0),                                   // first data byte
            (2, 1),                                   // last data byte
            (2, 3),                                   // first realloc byte
            (2, 2 + MAX_PERMITTED_DATA_INCREASE - 1), // last realloc byte
        ] {
            for instruction_id in [
                TEST_FORBID_WRITE_AFTER_OWNERSHIP_CHANGE_IN_CALLEE,
                TEST_FORBID_WRITE_AFTER_OWNERSHIP_CHANGE_IN_CALLER,
            ] {
                bank.register_unique_recent_blockhash_for_test();
                let account = AccountSharedData::new(42, account_size, &invoke_program_id);
                bank.store_account(&account_keypair.pubkey(), &account);
                let mut instruction_data = vec![instruction_id];
                instruction_data.extend_from_slice(byte_index.to_le_bytes().as_ref());

                let instruction = Instruction::new_with_bytes(
                    invoke_program_id,
                    &instruction_data,
                    account_metas.clone(),
                );

                let result = bank_client.send_and_confirm_instruction(&mint_keypair, instruction);

                if (byte_index as usize) < account_size || virtual_address_space_adjustments {
                    assert_eq!(
                        result.unwrap_err().unwrap(),
                        TransactionError::InstructionError(
                            0,
                            InstructionError::ExternalAccountDataModified,
                        )
                    );
                } else {
                    // without virtual_address_space_adjustments, changes to the realloc padding
                    // outside the account length are ignored
                    assert!(result.is_ok(), "{result:?}");
                }
            }
        }
        // Test that the CPI code that updates `ref_to_len_in_vm` fails if we
        // make it write to an invalid location. This is the first variant which
        // correctly triggers ExternalAccountDataModified when virtual_address_space_adjustments is
        // disabled. When virtual_address_space_adjustments is enabled this tests fails early
        // because we move the account data pointer.
        // TEST_FORBID_LEN_UPDATE_AFTER_OWNERSHIP_CHANGE is able to make more
        // progress when virtual_address_space_adjustments is on.
        let account = AccountSharedData::new(42, 0, &invoke_program_id);
        bank.store_account(&account_keypair.pubkey(), &account);
        let instruction_data = vec![
            TEST_FORBID_LEN_UPDATE_AFTER_OWNERSHIP_CHANGE_MOVING_DATA_POINTER,
            42,
            42,
            42,
        ];
        let instruction = Instruction::new_with_bytes(
            invoke_program_id,
            &instruction_data,
            account_metas.clone(),
        );
        let result = bank_client.send_and_confirm_instruction(&mint_keypair, instruction);
        assert_eq!(
            result.unwrap_err().unwrap(),
            if virtual_address_space_adjustments {
                // We move the data pointer, virtual_address_space_adjustments doesn't allow it
                // anymore so it errors out earlier. See
                // test_cpi_invalid_account_info_pointers.
                TransactionError::InstructionError(0, InstructionError::ProgramFailedToComplete)
            } else {
                // We managed to make CPI write into the account data, but the
                // usual checks still apply and we get an error.
                TransactionError::InstructionError(0, InstructionError::ExternalAccountDataModified)
            }
        );

        // We're going to try and make CPI write ref_to_len_in_vm into a 2nd
        // account, so we add an extra one here.
        let account2_keypair = Keypair::new();
        let mut account_metas = account_metas.clone();
        account_metas.push(AccountMeta::new(account2_keypair.pubkey(), false));

        for target_account in [1, account_metas.len() as u8 - 1] {
            // Similar to the test above where we try to make CPI write into account
            // data. This variant is for when virtual_address_space_adjustments is enabled.
            let account = AccountSharedData::new(42, 0, &invoke_program_id);
            bank.store_account(&account_keypair.pubkey(), &account);
            let account = AccountSharedData::new(42, 0, &invoke_program_id);
            bank.store_account(&account2_keypair.pubkey(), &account);
            let instruction_data = vec![
                TEST_FORBID_LEN_UPDATE_AFTER_OWNERSHIP_CHANGE,
                target_account,
                42,
                42,
            ];
            let instruction = Instruction::new_with_bytes(
                invoke_program_id,
                &instruction_data,
                account_metas.clone(),
            );
            let message = Message::new(&[instruction], Some(&mint_pubkey));
            let tx = Transaction::new(&[&mint_keypair], message.clone(), bank.last_blockhash());
            let (result, _, logs, _) = process_transaction_and_record_inner(&bank, tx);
            if virtual_address_space_adjustments {
                assert_eq!(
                    result.unwrap_err(),
                    TransactionError::InstructionError(
                        0,
                        InstructionError::ProgramFailedToComplete
                    )
                );
                // We haven't moved the data pointer, but ref_to_len_vm _is_ in
                // the account data vm range and that's not allowed either.
                assert!(
                    logs.iter().any(|log| log.contains("Invalid pointer")),
                    "{logs:?}"
                );
            } else {
                // we expect this to succeed as after updating `ref_to_len_in_vm`,
                // CPI will sync the actual account data between the callee and the
                // caller, _always_ writing over the location pointed by
                // `ref_to_len_in_vm`. To verify this, we check that the account
                // data is in fact all zeroes like it is in the callee.
                result.unwrap();
                let account = bank.get_account(&account_keypair.pubkey()).unwrap();
                assert_eq!(account.data(), vec![0; 40]);
            }
        }

        // Test that the caller can write to an account which it received from the callee
        let account = AccountSharedData::new(42, 0, &invoked_program_id);
        bank.store_account(&account_keypair.pubkey(), &account);
        let instruction_data = vec![TEST_ALLOW_WRITE_AFTER_OWNERSHIP_CHANGE_TO_CALLER, 1, 42, 42];
        let instruction =
            Instruction::new_with_bytes(invoke_program_id, &instruction_data, account_metas);
        let result = bank_client.send_and_confirm_instruction(&mint_keypair, instruction);
        result.unwrap();
    }
}

#[test]
#[cfg(feature = "sbf_rust")]
fn test_cpi_account_data_updates() {
    agave_logger::setup();

    for (deprecated_callee, deprecated_caller, virtual_address_space_adjustments) in
        [false, true].into_iter().flat_map(move |z| {
            [false, true]
                .into_iter()
                .flat_map(move |y| [false, true].into_iter().map(move |x| (x, y, z)))
        })
    {
        let GenesisConfigInfo {
            genesis_config,
            mint_keypair,
            ..
        } = create_genesis_config(100_123_456_789);
        let mut bank = Bank::new_for_tests(&genesis_config);
        let mut feature_set = FeatureSet::all_enabled();
        if !virtual_address_space_adjustments {
            feature_set.deactivate(&feature_set::syscall_parameter_address_restrictions::id());
            feature_set.deactivate(&feature_set::virtual_address_space_adjustments::id());
            feature_set.deactivate(&feature_set::account_data_direct_mapping::id());
        }

        bank.feature_set = Arc::new(feature_set);
        let (bank, bank_forks) = bank.wrap_with_bank_forks_for_tests();
        let mut bank_client = BankClient::new_shared(bank);
        let authority_keypair = Keypair::new();

        let (_bank, invoke_program_id) = load_program_of_loader_v4(
            &mut bank_client,
            &bank_forks,
            &mint_keypair,
            &authority_keypair,
            "solana_sbf_rust_invoke",
        );
        let (bank, realloc_program_id) = load_program_of_loader_v4(
            &mut bank_client,
            &bank_forks,
            &mint_keypair,
            &authority_keypair,
            "solana_sbf_rust_realloc",
        );
        let deprecated_program_id = create_program(
            &bank,
            &bpf_loader_deprecated::id(),
            "solana_sbf_rust_deprecated_loader",
        );

        let account_keypair = Keypair::new();
        let mint_pubkey = mint_keypair.pubkey();
        let account_metas = vec![
            AccountMeta::new(mint_pubkey, true),
            AccountMeta::new(account_keypair.pubkey(), false),
            AccountMeta::new_readonly(
                if deprecated_callee {
                    deprecated_program_id
                } else {
                    realloc_program_id
                },
                false,
            ),
            AccountMeta::new_readonly(
                if deprecated_caller {
                    deprecated_program_id
                } else {
                    invoke_program_id
                },
                false,
            ),
        ];

        // This tests the case where a caller extends an account beyond the original
        // data length. The callee should see the extended data (asserted in the
        // callee program, not here).
        let mut account = AccountSharedData::new(42, 0, &account_metas[3].pubkey);
        account.set_data(b"foo".to_vec());
        bank.store_account(&account_keypair.pubkey(), &account);
        let mut instruction_data = vec![TEST_CPI_ACCOUNT_UPDATE_CALLER_GROWS];
        instruction_data.extend_from_slice(b"bar");
        let instruction = Instruction::new_with_bytes(
            account_metas[3].pubkey,
            &instruction_data,
            account_metas.clone(),
        );
        let result = bank_client.send_and_confirm_instruction(&mint_keypair, instruction);
        if deprecated_caller {
            assert_eq!(
                result.unwrap_err().unwrap(),
                TransactionError::InstructionError(
                    0,
                    if virtual_address_space_adjustments {
                        InstructionError::ProgramFailedToComplete
                    } else {
                        InstructionError::ModifiedProgramId
                    }
                )
            );
        } else {
            assert!(result.is_ok(), "{result:?}");
            let account = bank.get_account(&account_keypair.pubkey()).unwrap();
            // "bar" here was copied from the realloc region
            assert_eq!(account.data(), b"foobar");
        }

        // This tests the case where a callee extends an account beyond the original
        // data length. The caller should see the extended data where the realloc
        // region contains the new data. In this test the callee owns the account,
        // the caller can't write but the CPI glue still updates correctly.
        let mut account = AccountSharedData::new(42, 0, &account_metas[2].pubkey);
        account.set_data(b"foo".to_vec());
        bank.store_account(&account_keypair.pubkey(), &account);
        let mut instruction_data = vec![TEST_CPI_ACCOUNT_UPDATE_CALLEE_GROWS];
        instruction_data.extend_from_slice(b"bar");
        let instruction = Instruction::new_with_bytes(
            account_metas[3].pubkey,
            &instruction_data,
            account_metas.clone(),
        );
        let result = bank_client.send_and_confirm_instruction(&mint_keypair, instruction);
        if deprecated_callee {
            assert!(result.is_ok(), "{result:?}");
            let account = bank.get_account(&account_keypair.pubkey()).unwrap();
            // deprecated_callee is incapable of resizing accounts
            assert_eq!(account.data(), b"foo");
        } else if deprecated_caller {
            assert_eq!(
                result.unwrap_err().unwrap(),
                TransactionError::InstructionError(
                    0,
                    if virtual_address_space_adjustments {
                        InstructionError::InvalidRealloc
                    } else {
                        InstructionError::AccountDataSizeChanged
                    }
                )
            );
        } else {
            assert!(result.is_ok(), "{result:?}");
            let account = bank.get_account(&account_keypair.pubkey()).unwrap();
            // "bar" here was copied from the realloc region
            assert_eq!(account.data(), b"foobar");
        }

        // This tests the case where a callee shrinks an account, the caller data
        // slice must be truncated accordingly and post_len..original_data_len must
        // be zeroed (zeroing is checked in the invoked program not here). Same as
        // above, the callee owns the account but the changes are still reflected in
        // the caller even if things are readonly from the caller's POV.
        let mut account = AccountSharedData::new(42, 0, &account_metas[2].pubkey);
        account.set_data(b"foobar".to_vec());
        bank.store_account(&account_keypair.pubkey(), &account);
        let mut instruction_data = vec![
            TEST_CPI_ACCOUNT_UPDATE_CALLEE_SHRINKS_SMALLER_THAN_ORIGINAL_LEN,
            virtual_address_space_adjustments as u8,
        ];
        instruction_data.extend_from_slice(4usize.to_le_bytes().as_ref());
        let instruction = Instruction::new_with_bytes(
            account_metas[3].pubkey,
            &instruction_data,
            account_metas.clone(),
        );
        let result = bank_client.send_and_confirm_instruction(&mint_keypair, instruction);
        if deprecated_callee {
            assert!(result.is_ok(), "{result:?}");
            let account = bank.get_account(&account_keypair.pubkey()).unwrap();
            // deprecated_callee is incapable of resizing accounts
            assert_eq!(account.data(), b"foobar");
        } else if deprecated_caller {
            assert_eq!(
                result.unwrap_err().unwrap(),
                TransactionError::InstructionError(
                    0,
                    if virtual_address_space_adjustments && deprecated_callee {
                        InstructionError::InvalidRealloc
                    } else {
                        InstructionError::AccountDataSizeChanged
                    }
                )
            );
        } else {
            assert!(result.is_ok(), "{result:?}");
            let account = bank.get_account(&account_keypair.pubkey()).unwrap();
            assert_eq!(account.data(), b"foob");
        }

        // This tests the case where the program extends an account, then calls
        // itself and in the inner call it shrinks the account to a size that is
        // still larger than the original size. The account data must be set to the
        // correct value in the caller frame, and the realloc region must be zeroed
        // (again tested in the invoked program).
        let mut account = AccountSharedData::new(42, 0, &account_metas[3].pubkey);
        account.set_data(b"foo".to_vec());
        bank.store_account(&account_keypair.pubkey(), &account);
        let mut instruction_data = vec![
            TEST_CPI_ACCOUNT_UPDATE_CALLER_GROWS_CALLEE_SHRINKS,
            virtual_address_space_adjustments as u8,
        ];
        // realloc to "foobazbad" then shrink to "foobazb"
        instruction_data.extend_from_slice(7usize.to_le_bytes().as_ref());
        instruction_data.extend_from_slice(b"bazbad");
        let instruction = Instruction::new_with_bytes(
            account_metas[3].pubkey,
            &instruction_data,
            account_metas.clone(),
        );
        let result = bank_client.send_and_confirm_instruction(&mint_keypair, instruction);
        if deprecated_caller {
            assert_eq!(
                result.unwrap_err().unwrap(),
                TransactionError::InstructionError(
                    0,
                    if virtual_address_space_adjustments {
                        InstructionError::ProgramFailedToComplete
                    } else {
                        InstructionError::ModifiedProgramId
                    }
                )
            );
        } else {
            assert!(result.is_ok(), "{result:?}");
            let account = bank.get_account(&account_keypair.pubkey()).unwrap();
            assert_eq!(account.data(), b"foobazb");
        }

        // Similar to the test above, but this time the nested invocation shrinks to
        // _below_ the original data length. Both the spare capacity in the account
        // data _end_ the realloc region must be zeroed.
        let mut account = AccountSharedData::new(42, 0, &account_metas[3].pubkey);
        account.set_data(b"foo".to_vec());
        bank.store_account(&account_keypair.pubkey(), &account);
        let mut instruction_data = vec![
            TEST_CPI_ACCOUNT_UPDATE_CALLER_GROWS_CALLEE_SHRINKS,
            virtual_address_space_adjustments as u8,
        ];
        // realloc to "foobazbad" then shrink to "f"
        instruction_data.extend_from_slice(1usize.to_le_bytes().as_ref());
        instruction_data.extend_from_slice(b"bazbad");
        let instruction = Instruction::new_with_bytes(
            account_metas[3].pubkey,
            &instruction_data,
            account_metas.clone(),
        );
        let result = bank_client.send_and_confirm_instruction(&mint_keypair, instruction);
        if deprecated_caller {
            assert_eq!(
                result.unwrap_err().unwrap(),
                TransactionError::InstructionError(
                    0,
                    if virtual_address_space_adjustments {
                        InstructionError::ProgramFailedToComplete
                    } else {
                        InstructionError::ModifiedProgramId
                    }
                )
            );
        } else {
            assert!(result.is_ok(), "{result:?}");
            let account = bank.get_account(&account_keypair.pubkey()).unwrap();
            assert_eq!(account.data(), b"f");
        }
    }
}

#[test]
#[cfg(any(feature = "sbf_c", feature = "sbf_rust"))]
fn test_cpi_invalid_account_info_pointers() {
    agave_logger::setup();

    let GenesisConfigInfo {
        genesis_config,
        mint_keypair,
        ..
    } = create_genesis_config(100_123_456_789);

    let bank = Bank::new_for_tests(&genesis_config);
    let (bank, bank_forks) = bank.wrap_with_bank_forks_for_tests();
    let mut bank_client = BankClient::new_shared(bank);
    let authority_keypair = Keypair::new();

    let account_keypair = Keypair::new();
    let mint_pubkey = mint_keypair.pubkey();
    let mut account_metas = vec![
        AccountMeta::new(mint_pubkey, true),
        AccountMeta::new(account_keypair.pubkey(), false),
    ];

    let mut program_ids: Vec<Pubkey> = Vec::with_capacity(2);

    #[allow(unused_mut)]
    let mut bank;
    #[cfg(feature = "sbf_rust")]
    {
        let (new_bank, invoke_program_id) = load_program_of_loader_v4(
            &mut bank_client,
            &bank_forks,
            &mint_keypair,
            &authority_keypair,
            "solana_sbf_rust_invoke",
        );
        account_metas.push(AccountMeta::new_readonly(invoke_program_id, false));
        program_ids.push(invoke_program_id);
        #[allow(unused)]
        {
            bank = new_bank;
        }
    }

    #[cfg(feature = "sbf_c")]
    {
        let (new_bank, c_invoke_program_id) = load_program_of_loader_v4(
            &mut bank_client,
            &bank_forks,
            &mint_keypair,
            &authority_keypair,
            "invoke",
        );
        account_metas.push(AccountMeta::new_readonly(c_invoke_program_id, false));
        program_ids.push(c_invoke_program_id);
        #[allow(unused)]
        {
            bank = new_bank;
        }
    }

    for invoke_program_id in &program_ids {
        for ix in [
            TEST_CPI_INVALID_KEY_POINTER,
            TEST_CPI_INVALID_LAMPORTS_POINTER,
            TEST_CPI_INVALID_OWNER_POINTER,
            TEST_CPI_INVALID_DATA_POINTER,
        ] {
            let account = AccountSharedData::new(42, 5, invoke_program_id);
            bank.store_account(&account_keypair.pubkey(), &account);
            let instruction = Instruction::new_with_bytes(
                *invoke_program_id,
                &[ix, 42, 42, 42],
                account_metas.clone(),
            );

            let message = Message::new(&[instruction], Some(&mint_pubkey));
            let tx = Transaction::new(&[&mint_keypair], message.clone(), bank.last_blockhash());
            let (result, _, logs, _) = process_transaction_and_record_inner(&bank, tx);
            assert!(result.is_err(), "{result:?}");
            assert!(
                logs.iter().any(|log| log.contains("Invalid pointer")),
                "{logs:?}"
            );
        }
    }
}

#[test]
#[cfg(feature = "sbf_rust")]
fn test_deplete_cost_meter_with_access_violation() {
    agave_logger::setup();
    let GenesisConfigInfo {
        genesis_config,
        mint_keypair,
        ..
    } = create_genesis_config(100_123_456_789);

    let bank = Bank::new_for_tests(&genesis_config);
    let (bank, bank_forks) = bank.wrap_with_bank_forks_for_tests();
    let mut bank_client = BankClient::new_shared(bank.clone());
    let authority_keypair = Keypair::new();
    let (bank, invoke_program_id) = load_program_of_loader_v4(
        &mut bank_client,
        bank_forks.as_ref(),
        &mint_keypair,
        &authority_keypair,
        "solana_sbf_rust_invoke",
    );

    let account_keypair = Keypair::new();
    let mint_pubkey = mint_keypair.pubkey();
    let account_metas = vec![
        AccountMeta::new(mint_pubkey, true),
        AccountMeta::new(account_keypair.pubkey(), false),
        AccountMeta::new_readonly(invoke_program_id, false),
    ];

    let mut instruction_data = vec![TEST_WRITE_ACCOUNT, 2];
    instruction_data.extend_from_slice(3usize.to_le_bytes().as_ref());
    instruction_data.push(42);

    let instruction =
        Instruction::new_with_bytes(invoke_program_id, &instruction_data, account_metas.clone());

    let compute_unit_limit = 10_000u32;
    let message = Message::new(
        &[
            ComputeBudgetInstruction::set_compute_unit_limit(compute_unit_limit),
            instruction,
        ],
        Some(&mint_keypair.pubkey()),
    );
    let tx = Transaction::new(&[&mint_keypair], message, bank.last_blockhash());

    let result = load_execute_and_commit_transaction(&bank, tx).unwrap();

    assert_eq!(
        result.status.unwrap_err(),
        TransactionError::InstructionError(1, InstructionError::ReadonlyDataModified)
    );

    // all compute unit limit should be consumed due to SBF VM error
    assert_eq!(result.executed_units, u64::from(compute_unit_limit));
}

#[test]
#[cfg(feature = "sbf_rust")]
fn test_program_sbf_deplete_cost_meter_with_divide_by_zero() {
    agave_logger::setup();

    let program_elf = harness::file::load_program_elf("solana_sbf_rust_divide_by_zero");
    let program_id = Pubkey::new_unique();

    let feature_set = SVMFeatureSet::all_enabled();
    let compute_budget = {
        let mut budget = ComputeBudget::new_with_defaults(false, false);
        budget.compute_unit_limit = 10_000u64;
        budget
    };

    let mut program_cache = default_program_cache_with_program(
        &program_id,
        &program_elf,
        &feature_set,
        &compute_budget,
    );
    let sysvar_cache = default_sysvar_cache();

    let instruction = Instruction::new_with_bytes(program_id, &[], vec![]);

    let context = InstrContext {
        feature_set,
        accounts: vec![],
        instruction,
    };

    let effects =
        harness::execute_instr(context, &compute_budget, &mut program_cache, &sysvar_cache)
            .unwrap();

    assert_eq!(
        effects.result,
        Some(InstructionError::ProgramFailedToComplete)
    );

    // all compute unit limit should be consumed due to SBF VM error
    assert_eq!(effects.cu_avail, 0);
}

#[test]
#[cfg(feature = "sbf_rust")]
fn test_deny_access_beyond_current_length() {
    agave_logger::setup();

    let GenesisConfigInfo {
        genesis_config,
        mint_keypair,
        ..
    } = create_genesis_config(100_123_456_789);

    for virtual_address_space_adjustments in [false, true] {
        let mut bank = Bank::new_for_tests(&genesis_config);
        let feature_set = Arc::make_mut(&mut bank.feature_set);
        // by default test banks have all features enabled, so we only need to
        // disable when needed
        if !virtual_address_space_adjustments {
            feature_set.deactivate(&feature_set::syscall_parameter_address_restrictions::id());
            feature_set.deactivate(&feature_set::virtual_address_space_adjustments::id());
            feature_set.deactivate(&feature_set::account_data_direct_mapping::id());
        }
        let (bank, bank_forks) = bank.wrap_with_bank_forks_for_tests();
        let mut bank_client = BankClient::new_shared(bank);
        let authority_keypair = Keypair::new();

        let (bank, invoke_program_id) = load_program_of_loader_v4(
            &mut bank_client,
            &bank_forks,
            &mint_keypair,
            &authority_keypair,
            "solana_sbf_rust_invoke",
        );
        let account = AccountSharedData::new(42, 0, &invoke_program_id);
        let readonly_account_keypair = Keypair::new();
        let writable_account_keypair = Keypair::new();
        bank.store_account(&readonly_account_keypair.pubkey(), &account);
        bank.store_account(&writable_account_keypair.pubkey(), &account);

        let mint_pubkey = mint_keypair.pubkey();
        let account_metas = vec![
            AccountMeta::new(mint_pubkey, true),
            AccountMeta::new_readonly(readonly_account_keypair.pubkey(), false),
            AccountMeta::new(writable_account_keypair.pubkey(), false),
            AccountMeta::new_readonly(invoke_program_id, false),
        ];

        for (instruction_account_index, expected_error) in [
            (1, InstructionError::AccountDataTooSmall),
            (2, InstructionError::InvalidRealloc),
        ] {
            let mut instruction_data = vec![TEST_READ_ACCOUNT, instruction_account_index];
            instruction_data.extend_from_slice(3usize.to_le_bytes().as_ref());
            let instruction = Instruction::new_with_bytes(
                invoke_program_id,
                &instruction_data,
                account_metas.clone(),
            );
            let result = bank_client.send_and_confirm_instruction(&mint_keypair, instruction);
            if virtual_address_space_adjustments {
                assert_eq!(
                    result.unwrap_err().unwrap(),
                    TransactionError::InstructionError(0, expected_error)
                );
            } else {
                result.unwrap();
            }
        }
    }
}

#[test]
#[cfg(feature = "sbf_rust")]
fn test_deny_executable_write() {
    agave_logger::setup();

    let GenesisConfigInfo {
        genesis_config,
        mint_keypair,
        ..
    } = create_genesis_config(100_123_456_789);

    for virtual_address_space_adjustments in [false, true] {
        let mut bank = Bank::new_for_tests(&genesis_config);
        let feature_set = Arc::make_mut(&mut bank.feature_set);
        // by default test banks have all features enabled, so we only need to
        // disable when needed
        if !virtual_address_space_adjustments {
            feature_set.deactivate(&feature_set::syscall_parameter_address_restrictions::id());
            feature_set.deactivate(&feature_set::virtual_address_space_adjustments::id());
            feature_set.deactivate(&feature_set::account_data_direct_mapping::id());
        }
        let (bank, bank_forks) = bank.wrap_with_bank_forks_for_tests();
        let mut bank_client = BankClient::new_shared(bank);
        let authority_keypair = Keypair::new();

        let (_bank, invoke_program_id) = load_program_of_loader_v4(
            &mut bank_client,
            &bank_forks,
            &mint_keypair,
            &authority_keypair,
            "solana_sbf_rust_invoke",
        );

        let account_keypair = Keypair::new();
        let mint_pubkey = mint_keypair.pubkey();
        let account_metas = vec![
            AccountMeta::new(mint_pubkey, true),
            AccountMeta::new(account_keypair.pubkey(), false),
            AccountMeta::new_readonly(invoke_program_id, false),
        ];

        let mut instruction_data = vec![TEST_WRITE_ACCOUNT, 2];
        instruction_data.extend_from_slice(3usize.to_le_bytes().as_ref());
        instruction_data.push(42);
        let instruction = Instruction::new_with_bytes(
            invoke_program_id,
            &instruction_data,
            account_metas.clone(),
        );
        let result = bank_client.send_and_confirm_instruction(&mint_keypair, instruction);
        assert_eq!(
            result.unwrap_err().unwrap(),
            TransactionError::InstructionError(0, InstructionError::ReadonlyDataModified)
        );
    }
}

#[test]
fn test_update_callee_account() {
    // Test that fn update_callee_account() works and we are updating the callee account on CPI.
    agave_logger::setup();

    let GenesisConfigInfo {
        genesis_config,
        mint_keypair,
        ..
    } = create_genesis_config(100_123_456_789);

    for virtual_address_space_adjustments in [false, true] {
        let mut bank = Bank::new_for_tests(&genesis_config);
        let feature_set = Arc::make_mut(&mut bank.feature_set);
        // by default test banks have all features enabled, so we only need to
        // disable when needed
        if !virtual_address_space_adjustments {
            feature_set.deactivate(&feature_set::syscall_parameter_address_restrictions::id());
            feature_set.deactivate(&feature_set::virtual_address_space_adjustments::id());
            feature_set.deactivate(&feature_set::account_data_direct_mapping::id());
        }
        let (bank, bank_forks) = bank.wrap_with_bank_forks_for_tests();
        let mut bank_client = BankClient::new_shared(bank.clone());
        let authority_keypair = Keypair::new();

        let (bank, invoke_program_id) = load_program_of_loader_v4(
            &mut bank_client,
            &bank_forks,
            &mint_keypair,
            &authority_keypair,
            "solana_sbf_rust_invoke",
        );

        let account_keypair = Keypair::new();

        let mint_pubkey = mint_keypair.pubkey();

        let account_metas = vec![
            AccountMeta::new(mint_pubkey, true),
            AccountMeta::new(account_keypair.pubkey(), false),
            AccountMeta::new_readonly(invoke_program_id, false),
        ];

        // I. do CPI with account in read only (separate code path with virtual_address_space_adjustments)
        let mut account = AccountSharedData::new(42, 10240, &invoke_program_id);
        let data: Vec<u8> = (0..10240).map(|n| n as u8).collect();
        account.set_data(data);

        bank.store_account(&account_keypair.pubkey(), &account);

        let mut instruction_data = vec![TEST_CALLEE_ACCOUNT_UPDATES, 0, 0];
        instruction_data.extend_from_slice(20480usize.to_le_bytes().as_ref());
        instruction_data.extend_from_slice(0usize.to_le_bytes().as_ref());
        instruction_data.extend_from_slice(16384usize.to_le_bytes().as_ref());
        // instruction data for inner CPI (2x)
        instruction_data.extend_from_slice(&[TEST_CALLEE_ACCOUNT_UPDATES, 0, 0]);
        instruction_data.extend_from_slice(0usize.to_le_bytes().as_ref());
        instruction_data.extend_from_slice(0usize.to_le_bytes().as_ref());
        instruction_data.extend_from_slice(0usize.to_le_bytes().as_ref());

        instruction_data.extend_from_slice(&[TEST_CALLEE_ACCOUNT_UPDATES, 0, 0]);
        instruction_data.extend_from_slice(0usize.to_le_bytes().as_ref());
        instruction_data.extend_from_slice(0usize.to_le_bytes().as_ref());
        instruction_data.extend_from_slice(0usize.to_le_bytes().as_ref());

        let instruction = Instruction::new_with_bytes(
            invoke_program_id,
            &instruction_data,
            account_metas.clone(),
        );
        let result = bank_client.send_and_confirm_instruction(&mint_keypair, instruction);
        assert!(result.is_ok());

        let data = bank_client
            .get_account_data(&account_keypair.pubkey())
            .unwrap()
            .unwrap();

        assert_eq!(data.len(), 20480);

        data.iter().enumerate().for_each(|(i, v)| {
            let expected = match i {
                ..=10240 => i as u8,
                16384 => 0xe5,
                _ => 0,
            };

            assert_eq!(*v, expected, "offset:{i} {v:#x} != {expected:#x}");
        });

        // II. do CPI with account with resize to smaller and write
        let mut account = AccountSharedData::new(42, 10240, &invoke_program_id);
        let data: Vec<u8> = (0..10240).map(|n| n as u8).collect();
        account.set_data(data);
        bank.store_account(&account_keypair.pubkey(), &account);

        let mut instruction_data = vec![TEST_CALLEE_ACCOUNT_UPDATES, 1, 0];
        instruction_data.extend_from_slice(20480usize.to_le_bytes().as_ref());
        instruction_data.extend_from_slice(0usize.to_le_bytes().as_ref());
        instruction_data.extend_from_slice(16384usize.to_le_bytes().as_ref());
        // instruction data for inner CPI
        instruction_data.extend_from_slice(&[TEST_CALLEE_ACCOUNT_UPDATES, 0, 0]);
        instruction_data.extend_from_slice(19480usize.to_le_bytes().as_ref());
        instruction_data.extend_from_slice(0usize.to_le_bytes().as_ref());
        instruction_data.extend_from_slice(8129usize.to_le_bytes().as_ref());

        let instruction = Instruction::new_with_bytes(
            invoke_program_id,
            &instruction_data,
            account_metas.clone(),
        );
        let result = bank_client.send_and_confirm_instruction(&mint_keypair, instruction);
        assert!(result.is_ok());

        let data = bank_client
            .get_account_data(&account_keypair.pubkey())
            .unwrap()
            .unwrap();

        assert_eq!(data.len(), 19480);

        data.iter().enumerate().for_each(|(i, v)| {
            let expected = match i {
                8129 => (i as u8) ^ 0xe5,
                ..=10240 => i as u8,
                16384 => 0xe5,
                _ => 0,
            };

            assert_eq!(*v, expected, "offset:{i} {v:#x} != {expected:#x}");
        });

        // III. do CPI with account with resize to larger and write
        let mut account = AccountSharedData::new(42, 10240, &invoke_program_id);
        let data: Vec<u8> = (0..10240).map(|n| n as u8).collect();
        account.set_data(data);
        bank.store_account(&account_keypair.pubkey(), &account);

        let mut instruction_data = vec![TEST_CALLEE_ACCOUNT_UPDATES, 1, 0];
        instruction_data.extend_from_slice(16384usize.to_le_bytes().as_ref());
        instruction_data.extend_from_slice(0usize.to_le_bytes().as_ref());
        instruction_data.extend_from_slice(16384usize.to_le_bytes().as_ref());
        // instruction data for inner CPI
        instruction_data.extend_from_slice(&[TEST_CALLEE_ACCOUNT_UPDATES, 0, 0]);
        instruction_data.extend_from_slice(20480usize.to_le_bytes().as_ref());
        instruction_data.extend_from_slice(0usize.to_le_bytes().as_ref());
        instruction_data.extend_from_slice(16385usize.to_le_bytes().as_ref());

        let instruction = Instruction::new_with_bytes(
            invoke_program_id,
            &instruction_data,
            account_metas.clone(),
        );
        let result = bank_client.send_and_confirm_instruction(&mint_keypair, instruction);
        assert!(result.is_ok());

        let data = bank_client
            .get_account_data(&account_keypair.pubkey())
            .unwrap()
            .unwrap();

        assert_eq!(data.len(), 20480);

        data.iter().enumerate().for_each(|(i, v)| {
            let expected = match i {
                ..=10240 => i as u8,
                16384 | 16385 => 0xe5,
                _ => 0,
            };

            assert_eq!(*v, expected, "offset:{i} {v:#x} != {expected:#x}");
        });

        // IV. do CPI with account with resize to larger and write
        let mut account = AccountSharedData::new(42, 10240, &invoke_program_id);
        let data: Vec<u8> = (0..10240).map(|n| n as u8).collect();
        account.set_data(data);
        bank.store_account(&account_keypair.pubkey(), &account);

        let mut instruction_data = vec![TEST_CALLEE_ACCOUNT_UPDATES, 1, 0];
        instruction_data.extend_from_slice(16384usize.to_le_bytes().as_ref());
        instruction_data.extend_from_slice(0usize.to_le_bytes().as_ref());
        instruction_data.extend_from_slice(16384usize.to_le_bytes().as_ref());
        // instruction data for inner CPI (2x)
        instruction_data.extend_from_slice(&[TEST_CALLEE_ACCOUNT_UPDATES, 1, 0]);
        instruction_data.extend_from_slice(0usize.to_le_bytes().as_ref());
        instruction_data.extend_from_slice(0usize.to_le_bytes().as_ref());
        instruction_data.extend_from_slice(0usize.to_le_bytes().as_ref());

        instruction_data.extend_from_slice(&[TEST_CALLEE_ACCOUNT_UPDATES, 1, 0]);
        instruction_data.extend_from_slice(0usize.to_le_bytes().as_ref());
        instruction_data.extend_from_slice(0usize.to_le_bytes().as_ref());
        instruction_data.extend_from_slice(0usize.to_le_bytes().as_ref());
        // instruction data for inner CPI
        instruction_data.extend_from_slice(&[TEST_CALLEE_ACCOUNT_UPDATES, 0, 0]);
        instruction_data.extend_from_slice(20480usize.to_le_bytes().as_ref());
        instruction_data.extend_from_slice(0usize.to_le_bytes().as_ref());
        instruction_data.extend_from_slice(16385usize.to_le_bytes().as_ref());

        let instruction = Instruction::new_with_bytes(
            invoke_program_id,
            &instruction_data,
            account_metas.clone(),
        );
        let result = bank_client.send_and_confirm_instruction(&mint_keypair, instruction);
        assert!(result.is_ok());

        let data = bank_client
            .get_account_data(&account_keypair.pubkey())
            .unwrap()
            .unwrap();

        assert_eq!(data.len(), 20480);

        data.iter().enumerate().for_each(|(i, v)| {
            let expected = match i {
                ..=10240 => i as u8,
                16384 | 16385 => 0xe5,
                _ => 0,
            };

            assert_eq!(*v, expected, "offset:{i} {v:#x} != {expected:#x}");
        });

        // V. clone data, modify and CPI
        let mut account = AccountSharedData::new(42, 10240, &invoke_program_id);
        let data: Vec<u8> = (0..10240).map(|n| n as u8).collect();
        account.set_data(data);

        bank.store_account(&account_keypair.pubkey(), &account);

        let mut instruction_data = vec![TEST_CALLEE_ACCOUNT_UPDATES, 1, 1];
        instruction_data.extend_from_slice(0usize.to_le_bytes().as_ref());
        instruction_data.extend_from_slice(0usize.to_le_bytes().as_ref());
        instruction_data.extend_from_slice(8190usize.to_le_bytes().as_ref());

        // instruction data for inner CPI
        instruction_data.extend_from_slice(&[TEST_CALLEE_ACCOUNT_UPDATES, 1, 0]);
        instruction_data.extend_from_slice(0usize.to_le_bytes().as_ref());
        instruction_data.extend_from_slice(0usize.to_le_bytes().as_ref());
        instruction_data.extend_from_slice(8191usize.to_le_bytes().as_ref());

        let instruction = Instruction::new_with_bytes(
            invoke_program_id,
            &instruction_data,
            account_metas.clone(),
        );
        let result = bank_client.send_and_confirm_instruction(&mint_keypair, instruction);

        if virtual_address_space_adjustments {
            // changing the data pointer is not permitted
            assert!(result.is_err());
        } else {
            assert!(result.is_ok());

            let data = bank_client
                .get_account_data(&account_keypair.pubkey())
                .unwrap()
                .unwrap();

            assert_eq!(data.len(), 10240);

            data.iter().enumerate().for_each(|(i, v)| {
                let expected = match i {
                    // since the data is was cloned, the write to 8191 was lost
                    8190 => (i as u8) ^ 0xe5,
                    ..=10240 => i as u8,
                    _ => 0,
                };

                assert_eq!(*v, expected, "offset:{i} {v:#x} != {expected:#x}");
            });
        }
    }
}

#[test]
fn test_account_info_in_account() {
    agave_logger::setup();

    let GenesisConfigInfo {
        genesis_config,
        mint_keypair,
        ..
    } = create_genesis_config(100_123_456_789);

    let mut programs = Vec::new();
    #[cfg(feature = "sbf_c")]
    {
        programs.push("invoke");
    }
    #[cfg(feature = "sbf_rust")]
    {
        programs.push("solana_sbf_rust_invoke");
    }

    for program in programs {
        for syscall_parameter_address_restrictions in [false, true] {
            let mut bank = Bank::new_for_tests(&genesis_config);
            let feature_set = Arc::make_mut(&mut bank.feature_set);
            // by default test banks have all features enabled, so we only need to
            // disable when needed
            if !syscall_parameter_address_restrictions {
                feature_set.deactivate(&feature_set::syscall_parameter_address_restrictions::id());
                feature_set.deactivate(&feature_set::virtual_address_space_adjustments::id());
                feature_set.deactivate(&feature_set::account_data_direct_mapping::id());
            }

            let (bank, bank_forks) = bank.wrap_with_bank_forks_for_tests();
            let mut bank_client = BankClient::new_shared(bank.clone());
            let authority_keypair = Keypair::new();

            let (bank, invoke_program_id) = load_program_of_loader_v4(
                &mut bank_client,
                &bank_forks,
                &mint_keypair,
                &authority_keypair,
                program,
            );

            let account_keypair = Keypair::new();

            let mint_pubkey = mint_keypair.pubkey();

            let account_metas = vec![
                AccountMeta::new(mint_pubkey, true),
                AccountMeta::new(account_keypair.pubkey(), false),
                AccountMeta::new_readonly(invoke_program_id, false),
            ];

            let mut instruction_data = vec![TEST_ACCOUNT_INFO_IN_ACCOUNT];
            instruction_data.extend_from_slice(32usize.to_le_bytes().as_ref());

            let instruction =
                Instruction::new_with_bytes(invoke_program_id, &instruction_data, account_metas);

            let account = AccountSharedData::new(42, 10240, &invoke_program_id);

            bank.store_account(&account_keypair.pubkey(), &account);

            let result = bank_client.send_and_confirm_instruction(&mint_keypair, instruction);
            if syscall_parameter_address_restrictions {
                assert!(result.is_err());
            } else {
                assert!(result.is_ok());
            }
        }
    }
}

#[test]
fn test_account_info_rc_in_account() {
    agave_logger::setup();

    let GenesisConfigInfo {
        genesis_config,
        mint_keypair,
        ..
    } = create_genesis_config(100_123_456_789);

    for syscall_parameter_address_restrictions in [false, true] {
        let mut bank = Bank::new_for_tests(&genesis_config);
        let feature_set = Arc::make_mut(&mut bank.feature_set);
        // by default test banks have all features enabled, so we only need to
        // disable when needed
        if !syscall_parameter_address_restrictions {
            feature_set.deactivate(&feature_set::syscall_parameter_address_restrictions::id());
            feature_set.deactivate(&feature_set::virtual_address_space_adjustments::id());
            feature_set.deactivate(&feature_set::account_data_direct_mapping::id());
        }

        let (bank, bank_forks) = bank.wrap_with_bank_forks_for_tests();
        let mut bank_client = BankClient::new_shared(bank.clone());
        let authority_keypair = Keypair::new();

        let (bank, invoke_program_id) = load_program_of_loader_v4(
            &mut bank_client,
            &bank_forks,
            &mint_keypair,
            &authority_keypair,
            "solana_sbf_rust_invoke",
        );

        let account_keypair = Keypair::new();

        let mint_pubkey = mint_keypair.pubkey();

        let account_metas = vec![
            AccountMeta::new(mint_pubkey, true),
            AccountMeta::new(account_keypair.pubkey(), false),
            AccountMeta::new_readonly(invoke_program_id, false),
        ];

        let instruction_data = vec![TEST_ACCOUNT_INFO_LAMPORTS_RC, 0, 0, 0];

        let instruction = Instruction::new_with_bytes(
            invoke_program_id,
            &instruction_data,
            account_metas.clone(),
        );

        let account = AccountSharedData::new(42, 10240, &invoke_program_id);

        bank.store_account(&account_keypair.pubkey(), &account);

        let message = Message::new(&[instruction], Some(&mint_pubkey));
        let tx = Transaction::new(&[&mint_keypair], message.clone(), bank.last_blockhash());
        let (result, _, logs, _) = process_transaction_and_record_inner(&bank, tx);

        if syscall_parameter_address_restrictions {
            assert!(
                logs.last().unwrap().ends_with(" failed: Invalid pointer"),
                "{logs:?}"
            );
            assert!(result.is_err());
        } else {
            assert!(result.is_ok(), "{logs:?}");
        }

        let instruction_data = vec![TEST_ACCOUNT_INFO_DATA_RC, 0, 0, 0];

        let instruction =
            Instruction::new_with_bytes(invoke_program_id, &instruction_data, account_metas);

        let account = AccountSharedData::new(42, 10240, &invoke_program_id);

        bank.store_account(&account_keypair.pubkey(), &account);

        let message = Message::new(&[instruction], Some(&mint_pubkey));
        let tx = Transaction::new(&[&mint_keypair], message.clone(), bank.last_blockhash());
        let (result, _, logs, _) = process_transaction_and_record_inner(&bank, tx);

        if syscall_parameter_address_restrictions {
            assert!(
                logs.last().unwrap().ends_with(" failed: Invalid pointer"),
                "{logs:?}"
            );
            assert!(result.is_err());
        } else {
            assert!(result.is_ok(), "{logs:?}");
        }
    }
}

#[test]
fn test_clone_account_data() {
    // Test cloning account data works as expect with
    agave_logger::setup();

    let GenesisConfigInfo {
        genesis_config,
        mint_keypair,
        ..
    } = create_genesis_config(100_123_456_789);

    let mut bank = Bank::new_for_tests(&genesis_config);
    let feature_set = Arc::make_mut(&mut bank.feature_set);

    feature_set.deactivate(&feature_set::syscall_parameter_address_restrictions::id());
    feature_set.deactivate(&feature_set::virtual_address_space_adjustments::id());
    feature_set.deactivate(&feature_set::account_data_direct_mapping::id());

    let (bank, bank_forks) = bank.wrap_with_bank_forks_for_tests();
    let mut bank_client = BankClient::new_shared(bank.clone());
    let authority_keypair = Keypair::new();

    let (_bank, invoke_program_id) = load_program_of_loader_v4(
        &mut bank_client,
        &bank_forks,
        &mint_keypair,
        &authority_keypair,
        "solana_sbf_rust_invoke",
    );
    let (bank, invoke_program_id2) = load_program_of_loader_v4(
        &mut bank_client,
        &bank_forks,
        &mint_keypair,
        &authority_keypair,
        "solana_sbf_rust_invoke",
    );

    let account_keypair = Keypair::new();

    let mint_pubkey = mint_keypair.pubkey();

    let account_metas = vec![
        AccountMeta::new(mint_pubkey, true),
        AccountMeta::new(account_keypair.pubkey(), false),
        AccountMeta::new_readonly(invoke_program_id2, false),
        AccountMeta::new_readonly(invoke_program_id, false),
    ];

    // I. clone data and CPI; modify data in callee.
    // Now the original data in the caller is unmodified, and we get a "instruction modified data of an account it does not own"
    // error in the caller
    let mut account = AccountSharedData::new(42, 10240, &invoke_program_id2);
    let data: Vec<u8> = (0..10240).map(|n| n as u8).collect();
    account.set_data(data);

    bank.store_account(&account_keypair.pubkey(), &account);

    let mut instruction_data = vec![TEST_CALLEE_ACCOUNT_UPDATES, 1, 1];
    instruction_data.extend_from_slice(0usize.to_le_bytes().as_ref());
    instruction_data.extend_from_slice(0usize.to_le_bytes().as_ref());
    instruction_data.extend_from_slice(0usize.to_le_bytes().as_ref());

    // instruction data for inner CPI: modify account
    instruction_data.extend_from_slice(&[TEST_CALLEE_ACCOUNT_UPDATES, 0, 0]);
    instruction_data.extend_from_slice(0usize.to_le_bytes().as_ref());
    instruction_data.extend_from_slice(0usize.to_le_bytes().as_ref());
    instruction_data.extend_from_slice(8190usize.to_le_bytes().as_ref());

    let instruction =
        Instruction::new_with_bytes(invoke_program_id, &instruction_data, account_metas.clone());

    let message = Message::new(&[instruction], Some(&mint_pubkey));
    let tx = Transaction::new(&[&mint_keypair], message.clone(), bank.last_blockhash());
    let (result, _, logs, _) = process_transaction_and_record_inner(&bank, tx);
    assert!(result.is_err(), "{result:?}");
    let error = format!(
        "Program {invoke_program_id} failed: instruction modified data of an account it does not \
         own"
    );
    assert!(logs.iter().any(|log| log.contains(&error)), "{logs:?}");

    // II. clone data, modify and then CPI
    // The deserialize checks should verify that we're not allowed to modify an account we don't own, even though
    // we have only modified a copy of the data. Fails in caller
    let mut account = AccountSharedData::new(42, 10240, &invoke_program_id2);
    let data: Vec<u8> = (0..10240).map(|n| n as u8).collect();
    account.set_data(data);

    bank.store_account(&account_keypair.pubkey(), &account);

    let mut instruction_data = vec![TEST_CALLEE_ACCOUNT_UPDATES, 1, 1];
    instruction_data.extend_from_slice(0usize.to_le_bytes().as_ref());
    instruction_data.extend_from_slice(8190usize.to_le_bytes().as_ref());
    instruction_data.extend_from_slice(0usize.to_le_bytes().as_ref());

    // instruction data for inner CPI
    instruction_data.extend_from_slice(&[TEST_CALLEE_ACCOUNT_UPDATES, 0, 0]);
    instruction_data.extend_from_slice(0usize.to_le_bytes().as_ref());
    instruction_data.extend_from_slice(0usize.to_le_bytes().as_ref());
    instruction_data.extend_from_slice(0usize.to_le_bytes().as_ref());

    let instruction =
        Instruction::new_with_bytes(invoke_program_id, &instruction_data, account_metas.clone());

    let message = Message::new(&[instruction], Some(&mint_pubkey));
    let tx = Transaction::new(&[&mint_keypair], message.clone(), bank.last_blockhash());
    let (result, _, logs, _) = process_transaction_and_record_inner(&bank, tx);
    assert!(result.is_err(), "{result:?}");
    let error = format!(
        "Program {invoke_program_id} failed: instruction modified data of an account it does not \
         own"
    );
    assert!(logs.iter().any(|log| log.contains(&error)), "{logs:?}");

    // II. Clone data, call, modifiy in callee and then make the same change in the caller - transaction succeeds
    // Note the caller needs to modify the original account data, not the copy
    let mut account = AccountSharedData::new(42, 10240, &invoke_program_id2);
    let data: Vec<u8> = (0..10240).map(|n| n as u8).collect();
    account.set_data(data);

    bank.store_account(&account_keypair.pubkey(), &account);

    let mut instruction_data = vec![TEST_CALLEE_ACCOUNT_UPDATES, 1, 1];
    instruction_data.extend_from_slice(0usize.to_le_bytes().as_ref());
    instruction_data.extend_from_slice(0usize.to_le_bytes().as_ref());
    instruction_data.extend_from_slice(8190usize.to_le_bytes().as_ref());

    // instruction data for inner CPI
    instruction_data.extend_from_slice(&[TEST_CALLEE_ACCOUNT_UPDATES, 0, 0]);
    instruction_data.extend_from_slice(0usize.to_le_bytes().as_ref());
    instruction_data.extend_from_slice(0usize.to_le_bytes().as_ref());
    instruction_data.extend_from_slice(8190usize.to_le_bytes().as_ref());

    let instruction =
        Instruction::new_with_bytes(invoke_program_id, &instruction_data, account_metas.clone());
    let result = bank_client.send_and_confirm_instruction(&mint_keypair, instruction);

    // works because the account is exactly the same in caller as callee
    assert!(result.is_ok(), "{result:?}");
}

#[test]
fn test_stack_heap_zeroed() {
    agave_logger::setup();

    let GenesisConfigInfo {
        genesis_config,
        mint_keypair,
        ..
    } = create_genesis_config(100_123_456_789);

    let bank = Bank::new_for_tests(&genesis_config);

    let (bank, bank_forks) = bank.wrap_with_bank_forks_for_tests();
    let mut bank_client = BankClient::new_shared(bank);
    let authority_keypair = Keypair::new();

    let (bank, invoke_program_id) = load_program_of_loader_v4(
        &mut bank_client,
        &bank_forks,
        &mint_keypair,
        &authority_keypair,
        "solana_sbf_rust_invoke",
    );

    let account_keypair = Keypair::new();
    let mint_pubkey = mint_keypair.pubkey();
    let account_metas = vec![
        AccountMeta::new(mint_pubkey, true),
        AccountMeta::new(account_keypair.pubkey(), false),
        AccountMeta::new_readonly(invoke_program_id, false),
    ];

    // Check multiple heap sizes. It's generally a good idea, and also it's needed to ensure that
    // pooled heap and stack values are reused - and therefore zeroed - across executions.
    for heap_len in [32usize * 1024, 64 * 1024, 128 * 1024, 256 * 1024] {
        // TEST_STACK_HEAP_ZEROED will recursively check that stack and heap are zeroed until it
        // reaches max CPI invoke depth. We make it fail at max depth so we're sure that there's no
        // legit way to access non-zeroed stack and heap regions.
        let mut instruction_data = vec![TEST_STACK_HEAP_ZEROED];
        instruction_data.extend_from_slice(&heap_len.to_le_bytes());

        let instruction = Instruction::new_with_bytes(
            invoke_program_id,
            &instruction_data,
            account_metas.clone(),
        );

        let message = Message::new(
            &[
                ComputeBudgetInstruction::set_compute_unit_limit(1_400_000),
                ComputeBudgetInstruction::request_heap_frame(heap_len as u32),
                instruction,
            ],
            Some(&mint_pubkey),
        );
        let tx = Transaction::new(&[&mint_keypair], message.clone(), bank.last_blockhash());
        let (result, _, logs, _) = process_transaction_and_record_inner(&bank, tx);
        assert!(result.is_err(), "{result:?}");
        assert!(
            logs.iter()
                .any(|log| log.contains("Cross-program invocation call depth too deep")),
            "{logs:?}"
        );
    }
}

#[test]
fn test_function_call_args() {
    // This function tests edge compiler edge cases when calling functions with more than five
    // arguments and passing by value arguments with more than 16 bytes.
    agave_logger::setup();

    let GenesisConfigInfo {
        genesis_config,
        mint_keypair,
        ..
    } = create_genesis_config(100_123_456_789);

    let (bank, bank_forks) = Bank::new_with_bank_forks_for_tests(&genesis_config);
    let mut bank_client = BankClient::new_shared(bank);
    let authority_keypair = Keypair::new();

    let (bank, program_id) = load_program_of_loader_v4(
        &mut bank_client,
        &bank_forks,
        &mint_keypair,
        &authority_keypair,
        "solana_sbf_rust_call_args",
    );

    #[derive(BorshSerialize, BorshDeserialize, PartialEq, Eq, Debug)]
    struct Test128 {
        a: u128,
        b: u128,
    }

    #[derive(BorshSerialize)]
    struct InputData {
        test_128: Test128,
        arg1: i64,
        arg2: i64,
        arg3: i64,
        arg4: i64,
        arg5: i64,
        arg6: i64,
        arg7: i64,
        arg8: i64,
    }

    #[derive(BorshDeserialize)]
    struct OutputData {
        res_128: u128,
        res_256: Test128,
        many_args_1: i64,
        many_args_2: i64,
    }

    let input_data = InputData {
        test_128: Test128 {
            a: rand::random::<u128>(),
            b: rand::random::<u128>(),
        },
        arg1: rand::random::<i64>(),
        arg2: rand::random::<i64>(),
        arg3: rand::random::<i64>(),
        arg4: rand::random::<i64>(),
        arg5: rand::random::<i64>(),
        arg6: rand::random::<i64>(),
        arg7: rand::random::<i64>(),
        arg8: rand::random::<i64>(),
    };

    let instruction_data = to_vec(&input_data).unwrap();
    let account_metas = vec![
        AccountMeta::new(mint_keypair.pubkey(), true),
        AccountMeta::new(Keypair::new().pubkey(), false),
    ];

    let instruction = Instruction::new_with_bytes(program_id, &instruction_data, account_metas);
    let message = Message::new(&[instruction], Some(&mint_keypair.pubkey()));

    let tx = Transaction::new(&[&mint_keypair], message.clone(), bank.last_blockhash());

    let txs = vec![tx];
    let tx_batch = bank.prepare_batch_for_tests(txs);
    let result = bank
        .load_execute_and_commit_transactions(
            &tx_batch,
            MAX_PROCESSING_AGE,
            ExecutionRecordingConfig {
                enable_cpi_recording: false,
                enable_log_recording: false,
                enable_return_data_recording: true,
                enable_transaction_balance_recording: false,
            },
            &mut ExecuteTimings::default(),
            None,
        )
        .0;

    fn verify_many_args(input: &InputData) -> i64 {
        let a = input
            .arg1
            .overflowing_add(input.arg2)
            .0
            .overflowing_sub(input.arg3)
            .0
            .overflowing_add(input.arg4)
            .0
            .overflowing_sub(input.arg5)
            .0;
        (a % input.arg6)
            .overflowing_sub(input.arg7)
            .0
            .overflowing_add(input.arg8)
            .0
    }

    let return_data = &result[0]
        .as_ref()
        .unwrap()
        .return_data
        .as_ref()
        .unwrap()
        .data;
    let decoded: OutputData = from_slice::<OutputData>(return_data).unwrap();
    assert_eq!(
        decoded.res_128,
        input_data.test_128.a % input_data.test_128.b
    );
    assert_eq!(
        decoded.res_256,
        Test128 {
            a: input_data
                .test_128
                .a
                .overflowing_add(input_data.test_128.b)
                .0,
            b: input_data
                .test_128
                .a
                .overflowing_sub(input_data.test_128.b)
                .0
        }
    );
    assert_eq!(decoded.many_args_1, verify_many_args(&input_data));
    assert_eq!(decoded.many_args_2, verify_many_args(&input_data));
}

#[test]
#[cfg(feature = "sbf_rust")]
fn test_mem_syscalls_overlap_account_begin_or_end() {
    agave_logger::setup();

    for virtual_address_space_adjustments in [false, true] {
        let GenesisConfigInfo {
            genesis_config,
            mint_keypair,
            ..
        } = create_genesis_config(100_123_456_789);

        let mut bank = Bank::new_for_tests(&genesis_config);
        let mut feature_set = FeatureSet::all_enabled();
        if !virtual_address_space_adjustments {
            feature_set.deactivate(&feature_set::syscall_parameter_address_restrictions::id());
            feature_set.deactivate(&feature_set::virtual_address_space_adjustments::id());
            feature_set.deactivate(&feature_set::account_data_direct_mapping::id());
        }

        let account_keypair = Keypair::new();

        bank.feature_set = Arc::new(feature_set);
        let (bank, bank_forks) = bank.wrap_with_bank_forks_for_tests();
        let mut bank_client = BankClient::new_shared(bank);
        let authority_keypair = Keypair::new();

        let (bank, loader_v4_program_id) = load_program_of_loader_v4(
            &mut bank_client,
            &bank_forks,
            &mint_keypair,
            &authority_keypair,
            "solana_sbf_rust_account_mem",
        );

        let deprecated_program_id = create_program(
            &bank,
            &bpf_loader_deprecated::id(),
            "solana_sbf_rust_account_mem_deprecated",
        );

        let mint_pubkey = mint_keypair.pubkey();

        for deprecated in [false, true] {
            let program_id = if deprecated {
                deprecated_program_id
            } else {
                loader_v4_program_id
            };

            let account_metas = vec![
                AccountMeta::new(mint_pubkey, true),
                AccountMeta::new_readonly(program_id, false),
                AccountMeta::new(account_keypair.pubkey(), false),
            ];

            let account = AccountSharedData::new(42, 1024, &program_id);
            bank.store_account(&account_keypair.pubkey(), &account);

            for instr in 0..=15 {
                println!(
                    "Testing deprecated:{deprecated} virtual_address_space_adjustments: \
                     {virtual_address_space_adjustments} instruction:{instr}"
                );
                let instruction =
                    Instruction::new_with_bytes(program_id, &[instr], account_metas.clone());

                let message = Message::new(&[instruction], Some(&mint_pubkey));
                let tx = Transaction::new(&[&mint_keypair], message.clone(), bank.last_blockhash());
                let (result, _, logs, _) = process_transaction_and_record_inner(&bank, tx);
                let last_line = logs.last().unwrap();

                if virtual_address_space_adjustments {
                    assert!(last_line.contains(" failed: Access violation"), "{logs:?}");
                } else {
                    assert!(result.is_ok(), "{logs:?}");
                }
            }

            let account = AccountSharedData::new(42, 0, &program_id);
            bank.store_account(&account_keypair.pubkey(), &account);

            for instr in 0..=15 {
                println!(
                    "Testing deprecated:{deprecated} virtual_address_space_adjustments: \
                     {virtual_address_space_adjustments} instruction:{instr} zero-length account"
                );
                let instruction =
                    Instruction::new_with_bytes(program_id, &[instr, 0], account_metas.clone());

                let message = Message::new(&[instruction], Some(&mint_pubkey));
                let tx = Transaction::new(&[&mint_keypair], message.clone(), bank.last_blockhash());
                let (result, _, logs, _) = process_transaction_and_record_inner(&bank, tx);
                let last_line = logs.last().unwrap();

                if virtual_address_space_adjustments && (!deprecated || instr < 8) {
                    assert!(
                        last_line.contains(" failed: account data too small")
                            || last_line.contains(" failed: Failed to reallocate account data")
                            || last_line.contains(" failed: Access violation"),
                        "{logs:?}",
                    );
                } else {
                    // virtual_address_space_adjustments && deprecated && instr >= 8 succeeds with zero-length accounts
                    // because there is no MemoryRegion for the account,
                    // so there can be no error when leaving that non-existent region.
                    assert!(result.is_ok(), "{logs:?}");
                }
            }
        }
    }
}

#[test_matrix(
    [0, 1, 2, 5, 10, 15, 20, 32],
    [1, 10, 50, 100, 255, 500, 1000, 1024]
)]
#[allow(clippy::arithmetic_side_effects)]
#[cfg(feature = "sbf_rust")]
fn test_program_sbf_rust_direct_account_pointers(num_accounts: usize, input_data_len: usize) {
    agave_logger::setup();

    let program_elf = harness::file::load_program_elf("solana_sbf_rust_direct_account_pointers");
    let program_id = Pubkey::new_unique();

    let feature_set = SVMFeatureSet::all_enabled();
    let compute_budget = ComputeBudget::new_with_defaults(false, false);

    let mut program_cache = default_program_cache_with_program(
        &program_id,
        &program_elf,
        &feature_set,
        &compute_budget,
    );
    let sysvar_cache = default_sysvar_cache();

    let mut accounts = Vec::new();
    let mut account_metas = Vec::new();

    for i in 0..num_accounts {
        let pubkey = Pubkey::new_unique();

        // Mixed account sizes.
        accounts.push((pubkey, Account::new(0, 100 + (i * 50), &program_id)));

        // Mixed account roles.
        if i % 2 == 0 {
            account_metas.push(AccountMeta::new(pubkey, false));
        } else {
            account_metas.push(AccountMeta::new_readonly(pubkey, false));
        }
    }

    // Add `num_accounts` duplicated accounts.
    for i in 0..num_accounts {
        let pubkey = accounts[i].0;
        account_metas.push(AccountMeta::new(pubkey, false));
    }

    let input_data: Vec<u8> = (0..input_data_len).map(|i| (i % 256) as u8).collect();

    let instruction = Instruction::new_with_bytes(program_id, &input_data, account_metas);

    let context = InstrContext {
        feature_set,
        accounts,
        instruction,
    };

    let effects =
        harness::execute_instr(context, &compute_budget, &mut program_cache, &sysvar_cache)
            .unwrap();

    assert!(effects.result.is_none());
    // `num_accounts * 2` will be added as return data.
    assert_eq!(
        (num_accounts * 2).to_le_bytes().to_vec(),
        effects.return_data
    );
}

// ============================================================================
// F10 subaccount tests — must agree with the constants in
// programs/sbf/rust/subaccount/src/lib.rs.
// ============================================================================

#[cfg(feature = "sbf_rust")]
const SUBACCOUNT_SEED_TAG: &[u8] = b"test-sub";
#[cfg(feature = "sbf_rust")]
const SUBACCOUNT_FUNDING_LAMPORTS: u64 = 2_000_000;

#[cfg(feature = "sbf_rust")]
fn deploy_subaccount_program(
    mint_lamports: u64,
) -> (
    Arc<Bank>,
    BankClient,
    Arc<RwLock<BankForks>>,
    Keypair,
    Pubkey,
) {
    agave_logger::setup();

    let GenesisConfigInfo {
        mut genesis_config,
        mint_keypair,
        ..
    } = create_genesis_config(mint_lamports);

    // F10 subaccounts use a mutual-gate with SIMD-0449: subaccount slots are reserved in
    // parameter serialization ONLY when `direct_account_pointers_in_program_input` is
    // INACTIVE. The default test genesis enables all features, so deactivate 0449 here —
    // otherwise `sol_load_subaccount*` fails with `MaxAccountsExceeded`.
    genesis_config
        .accounts
        .remove(&feature_set::direct_account_pointers_in_program_input::id());

    let (bank, bank_forks) = Bank::new_with_bank_forks_for_tests(&genesis_config);
    let mut bank_client = BankClient::new_shared(bank);
    let authority_keypair = Keypair::new();
    let (bank, program_id) = load_program_of_loader_v4(
        &mut bank_client,
        &bank_forks,
        &mint_keypair,
        &authority_keypair,
        "solana_sbf_rust_subaccount",
    );
    (bank, bank_client, bank_forks, mint_keypair, program_id)
}

#[cfg(feature = "sbf_rust")]
fn subaccount_storage_addr_for(base: &Pubkey, program_id: &Pubkey) -> Pubkey {
    use solana_transaction_context::{create_subaccount_address, subaccount_storage_address};
    let owner_pubkey = create_subaccount_address(&[base.as_ref(), SUBACCOUNT_SEED_TAG], program_id)
        .expect("derive subaccount address");
    subaccount_storage_address(&owner_pubkey)
}

#[cfg(feature = "sbf_rust")]
fn subaccount_create_instruction(
    program_id: Pubkey,
    base: Pubkey,
    payer: Pubkey,
    discriminator: u8,
    payload: &[u8],
    extra_metas: Vec<AccountMeta>,
) -> Instruction {
    let mut account_metas = vec![
        AccountMeta::new(base, false),
        AccountMeta::new(payer, true),
        AccountMeta::new_readonly(system_program::id(), false),
    ];
    account_metas.extend(extra_metas);
    let mut ix_data = Vec::with_capacity(1 + payload.len());
    ix_data.push(discriminator);
    ix_data.extend_from_slice(payload);
    Instruction::new_with_bytes(program_id, &ix_data, account_metas)
}

#[cfg(feature = "sbf_rust")]
fn subaccount_instruction(
    program_id: Pubkey,
    base: Pubkey,
    is_writable: bool,
    discriminator: u8,
    payload: &[u8],
    extra_metas: Vec<AccountMeta>,
) -> Instruction {
    let mut account_metas = vec![if is_writable {
        AccountMeta::new(base, false)
    } else {
        AccountMeta::new_readonly(base, false)
    }];
    account_metas.extend(extra_metas);
    let mut ix_data = Vec::with_capacity(1 + payload.len());
    ix_data.push(discriminator);
    ix_data.extend_from_slice(payload);
    Instruction::new_with_bytes(program_id, &ix_data, account_metas)
}

#[cfg(feature = "sbf_rust")]
fn run_subaccount_tx(
    bank: &Bank,
    mint_keypair: &Keypair,
    instruction: Instruction,
) -> (
    Result<(), TransactionError>,
    Vec<Vec<InnerInstruction>>,
    Vec<String>,
) {
    let blockhash = bank.last_blockhash();
    let tx = Transaction::new_signed_with_payer(
        &[instruction],
        Some(&mint_keypair.pubkey()),
        &[mint_keypair],
        blockhash,
    );
    let (status, inner_instructions, log_messages, _units) =
        process_transaction_and_record_inner(bank, tx);
    (status, inner_instructions, log_messages)
}

#[test]
#[cfg(feature = "sbf_rust")]
fn test_program_sbf_subaccount_create_load_write_unload() {
    use solana_system_interface::instruction as system_instruction;

    let (bank, _bank_client, _bank_forks, mint_keypair, program_id) =
        deploy_subaccount_program(1_000_000_000);

    let base_pubkey = Pubkey::new_unique();
    let payload: &[u8] = b"hello, subaccount world!";
    let payer_pubkey = mint_keypair.pubkey();
    let payer_lamports_before = bank.get_balance(&payer_pubkey);

    let instruction =
        subaccount_create_instruction(program_id, base_pubkey, payer_pubkey, 0, payload, vec![]);
    let (status, inner_instructions, log_messages) =
        run_subaccount_tx(&bank, &mint_keypair, instruction);
    assert!(
        status.is_ok(),
        "tx failed: {status:?}\nlogs:\n{}",
        log_messages.join("\n"),
    );

    let storage_addr = subaccount_storage_addr_for(&base_pubkey, &program_id);
    let stored = bank.get_account(&storage_addr).unwrap_or_else(|| {
        panic!(
            "subaccount storage account {storage_addr} missing\nlogs:\n{}",
            log_messages.join("\n"),
        )
    });
    assert_eq!(stored.lamports(), SUBACCOUNT_FUNDING_LAMPORTS);
    assert_eq!(stored.owner(), &program_id);
    assert_eq!(stored.data(), payload);

    let payer_lamports_after = bank.get_balance(&payer_pubkey);
    let debited = payer_lamports_before.saturating_sub(payer_lamports_after);
    assert_eq!(
        debited, SUBACCOUNT_FUNDING_LAMPORTS,
        "payer debited {debited}, expected {SUBACCOUNT_FUNDING_LAMPORTS}",
    );

    let transfer_seen = inner_instructions.iter().flatten().any(|inner| {
        matches!(
            bincode::deserialize::<system_instruction::SystemInstruction>(
                &inner.instruction.data,
            ),
            Ok(system_instruction::SystemInstruction::Transfer { lamports })
                if lamports == SUBACCOUNT_FUNDING_LAMPORTS
        )
    });
    assert!(
        transfer_seen,
        "expected system_program::Transfer of {SUBACCOUNT_FUNDING_LAMPORTS} in inner instructions",
    );
}

#[test]
#[cfg(feature = "sbf_rust")]
fn test_program_sbf_subaccount_program_isolation() {
    let (_bank, mut bank_client, bank_forks, mint_keypair, program_id) =
        deploy_subaccount_program(1_000_000_000);

    let (bank, program_id2) = load_program_of_loader_v4(
        &mut bank_client,
        &bank_forks,
        &mint_keypair,
        &Keypair::new(),
        "solana_sbf_rust_subaccount",
    );

    let base_pubkey = Pubkey::new_unique();
    let payload: &[u8] = b"hello, subaccount world!";
    let payload2: &[u8] = b"hello from another program!";
    let payer_pubkey = mint_keypair.pubkey();

    let instruction =
        subaccount_create_instruction(program_id, base_pubkey, payer_pubkey, 0, payload, vec![]);
    let (status, _, log_messages) = run_subaccount_tx(&bank, &mint_keypair, instruction);
    assert!(
        status.is_ok(),
        "tx failed: {status:?}\nlogs:\n{}",
        log_messages.join("\n"),
    );

    let instruction2 =
        subaccount_create_instruction(program_id2, base_pubkey, payer_pubkey, 0, payload2, vec![]);
    let (status2, _, log_messages2) = run_subaccount_tx(&bank, &mint_keypair, instruction2);
    assert!(
        status2.is_ok(),
        "tx failed: {status2:?}\nlogs:\n{}",
        log_messages2.join("\n"),
    );

    let storage_addr = subaccount_storage_addr_for(&base_pubkey, &program_id);
    let stored = bank.get_account(&storage_addr).unwrap_or_else(|| {
        panic!(
            "subaccount storage account {storage_addr} missing\nlogs:\n{}",
            log_messages.join("\n"),
        )
    });
    assert_eq!(stored.lamports(), SUBACCOUNT_FUNDING_LAMPORTS);
    assert_eq!(stored.owner(), &program_id);
    assert_eq!(stored.data(), payload);

    let storage_addr2 = subaccount_storage_addr_for(&base_pubkey, &program_id2);
    let stored2 = bank.get_account(&storage_addr2).unwrap_or_else(|| {
        panic!(
            "subaccount storage account2 {storage_addr2} missing\nlogs:\n{}",
            log_messages2.join("\n"),
        )
    });
    assert_eq!(stored2.lamports(), SUBACCOUNT_FUNDING_LAMPORTS);
    assert_eq!(stored2.owner(), &program_id2);
    assert_eq!(stored2.data(), payload2);

    assert_ne!(
        storage_addr, storage_addr2,
        "storage addresses must be different for different programs"
    );
}

#[test]
#[cfg(feature = "sbf_rust")]
fn test_program_sbf_subaccount_create_empty_subaccount() {
    let (bank, _bank_client, _bank_forks, mint_keypair, program_id) =
        deploy_subaccount_program(1_000_000_000);

    let base_pubkey = Pubkey::new_unique();
    let payer_pubkey = mint_keypair.pubkey();

    let payload: &[u8] = &[]; // empty payload, tests that zero-length accounts work and that the program can handle them
    let instruction =
        subaccount_create_instruction(program_id, base_pubkey, payer_pubkey, 0, payload, vec![]);
    let (status, _, log_messages) = run_subaccount_tx(&bank, &mint_keypair, instruction);
    assert!(
        status.is_ok(),
        "tx failed: {status:?}\nlogs:\n{}",
        log_messages.join("\n"),
    );

    let storage_addr = subaccount_storage_addr_for(&base_pubkey, &program_id);
    let stored = bank.get_account(&storage_addr).unwrap_or_else(|| {
        panic!(
            "subaccount storage account {storage_addr} missing\nlogs:\n{}",
            log_messages.join("\n"),
        )
    });
    assert_eq!(stored.lamports(), SUBACCOUNT_FUNDING_LAMPORTS);
    assert_eq!(stored.owner(), &program_id);
    assert_eq!(stored.data(), payload);
    assert_eq!(
        stored.data().len(),
        0,
        "stored data length must be zero for empty payload"
    );
}

#[test]
#[cfg(feature = "sbf_rust")]
fn test_program_sbf_subaccount_transfer_lamports() {
    let (bank, _bank_client, _bank_forks, mint_keypair, program_id) =
        deploy_subaccount_program(1_000_000_000);

    let base_pubkey = Pubkey::new_unique();
    let payer_pubkey = mint_keypair.pubkey();

    let payload: &[u8] = &[]; // empty payload, tests that zero-length accounts work and that the program can handle them
    let instruction =
        subaccount_create_instruction(program_id, base_pubkey, payer_pubkey, 0, payload, vec![]);
    let (status, _, log_messages) = run_subaccount_tx(&bank, &mint_keypair, instruction);
    assert!(
        status.is_ok(),
        "tx failed: {status:?}\nlogs:\n{}",
        log_messages.join("\n"),
    );

    let storage_addr = subaccount_storage_addr_for(&base_pubkey, &program_id);
    let stored = bank.get_account(&storage_addr).unwrap_or_else(|| {
        panic!(
            "subaccount storage account {storage_addr} missing\nlogs:\n{}",
            log_messages.join("\n"),
        )
    });
    assert_eq!(stored.lamports(), SUBACCOUNT_FUNDING_LAMPORTS);
    assert_eq!(stored.owner(), &program_id);

    const BALANCED: bool = true;
    const UNLOAD: bool = true;

    // The program can't spend the lamports from the readolny subaccount
    let extra_metas = vec![AccountMeta::new(payer_pubkey, false)];
    let instruction = subaccount_instruction(
        program_id,
        base_pubkey,
        false,
        7,
        &[BALANCED as u8, UNLOAD as u8],
        extra_metas,
    );
    let (status, _, log_messages) = run_subaccount_tx(&bank, &mint_keypair, instruction);
    assert_eq!(
        status,
        Err(TransactionError::InstructionError(
            0,
            InstructionError::ReadonlyLamportChange
        )),
        "tx status mismatch: {status:?}\nlogs:\n{}",
        log_messages.join("\n"),
    );

    // The program can spend the lamports from the subaccount
    let extra_metas = vec![AccountMeta::new(payer_pubkey, false)];
    let instruction = subaccount_instruction(
        program_id,
        base_pubkey,
        true,
        7,
        &[BALANCED as u8, UNLOAD as u8],
        extra_metas,
    );
    let (status, _, log_messages) = run_subaccount_tx(&bank, &mint_keypair, instruction);
    assert!(
        status.is_ok(),
        "tx failed: {status:?}\nlogs:\n{}",
        log_messages.join("\n"),
    );
    let extra_metas = vec![AccountMeta::new(payer_pubkey, false)];
    let instruction = subaccount_instruction(
        program_id,
        base_pubkey,
        true,
        7,
        &[BALANCED as u8, !UNLOAD as u8],
        extra_metas,
    );
    let (status, _, log_messages) = run_subaccount_tx(&bank, &mint_keypair, instruction);
    assert!(
        status.is_ok(),
        "tx failed: {status:?}\nlogs:\n{}",
        log_messages.join("\n"),
    );

    // The transaction fails if the program tries unbalanced transfer
    let extra_metas = vec![AccountMeta::new(payer_pubkey, false)];
    let instruction = subaccount_instruction(
        program_id,
        base_pubkey,
        true,
        7,
        &[!BALANCED as u8, UNLOAD as u8],
        extra_metas,
    );
    let (status, _, log_messages) = run_subaccount_tx(&bank, &mint_keypair, instruction);
    assert_eq!(
        status,
        Err(TransactionError::InstructionError(
            0,
            InstructionError::UnbalancedInstruction
        )),
        "tx status mismatch: {status:?}\nlogs:\n{}",
        log_messages.join("\n"),
    );
    let extra_metas = vec![AccountMeta::new(payer_pubkey, false)];
    let instruction = subaccount_instruction(
        program_id,
        base_pubkey,
        true,
        7,
        &[!BALANCED as u8, !UNLOAD as u8],
        extra_metas,
    );
    let (status, _, log_messages) = run_subaccount_tx(&bank, &mint_keypair, instruction);
    assert_eq!(
        status,
        Err(TransactionError::InstructionError(
            0,
            InstructionError::UnbalancedInstruction
        )),
        "tx status mismatch: {status:?}\nlogs:\n{}",
        log_messages.join("\n"),
    );
}

/// Scenario 2: subaccount state persists across two separate transactions.
/// Tx1 creates the subaccount and writes "v1"-sized payload; Tx2 finds the
/// existing subaccount by seeds (load syscall must locate the persisted
/// on-chain entry), then overwrites its data with a different payload.
/// Final on-chain state must match Tx2's payload while lamports stay at the
/// funding amount from Tx1 (no second Transfer).
#[test]
#[cfg(feature = "sbf_rust")]
fn test_program_sbf_subaccount_persists_across_transactions() {
    let (bank, mut bank_client, bank_forks, mint_keypair, program_id) =
        deploy_subaccount_program(1_000_000_000);

    let payload_v1: &[u8] = b"persist-marker-v1______________"; // 31 bytes
    let payload_v2: &[u8] = b"###tx2-overwrite-payload###____"; // 31 bytes, same len
    assert_eq!(payload_v1.len(), payload_v2.len());

    let payer_pubkey = mint_keypair.pubkey();
    let base_pubkey = Pubkey::new_unique();

    // Tx1 — create + write v1.
    let ix1 =
        subaccount_create_instruction(program_id, base_pubkey, payer_pubkey, 0, payload_v1, vec![]);
    let (status, _, logs) = run_subaccount_tx(&bank, &mint_keypair, ix1);
    assert!(
        status.is_ok(),
        "Tx1 failed: {status:?}\nlogs:\n{}",
        logs.join("\n")
    );

    // Advance into a new bank slot so Tx2 runs on a fresh frame.
    let bank = bank_client
        .advance_slot(1, bank_forks.as_ref(), &Pubkey::default())
        .expect("advance slot for Tx2");

    // Tx2 — load existing + overwrite with v2.
    let ix2 = subaccount_instruction(program_id, base_pubkey, true, 1, payload_v2, vec![]);
    let (status, _, logs) = run_subaccount_tx(&bank, &mint_keypair, ix2);
    assert!(
        status.is_ok(),
        "Tx2 failed: {status:?}\nlogs:\n{}",
        logs.join("\n")
    );

    let storage_addr = subaccount_storage_addr_for(&base_pubkey, &program_id);
    let stored = bank
        .get_account(&storage_addr)
        .expect("subaccount must still exist after Tx2");
    assert_eq!(
        stored.data(),
        payload_v2,
        "Tx2's overwrite must be on-chain (proves Tx1's storage was loaded by Tx2)",
    );
    // Lamports are unchanged from Tx1 — Tx2 did no funding.
    assert_eq!(stored.lamports(), SUBACCOUNT_FUNDING_LAMPORTS);
    assert_eq!(stored.owner(), &program_id);
}

/// `sol_load_subaccount_snapshot_*` reads the start-of-block (parent-slot)
/// state and does not require the base account to be present in the
/// transaction. Here the subaccount is created in one block and snapshot-read
/// in the next, with the base account deliberately omitted from the reading
/// transaction (disc=10 takes the base pubkey from instruction data).
#[test]
#[cfg(feature = "sbf_rust")]
fn test_program_sbf_subaccount_snapshot_reads_block_start_without_base() {
    let (bank, mut bank_client, bank_forks, mint_keypair, program_id) =
        deploy_subaccount_program(1_000_000_000);

    let v1: &[u8] = b"snapshot-marker-v1_____________"; // 31 bytes
    let payer_pubkey = mint_keypair.pubkey();
    let base_pubkey = Pubkey::new_unique();

    // Block A — create the subaccount with V1.
    let ix1 = subaccount_create_instruction(program_id, base_pubkey, payer_pubkey, 0, v1, vec![]);
    let (status, _, logs) = run_subaccount_tx(&bank, &mint_keypair, ix1);
    assert!(
        status.is_ok(),
        "create failed: {status:?}\nlogs:\n{}",
        logs.join("\n")
    );

    // Advance to a child block so the V1 write lives in the parent slot.
    let bank = bank_client
        .advance_slot(1, bank_forks.as_ref(), &Pubkey::default())
        .expect("advance slot for snapshot read");

    // Snapshot-read with NO base account in the transaction (empty account
    // metas). Payload = base pubkey ++ expected start-of-block bytes (V1).
    let mut data = vec![10u8];
    data.extend_from_slice(base_pubkey.as_ref());
    data.extend_from_slice(v1);
    let ix2 = Instruction::new_with_bytes(program_id, &data, vec![]);
    let (status, _, logs) = run_subaccount_tx(&bank, &mint_keypair, ix2);
    assert!(
        status.is_ok(),
        "base-free snapshot read failed: {status:?}\nlogs:\n{}",
        logs.join("\n")
    );
}

/// A `sol_load_subaccount_snapshot_*` read sees the start-of-block state even
/// when an earlier transaction in the same block has overwritten the
/// subaccount: the snapshot must observe the parent-slot value, not the live
/// mid-block one.
#[test]
#[cfg(feature = "sbf_rust")]
fn test_program_sbf_subaccount_snapshot_ignores_midblock_write() {
    let (bank, mut bank_client, bank_forks, mint_keypair, program_id) =
        deploy_subaccount_program(1_000_000_000);

    let v1: &[u8] = b"snapshot-block-start-value-v1__"; // 31 bytes
    let v2: &[u8] = b"midblock-overwrite-value-v2____"; // 31 bytes, same len
    assert_eq!(v1.len(), v2.len());
    let payer_pubkey = mint_keypair.pubkey();
    let base_pubkey = Pubkey::new_unique();

    // Block A — create with V1.
    let ix1 = subaccount_create_instruction(program_id, base_pubkey, payer_pubkey, 0, v1, vec![]);
    let (status, _, logs) = run_subaccount_tx(&bank, &mint_keypair, ix1);
    assert!(
        status.is_ok(),
        "create failed: {status:?}\nlogs:\n{}",
        logs.join("\n")
    );

    // Advance to block N.
    let bank = bank_client
        .advance_slot(1, bank_forks.as_ref(), &Pubkey::default())
        .expect("advance slot for block N");

    // Tx1 in block N — overwrite the subaccount mid-block with V2.
    let ix2 = subaccount_instruction(program_id, base_pubkey, true, 1, v2, vec![]);
    let (status, _, logs) = run_subaccount_tx(&bank, &mint_keypair, ix2);
    assert!(
        status.is_ok(),
        "mid-block overwrite failed: {status:?}\nlogs:\n{}",
        logs.join("\n")
    );

    // Sanity: the live mid-block state is now V2.
    let storage_addr = subaccount_storage_addr_for(&base_pubkey, &program_id);
    assert_eq!(
        bank.get_account(&storage_addr)
            .expect("subaccount exists")
            .data(),
        v2,
        "mid-block overwrite should be the live state",
    );

    // Tx2 in the SAME block N — the snapshot read must still see V1 (the
    // start-of-block / parent-slot value), not the mid-block V2.
    let mut data = vec![10u8];
    data.extend_from_slice(base_pubkey.as_ref());
    data.extend_from_slice(v1);
    let ix3 = Instruction::new_with_bytes(program_id, &data, vec![]);
    let (status, _, logs) = run_subaccount_tx(&bank, &mint_keypair, ix3);
    assert!(
        status.is_ok(),
        "snapshot must read start-of-block V1, not mid-block V2: {status:?}\nlogs:\n{}",
        logs.join("\n")
    );
}

// ============================================================================
// `sol_load_subaccount_snapshot` — extended coverage (metadata, missing,
// read-only, double-load, slot reuse, two-at-once, and lane independence from a
// concurrent writable load / across blocks).
// ============================================================================

/// Build a disc=11 `load_subaccount_snapshot_verify` instruction: read the
/// start-of-block snapshot derived from `base` and assert owner / lamports /
/// data_len / data match. No account metas — proves the read is base-free.
#[cfg(feature = "sbf_rust")]
fn subaccount_snapshot_verify_instruction(
    program_id: Pubkey,
    base: &Pubkey,
    expected_owner: &Pubkey,
    expected_lamports: u64,
    expected_data: &[u8],
) -> Instruction {
    let mut data = vec![11u8];
    data.extend_from_slice(base.as_ref());
    data.extend_from_slice(expected_owner.as_ref());
    data.extend_from_slice(&expected_lamports.to_le_bytes());
    data.extend_from_slice(expected_data);
    Instruction::new_with_bytes(program_id, &data, vec![])
}

/// (1) The snapshot of a created subaccount reports the correct owner
/// (== program_id), lamports (== funding) and data_len — not just the bytes.
#[test]
#[cfg(feature = "sbf_rust")]
fn test_program_sbf_subaccount_snapshot_verifies_metadata() {
    let (bank, mut bank_client, bank_forks, mint_keypair, program_id) =
        deploy_subaccount_program(1_000_000_000);

    let v1: &[u8] = b"subaccount-snapshot-metadata-v1"; // 31 bytes
    let payer_pubkey = mint_keypair.pubkey();
    let base_pubkey = Pubkey::new_unique();

    // Block A — create the subaccount (owner=program_id, lamports=funding).
    let ix1 = subaccount_create_instruction(program_id, base_pubkey, payer_pubkey, 0, v1, vec![]);
    let (status, _, logs) = run_subaccount_tx(&bank, &mint_keypair, ix1);
    assert!(
        status.is_ok(),
        "create failed: {status:?}\nlogs:\n{}",
        logs.join("\n")
    );

    let bank = bank_client
        .advance_slot(1, bank_forks.as_ref(), &Pubkey::default())
        .expect("advance slot for snapshot read");

    let ix = subaccount_snapshot_verify_instruction(
        program_id,
        &base_pubkey,
        &program_id,
        SUBACCOUNT_FUNDING_LAMPORTS,
        v1,
    );
    let (status, _, logs) = run_subaccount_tx(&bank, &mint_keypair, ix);
    assert!(
        status.is_ok(),
        "subaccount snapshot metadata mismatch: {status:?}\nlogs:\n{}",
        logs.join("\n")
    );
}

/// (2) A snapshot of a subaccount that was never created reads as the empty
/// default (zero owner / zero lamports / empty data) rather than erroring.
#[test]
#[cfg(feature = "sbf_rust")]
fn test_program_sbf_subaccount_snapshot_missing_is_empty() {
    let (_bank, mut bank_client, bank_forks, mint_keypair, program_id) =
        deploy_subaccount_program(1_000_000_000);

    let base_pubkey = Pubkey::new_unique();

    let bank = bank_client
        .advance_slot(1, bank_forks.as_ref(), &Pubkey::default())
        .expect("advance slot for snapshot read");

    let ix =
        subaccount_snapshot_verify_instruction(program_id, &base_pubkey, &Pubkey::default(), 0, &[]);
    let (status, _, logs) = run_subaccount_tx(&bank, &mint_keypair, ix);
    assert!(
        status.is_ok(),
        "missing-subaccount snapshot must read as empty default: {status:?}\nlogs:\n{}",
        logs.join("\n")
    );
}

/// (3) The snapshot data region is read-only: a store into it must fault and
/// abort the instruction; the on-chain subaccount stays unchanged. The
/// `Custom(0xC5)` sentinel (returned only if the write was wrongly accepted)
/// must NOT be the failure reason.
#[test]
#[cfg(feature = "sbf_rust")]
fn test_program_sbf_subaccount_snapshot_read_only_cannot_modify() {
    let (bank, mut bank_client, bank_forks, mint_keypair, program_id) =
        deploy_subaccount_program(1_000_000_000);

    let v1: &[u8] = b"readonly-subaccount-snapshot-v1"; // 31 bytes
    let payer_pubkey = mint_keypair.pubkey();
    let base_pubkey = Pubkey::new_unique();

    let ix1 = subaccount_create_instruction(program_id, base_pubkey, payer_pubkey, 0, v1, vec![]);
    let (status, _, logs) = run_subaccount_tx(&bank, &mint_keypair, ix1);
    assert!(
        status.is_ok(),
        "create failed: {status:?}\nlogs:\n{}",
        logs.join("\n")
    );

    let bank = bank_client
        .advance_slot(1, bank_forks.as_ref(), &Pubkey::default())
        .expect("advance slot for snapshot read");

    // disc=12 — load snapshot then try to write into its data region.
    let mut data = vec![12u8];
    data.extend_from_slice(base_pubkey.as_ref());
    let ix = Instruction::new_with_bytes(program_id, &data, vec![]);
    let (status, _, logs) = run_subaccount_tx(&bank, &mint_keypair, ix);
    assert!(
        status.is_err(),
        "writing a read-only subaccount snapshot must fail\nlogs:\n{}",
        logs.join("\n")
    );
    assert_ne!(
        status,
        Err(TransactionError::InstructionError(
            0,
            InstructionError::Custom(0xC5)
        )),
        "the write was wrongly accepted (program reached the post-write sentinel)\nlogs:\n{}",
        logs.join("\n"),
    );

    let storage_addr = subaccount_storage_addr_for(&base_pubkey, &program_id);
    assert_eq!(
        bank.get_account(&storage_addr)
            .expect("subaccount exists")
            .data(),
        v1,
        "a snapshot read must never modify the underlying subaccount",
    );
}

/// (4) Loading the same subaccount snapshot twice in one instruction must fail
/// with `AccountAlreadyInitialized`.
#[test]
#[cfg(feature = "sbf_rust")]
fn test_program_sbf_subaccount_snapshot_same_twice_fails() {
    let (bank, mut bank_client, bank_forks, mint_keypair, program_id) =
        deploy_subaccount_program(1_000_000_000);

    let v1: &[u8] = b"double-loaded-subaccount-snap_v"; // 31 bytes
    let payer_pubkey = mint_keypair.pubkey();
    let base_pubkey = Pubkey::new_unique();

    let ix1 = subaccount_create_instruction(program_id, base_pubkey, payer_pubkey, 0, v1, vec![]);
    let (status, _, logs) = run_subaccount_tx(&bank, &mint_keypair, ix1);
    assert!(
        status.is_ok(),
        "create failed: {status:?}\nlogs:\n{}",
        logs.join("\n")
    );

    let bank = bank_client
        .advance_slot(1, bank_forks.as_ref(), &Pubkey::default())
        .expect("advance slot for snapshot read");

    // disc=13 — load twice.
    let mut data = vec![13u8];
    data.extend_from_slice(base_pubkey.as_ref());
    let ix = Instruction::new_with_bytes(program_id, &data, vec![]);
    let (status, _, logs) = run_subaccount_tx(&bank, &mint_keypair, ix);
    assert_eq!(
        status,
        Err(TransactionError::InstructionError(
            0,
            InstructionError::AccountAlreadyInitialized
        )),
        "loading the same subaccount snapshot twice must fail: {status:?}\nlogs:\n{}",
        logs.join("\n"),
    );
}

/// (5) A snapshot slot can be reused: load → unload → reload the same
/// subaccount in a single instruction must succeed.
#[test]
#[cfg(feature = "sbf_rust")]
fn test_program_sbf_subaccount_snapshot_unload_and_reload() {
    let (bank, mut bank_client, bank_forks, mint_keypair, program_id) =
        deploy_subaccount_program(1_000_000_000);

    let v1: &[u8] = b"reusable-subaccount-snap-slot_v"; // 31 bytes
    let payer_pubkey = mint_keypair.pubkey();
    let base_pubkey = Pubkey::new_unique();

    let ix1 = subaccount_create_instruction(program_id, base_pubkey, payer_pubkey, 0, v1, vec![]);
    let (status, _, logs) = run_subaccount_tx(&bank, &mint_keypair, ix1);
    assert!(
        status.is_ok(),
        "create failed: {status:?}\nlogs:\n{}",
        logs.join("\n")
    );

    let bank = bank_client
        .advance_slot(1, bank_forks.as_ref(), &Pubkey::default())
        .expect("advance slot for snapshot read");

    // disc=14 — load + verify + unload + reload + verify + unload.
    let mut data = vec![14u8];
    data.extend_from_slice(base_pubkey.as_ref());
    data.extend_from_slice(v1);
    let ix = Instruction::new_with_bytes(program_id, &data, vec![]);
    let (status, _, logs) = run_subaccount_tx(&bank, &mint_keypair, ix);
    assert!(
        status.is_ok(),
        "unload + reload of a subaccount snapshot failed: {status:?}\nlogs:\n{}",
        logs.join("\n")
    );
}

/// (6) Two distinct subaccounts can be snapshot-loaded into two slots at once
/// and each reads back its own data.
#[test]
#[cfg(feature = "sbf_rust")]
fn test_program_sbf_subaccount_snapshot_two_distinct() {
    let (bank, mut bank_client, bank_forks, mint_keypair, program_id) =
        deploy_subaccount_program(1_000_000_000);

    let payer_pubkey = mint_keypair.pubkey();
    let base1 = Pubkey::new_unique();
    let base2 = Pubkey::new_unique();
    let d1: &[u8] = b"first-subaccount-snapshot"; // 25 bytes
    let d2: &[u8] = b"second-subaccount-snapshot-data-longer"; // 38 bytes

    let ix1 = subaccount_create_instruction(program_id, base1, payer_pubkey, 0, d1, vec![]);
    let (status, _, logs) = run_subaccount_tx(&bank, &mint_keypair, ix1);
    assert!(
        status.is_ok(),
        "create #1 failed: {status:?}\nlogs:\n{}",
        logs.join("\n")
    );
    let ix2 = subaccount_create_instruction(program_id, base2, payer_pubkey, 0, d2, vec![]);
    let (status, _, logs) = run_subaccount_tx(&bank, &mint_keypair, ix2);
    assert!(
        status.is_ok(),
        "create #2 failed: {status:?}\nlogs:\n{}",
        logs.join("\n")
    );

    let bank = bank_client
        .advance_slot(1, bank_forks.as_ref(), &Pubkey::default())
        .expect("advance slot for snapshot read");

    // disc=15 — base1 ++ base2 ++ len(d1) as u16 LE ++ d1 ++ d2.
    let mut data = vec![15u8];
    data.extend_from_slice(base1.as_ref());
    data.extend_from_slice(base2.as_ref());
    data.extend_from_slice(&(d1.len() as u16).to_le_bytes());
    data.extend_from_slice(d1);
    data.extend_from_slice(d2);
    let ix = Instruction::new_with_bytes(program_id, &data, vec![]);
    let (status, _, logs) = run_subaccount_tx(&bank, &mint_keypair, ix);
    assert!(
        status.is_ok(),
        "two concurrent subaccount snapshots failed: {status:?}\nlogs:\n{}",
        logs.join("\n")
    );
}

/// (7) Within one instruction, a writable load that overwrites the subaccount
/// with V2 does not affect a snapshot of the same subaccount, which still reads
/// the start-of-block value V1. The writable overwrite is committed to V2.
#[test]
#[cfg(feature = "sbf_rust")]
fn test_program_sbf_subaccount_snapshot_independent_from_writable_load() {
    let (bank, mut bank_client, bank_forks, mint_keypair, program_id) =
        deploy_subaccount_program(1_000_000_000);

    let v1: &[u8] = b"snap-start-of-block-value-v1___"; // 31 bytes
    let v2: &[u8] = b"live-writable-overwrite-value_2"; // 31 bytes, same len
    assert_eq!(v1.len(), v2.len());
    let payer_pubkey = mint_keypair.pubkey();
    let base_pubkey = Pubkey::new_unique();

    // Block A — create V1.
    let ix1 = subaccount_create_instruction(program_id, base_pubkey, payer_pubkey, 0, v1, vec![]);
    let (status, _, logs) = run_subaccount_tx(&bank, &mint_keypair, ix1);
    assert!(
        status.is_ok(),
        "create failed: {status:?}\nlogs:\n{}",
        logs.join("\n")
    );

    // Advance to block N so V1 is the start-of-block (parent) value.
    let bank = bank_client
        .advance_slot(1, bank_forks.as_ref(), &Pubkey::default())
        .expect("advance slot for block N");

    // disc=16 — base must be writable for the writable load. Payload =
    // len(v1) as u16 LE ++ v1 (expected snapshot) ++ v2 (live overwrite).
    let mut payload = Vec::new();
    payload.extend_from_slice(&(v1.len() as u16).to_le_bytes());
    payload.extend_from_slice(v1);
    payload.extend_from_slice(v2);
    let ix = subaccount_instruction(program_id, base_pubkey, true, 16, &payload, vec![]);
    let (status, _, logs) = run_subaccount_tx(&bank, &mint_keypair, ix);
    assert!(
        status.is_ok(),
        "snapshot must stay independent of the live writable load: {status:?}\nlogs:\n{}",
        logs.join("\n")
    );

    // The writable overwrite was committed — live on-chain state is now V2.
    let storage_addr = subaccount_storage_addr_for(&base_pubkey, &program_id);
    assert_eq!(
        bank.get_account(&storage_addr)
            .expect("subaccount exists")
            .data(),
        v2,
        "the writable overwrite should have committed V2",
    );
}

/// (8) A snapshot reads the *parent slot's committed* state, not the original
/// creation value: V1 created in block A, overwritten + committed to V2 in
/// block B, snapshot-read in block C must return V2 (block B is C's parent).
#[test]
#[cfg(feature = "sbf_rust")]
fn test_program_sbf_subaccount_snapshot_tracks_parent_not_origin() {
    let (bank, mut bank_client, bank_forks, mint_keypair, program_id) =
        deploy_subaccount_program(1_000_000_000);

    let v1: &[u8] = b"origin-creation-value-block-a_1"; // 31 bytes
    let v2: &[u8] = b"committed-parent-value-block-b2"; // 31 bytes, same len
    assert_eq!(v1.len(), v2.len());
    let payer_pubkey = mint_keypair.pubkey();
    let base_pubkey = Pubkey::new_unique();

    // Block A — create V1.
    let ix1 = subaccount_create_instruction(program_id, base_pubkey, payer_pubkey, 0, v1, vec![]);
    let (status, _, logs) = run_subaccount_tx(&bank, &mint_keypair, ix1);
    assert!(
        status.is_ok(),
        "create failed: {status:?}\nlogs:\n{}",
        logs.join("\n")
    );

    // Advance to block B and commit an overwrite to V2 (load + overwrite + unload).
    let bank = bank_client
        .advance_slot(1, bank_forks.as_ref(), &Pubkey::default())
        .expect("advance slot for block B");
    let ix2 = subaccount_instruction(program_id, base_pubkey, true, 1, v2, vec![]);
    let (status, _, logs) = run_subaccount_tx(&bank, &mint_keypair, ix2);
    assert!(
        status.is_ok(),
        "block B overwrite failed: {status:?}\nlogs:\n{}",
        logs.join("\n")
    );

    // Advance to block C — the snapshot must observe V2 (parent slot B), not V1.
    let bank = bank_client
        .advance_slot(1, bank_forks.as_ref(), &Pubkey::default())
        .expect("advance slot for block C");
    let mut data = vec![10u8];
    data.extend_from_slice(base_pubkey.as_ref());
    data.extend_from_slice(v2);
    let ix3 = Instruction::new_with_bytes(program_id, &data, vec![]);
    let (status, _, logs) = run_subaccount_tx(&bank, &mint_keypair, ix3);
    assert!(
        status.is_ok(),
        "snapshot must read the parent-slot committed V2, not the origin V1: {status:?}\nlogs:\n{}",
        logs.join("\n")
    );
}

/// (9) HIGH-1 determinism core, subaccount lane: the full
/// write-in-tx-A / snapshot-in-a-later-tx-of-the-same-block / next-block
/// scenario, asserting owner + lamports + data in one test.
///
/// The snapshot syscalls read the *parent slot* via
/// `get_account_shared_data_at_block_start` →
/// `Bank::proper_ancestors()` (the current slot excluded). That read must be
/// byte-identical on the leader and on every replaying validator, so a snapshot
/// taken in block N must observe X exactly as of the block boundary regardless
/// of what earlier transactions in block N did to it.
///
/// Timeline:
///   * Block A     — create subaccount X with V1 (owner = program_id,
///                   lamports = funding). This is X's pre-block (parent-slot)
///                   state for block N.
///   * Block N tx A — a real transaction overwrites X's data to V2.
///   * Block N tx B — `load_subaccount_snapshot(X)` must report owner/lamports
///                   == X's pre-block values and data == V1 (NOT A's V2).
///   * Block N+1    — `load_subaccount_snapshot(X)` must now report data == V2,
///                   because block N (with A's write committed) is its parent.
///
/// Discriminating power: had the snapshot delegated to plain
/// `get_account_shared_data` (which includes the current slot's writes), tx B
/// would observe A's V2 and the disc=11 verifier would fail with `Custom(0xC0)`
/// (data mismatch). It passes only because the parent-slot read excludes block
/// N. The block-N+1 read then proves the parent read is not merely a frozen
/// creation-time value — it tracks the committed parent slot.
#[test]
#[cfg(feature = "sbf_rust")]
fn test_program_sbf_subaccount_snapshot_isolation_full() {
    let (bank, mut bank_client, bank_forks, mint_keypair, program_id) =
        deploy_subaccount_program(1_000_000_000);

    let v1: &[u8] = b"high1-subaccount-pre-block-val1"; // 31 bytes
    let v2: &[u8] = b"high1-subaccount-txA-overwrite2"; // 31 bytes, same len
    assert_eq!(v1.len(), v2.len());
    let payer_pubkey = mint_keypair.pubkey();
    let base_pubkey = Pubkey::new_unique();

    // Block A — create X with V1 (owner = program_id, lamports = funding).
    let ix1 = subaccount_create_instruction(program_id, base_pubkey, payer_pubkey, 0, v1, vec![]);
    let (status, _, logs) = run_subaccount_tx(&bank, &mint_keypair, ix1);
    assert!(
        status.is_ok(),
        "create failed: {status:?}\nlogs:\n{}",
        logs.join("\n")
    );

    // Advance to block N so the V1 write lives in the parent slot.
    let bank = bank_client
        .advance_slot(1, bank_forks.as_ref(), &Pubkey::default())
        .expect("advance slot for block N");

    // Block N, tx A — a real transaction overwrites X's data to V2.
    let ix_tx_a = subaccount_instruction(program_id, base_pubkey, true, 1, v2, vec![]);
    let (status, _, logs) = run_subaccount_tx(&bank, &mint_keypair, ix_tx_a);
    assert!(
        status.is_ok(),
        "tx A overwrite failed: {status:?}\nlogs:\n{}",
        logs.join("\n")
    );

    // Sanity: the live mid-block state is now V2.
    let storage_addr = subaccount_storage_addr_for(&base_pubkey, &program_id);
    assert_eq!(
        bank.get_account(&storage_addr)
            .expect("subaccount exists")
            .data(),
        v2,
        "tx A's overwrite should be the live state",
    );

    // Block N, tx B — the snapshot must report X's pre-block owner / lamports /
    // data (V1), NOT A's mid-block V2. Fails with Custom(0xC0) if the read
    // leaked A's write (i.e. if it used get_account_shared_data).
    let ix_tx_b = subaccount_snapshot_verify_instruction(
        program_id,
        &base_pubkey,
        &program_id,
        SUBACCOUNT_FUNDING_LAMPORTS,
        v1,
    );
    let (status, _, logs) = run_subaccount_tx(&bank, &mint_keypair, ix_tx_b);
    assert!(
        status.is_ok(),
        "same-block snapshot must read pre-block V1, not tx A's V2: {status:?}\nlogs:\n{}",
        logs.join("\n")
    );

    // Advance to block N+1 — block N (with A's write) is now the parent, so the
    // snapshot must observe A's committed V2.
    let bank = bank_client
        .advance_slot(1, bank_forks.as_ref(), &Pubkey::default())
        .expect("advance slot for block N+1");
    let ix_tx_c = subaccount_snapshot_verify_instruction(
        program_id,
        &base_pubkey,
        &program_id,
        SUBACCOUNT_FUNDING_LAMPORTS,
        v2,
    );
    let (status, _, logs) = run_subaccount_tx(&bank, &mint_keypair, ix_tx_c);
    assert!(
        status.is_ok(),
        "next-block snapshot must read tx A's committed V2: {status:?}\nlogs:\n{}",
        logs.join("\n")
    );
}

// ============================================================================
// `sol_load_account_snapshot` — read-only, base-free, start-of-block load of an
// arbitrary account *by pubkey* (not subaccount seeds). Mirrors the subaccount
// snapshot tests above but exercises the `SnapshotKey::Account` lane.
// ============================================================================

/// Build an `AccountSharedData` with explicit lamports / owner / data.
#[cfg(feature = "sbf_rust")]
fn make_account(lamports: u64, owner: &Pubkey, data: &[u8]) -> AccountSharedData {
    let mut account = AccountSharedData::new(lamports, data.len(), owner);
    account.set_data_from_slice(data);
    account
}

/// Build a disc=11 `load_account_snapshot_verify` instruction: read the
/// start-of-block snapshot of `account_pk` and assert owner / lamports /
/// data_len / data match. No account metas — proves the read is base-free.
#[cfg(feature = "sbf_rust")]
fn account_snapshot_verify_instruction(
    program_id: Pubkey,
    account_pk: &Pubkey,
    expected_owner: &Pubkey,
    expected_lamports: u64,
    expected_data: &[u8],
) -> Instruction {
    let mut data = vec![17u8];
    data.extend_from_slice(account_pk.as_ref());
    data.extend_from_slice(expected_owner.as_ref());
    data.extend_from_slice(&expected_lamports.to_le_bytes());
    data.extend_from_slice(expected_data);
    Instruction::new_with_bytes(program_id, &data, vec![])
}

/// Happy path: an account written in one block is snapshot-read by pubkey in the
/// next block with NO account in the transaction. The snapshot's owner,
/// lamports, data_len and data must all match the start-of-block state.
#[test]
#[cfg(feature = "sbf_rust")]
fn test_program_sbf_account_snapshot_reads_block_start_without_account() {
    let (bank, mut bank_client, bank_forks, mint_keypair, program_id) =
        deploy_subaccount_program(1_000_000_000);

    let account_pk = Pubkey::new_unique();
    let owner = Pubkey::new_unique();
    let lamports = 1_234_567u64;
    let v1: &[u8] = b"account-snapshot-block-start-v1"; // 31 bytes

    // Block A — store the account.
    bank.store_account(&account_pk, &make_account(lamports, &owner, v1));

    // Advance so the write lives in the parent slot.
    let bank = bank_client
        .advance_slot(1, bank_forks.as_ref(), &Pubkey::default())
        .expect("advance slot for snapshot read");

    let ix = account_snapshot_verify_instruction(program_id, &account_pk, &owner, lamports, v1);
    let (status, _, logs) = run_subaccount_tx(&bank, &mint_keypair, ix);
    assert!(
        status.is_ok(),
        "base-free account snapshot read failed: {status:?}\nlogs:\n{}",
        logs.join("\n")
    );
}

/// A snapshot read sees the start-of-block state even when an earlier write in
/// the same block has overwritten the account: the snapshot observes the
/// parent-slot value, not the live mid-block one.
#[test]
#[cfg(feature = "sbf_rust")]
fn test_program_sbf_account_snapshot_ignores_midblock_write() {
    let (bank, mut bank_client, bank_forks, mint_keypair, program_id) =
        deploy_subaccount_program(1_000_000_000);

    let account_pk = Pubkey::new_unique();
    let owner = Pubkey::new_unique();
    let lamports = 7_654_321u64;
    let v1: &[u8] = b"acct-snapshot-block-start-val-1"; // 31 bytes
    let v2: &[u8] = b"acct-midblock-overwrite-val-2__"; // 31 bytes, same len
    assert_eq!(v1.len(), v2.len());

    // Block A — store V1.
    bank.store_account(&account_pk, &make_account(lamports, &owner, v1));

    // Advance to block N.
    let bank = bank_client
        .advance_slot(1, bank_forks.as_ref(), &Pubkey::default())
        .expect("advance slot for block N");

    // Mid-block write into the live slot N — overwrite with V2.
    bank.store_account(&account_pk, &make_account(lamports, &owner, v2));
    assert_eq!(
        bank.get_account(&account_pk).expect("account exists").data(),
        v2,
        "mid-block overwrite should be the live state",
    );

    // The snapshot read in the same block N must still see V1.
    let ix = account_snapshot_verify_instruction(program_id, &account_pk, &owner, lamports, v1);
    let (status, _, logs) = run_subaccount_tx(&bank, &mint_keypair, ix);
    assert!(
        status.is_ok(),
        "snapshot must read start-of-block V1, not mid-block V2: {status:?}\nlogs:\n{}",
        logs.join("\n")
    );
}

/// HIGH-1 determinism core, account lane: the full
/// write-in-tx-A / snapshot-in-a-later-tx-of-the-same-block / next-block
/// scenario, asserting owner + lamports + data in one test. Mirrors
/// `test_program_sbf_subaccount_snapshot_isolation_full` but drives the
/// `SnapshotKey::Account` lane, and uses a genuine `system_program::Transfer`
/// transaction (not a direct `store_account`) as the in-block writer tx A.
///
/// Timeline:
///   * Block A      — store X (system-owned, lamports = L1). X's pre-block state.
///   * Block N tx A — a real system Transfer credits X to L1 + DELTA = L2.
///   * Block N tx B — `load_account_snapshot(X)` must report lamports == L1 and
///                    owner/data == X's pre-block values (NOT L2).
///   * Block N+1    — `load_account_snapshot(X)` must now report lamports == L2.
///
/// Discriminating power: a plain `get_account_shared_data` read (current slot
/// included) would see tx A's L2 in block N and the disc=17 verifier would fail
/// with `Custom(0xB3)` (lamports mismatch). It passes only because the
/// parent-slot read excludes block N; the block-N+1 read then proves the read
/// tracks the committed parent slot rather than a frozen value.
#[test]
#[cfg(feature = "sbf_rust")]
fn test_program_sbf_account_snapshot_isolation_full() {
    use solana_system_interface::instruction as system_instruction;

    let (bank, mut bank_client, bank_forks, mint_keypair, program_id) =
        deploy_subaccount_program(1_000_000_000);

    let account_pk = Pubkey::new_unique();
    let owner = system_program::id(); // system-owned so a Transfer can credit it
    let l1: u64 = 5_000_000; // pre-block balance (rent-exempt for 0-byte data)
    let delta: u64 = 1_500_000;
    let l2: u64 = l1 + delta;

    // Block A — store X (system-owned, empty data, lamports = L1).
    bank.store_account(&account_pk, &make_account(l1, &owner, &[]));

    // Advance to block N so the L1 write lives in the parent slot.
    let bank = bank_client
        .advance_slot(1, bank_forks.as_ref(), &Pubkey::default())
        .expect("advance slot for block N");

    // Block N, tx A — a real Transfer credits X from the mint, raising it to L2.
    let transfer_ix = system_instruction::transfer(&mint_keypair.pubkey(), &account_pk, delta);
    let (status, _, logs) = run_subaccount_tx(&bank, &mint_keypair, transfer_ix);
    assert!(
        status.is_ok(),
        "tx A transfer failed: {status:?}\nlogs:\n{}",
        logs.join("\n")
    );

    // Sanity: the live mid-block balance is now L2.
    assert_eq!(
        bank.get_account(&account_pk).expect("account exists").lamports(),
        l2,
        "tx A's transfer should be the live state",
    );

    // Block N, tx B — the snapshot must report X's pre-block lamports (L1),
    // owner and (empty) data, NOT tx A's mid-block L2. Fails with Custom(0xB3)
    // if the read leaked A's write.
    let ix_tx_b = account_snapshot_verify_instruction(program_id, &account_pk, &owner, l1, &[]);
    let (status, _, logs) = run_subaccount_tx(&bank, &mint_keypair, ix_tx_b);
    assert!(
        status.is_ok(),
        "same-block snapshot must read pre-block L1, not tx A's L2: {status:?}\nlogs:\n{}",
        logs.join("\n")
    );

    // Advance to block N+1 — block N (with A's transfer committed) is now the
    // parent, so the snapshot must observe L2.
    let bank = bank_client
        .advance_slot(1, bank_forks.as_ref(), &Pubkey::default())
        .expect("advance slot for block N+1");
    let ix_tx_c = account_snapshot_verify_instruction(program_id, &account_pk, &owner, l2, &[]);
    let (status, _, logs) = run_subaccount_tx(&bank, &mint_keypair, ix_tx_c);
    assert!(
        status.is_ok(),
        "next-block snapshot must read tx A's committed L2: {status:?}\nlogs:\n{}",
        logs.join("\n")
    );
}

/// Snapshot-reading a pubkey that was never created returns the default empty
/// state (zero owner / zero lamports / empty data) rather than erroring.
#[test]
#[cfg(feature = "sbf_rust")]
fn test_program_sbf_account_snapshot_missing_account_is_empty() {
    let (_bank, mut bank_client, bank_forks, mint_keypair, program_id) =
        deploy_subaccount_program(1_000_000_000);

    let account_pk = Pubkey::new_unique();

    // Advance a slot so the read has a parent slot to consult; the account is
    // absent in every ancestor.
    let bank = bank_client
        .advance_slot(1, bank_forks.as_ref(), &Pubkey::default())
        .expect("advance slot for snapshot read");

    let ix =
        account_snapshot_verify_instruction(program_id, &account_pk, &Pubkey::default(), 0, &[]);
    let (status, _, logs) = run_subaccount_tx(&bank, &mint_keypair, ix);
    assert!(
        status.is_ok(),
        "missing-account snapshot must read as empty default: {status:?}\nlogs:\n{}",
        logs.join("\n")
    );
}

/// The snapshot data region is read-only: a store into it must fault and abort
/// the instruction. The on-chain account must be unchanged afterwards (a
/// snapshot is never persisted). The `Custom(0xB5)` sentinel — returned only if
/// the write was wrongly accepted — must NOT be the failure reason.
#[test]
#[cfg(feature = "sbf_rust")]
fn test_program_sbf_account_snapshot_read_only_cannot_modify() {
    let (bank, mut bank_client, bank_forks, mint_keypair, program_id) =
        deploy_subaccount_program(1_000_000_000);

    let account_pk = Pubkey::new_unique();
    let owner = Pubkey::new_unique();
    let v1: &[u8] = b"readonly-account-snapshot-data_"; // 31 bytes

    bank.store_account(&account_pk, &make_account(42, &owner, v1));
    let bank = bank_client
        .advance_slot(1, bank_forks.as_ref(), &Pubkey::default())
        .expect("advance slot for snapshot read");

    // disc=18 — load snapshot then try to write into its data region.
    let mut data = vec![18u8];
    data.extend_from_slice(account_pk.as_ref());
    let ix = Instruction::new_with_bytes(program_id, &data, vec![]);
    let (status, _, logs) = run_subaccount_tx(&bank, &mint_keypair, ix);
    assert!(
        status.is_err(),
        "writing a read-only account snapshot must fail\nlogs:\n{}",
        logs.join("\n")
    );
    assert_ne!(
        status,
        Err(TransactionError::InstructionError(
            0,
            InstructionError::Custom(0xB5)
        )),
        "the write was wrongly accepted (program reached the post-write sentinel)\nlogs:\n{}",
        logs.join("\n"),
    );

    // The original account is untouched.
    assert_eq!(
        bank.get_account(&account_pk).expect("account exists").data(),
        v1,
        "a snapshot read must never modify the underlying account",
    );
}

/// Loading the same account snapshot twice in one instruction must fail with
/// `AccountAlreadyInitialized`.
#[test]
#[cfg(feature = "sbf_rust")]
fn test_program_sbf_account_snapshot_same_account_twice_fails() {
    let (bank, mut bank_client, bank_forks, mint_keypair, program_id) =
        deploy_subaccount_program(1_000_000_000);

    let account_pk = Pubkey::new_unique();
    let owner = Pubkey::new_unique();
    let v1: &[u8] = b"double-loaded-account-snapshot_"; // 31 bytes

    bank.store_account(&account_pk, &make_account(42, &owner, v1));
    let bank = bank_client
        .advance_slot(1, bank_forks.as_ref(), &Pubkey::default())
        .expect("advance slot for snapshot read");

    // disc=19 — load twice.
    let mut data = vec![19u8];
    data.extend_from_slice(account_pk.as_ref());
    let ix = Instruction::new_with_bytes(program_id, &data, vec![]);
    let (status, _, logs) = run_subaccount_tx(&bank, &mint_keypair, ix);
    assert_eq!(
        status,
        Err(TransactionError::InstructionError(
            0,
            InstructionError::AccountAlreadyInitialized
        )),
        "loading the same account snapshot twice must fail: {status:?}\nlogs:\n{}",
        logs.join("\n"),
    );
}

/// A snapshot slot can be reused: load → unload → reload the same account in a
/// single instruction must succeed.
#[test]
#[cfg(feature = "sbf_rust")]
fn test_program_sbf_account_snapshot_unload_and_reload() {
    let (bank, mut bank_client, bank_forks, mint_keypair, program_id) =
        deploy_subaccount_program(1_000_000_000);

    let account_pk = Pubkey::new_unique();
    let owner = Pubkey::new_unique();
    let v1: &[u8] = b"reusable-account-snapshot-slot_"; // 31 bytes

    bank.store_account(&account_pk, &make_account(42, &owner, v1));
    let bank = bank_client
        .advance_slot(1, bank_forks.as_ref(), &Pubkey::default())
        .expect("advance slot for snapshot read");

    // disc=20 — load + verify + unload + reload + verify + unload.
    let mut data = vec![20u8];
    data.extend_from_slice(account_pk.as_ref());
    data.extend_from_slice(v1);
    let ix = Instruction::new_with_bytes(program_id, &data, vec![]);
    let (status, _, logs) = run_subaccount_tx(&bank, &mint_keypair, ix);
    assert!(
        status.is_ok(),
        "unload + reload of an account snapshot failed: {status:?}\nlogs:\n{}",
        logs.join("\n")
    );
}

/// Two distinct accounts can be snapshot-loaded into two slots at once and each
/// reads back its own data.
#[test]
#[cfg(feature = "sbf_rust")]
fn test_program_sbf_account_snapshot_two_distinct_accounts() {
    let (bank, mut bank_client, bank_forks, mint_keypair, program_id) =
        deploy_subaccount_program(1_000_000_000);

    let pk1 = Pubkey::new_unique();
    let pk2 = Pubkey::new_unique();
    let owner = Pubkey::new_unique();
    let d1: &[u8] = b"first-account-snapshot-data"; // 27 bytes
    let d2: &[u8] = b"second-account-snapshot-data-longer-payload"; // 43 bytes

    bank.store_account(&pk1, &make_account(11, &owner, d1));
    bank.store_account(&pk2, &make_account(22, &owner, d2));
    let bank = bank_client
        .advance_slot(1, bank_forks.as_ref(), &Pubkey::default())
        .expect("advance slot for snapshot read");

    // disc=21 — pk1 ++ pk2 ++ len(d1) as u16 LE ++ d1 ++ d2.
    let mut data = vec![21u8];
    data.extend_from_slice(pk1.as_ref());
    data.extend_from_slice(pk2.as_ref());
    data.extend_from_slice(&(d1.len() as u16).to_le_bytes());
    data.extend_from_slice(d1);
    data.extend_from_slice(d2);
    let ix = Instruction::new_with_bytes(program_id, &data, vec![]);
    let (status, _, logs) = run_subaccount_tx(&bank, &mint_keypair, ix);
    assert!(
        status.is_ok(),
        "two concurrent account snapshots failed: {status:?}\nlogs:\n{}",
        logs.join("\n")
    );
}

/// `SnapshotKey::Account(pk)` and `SnapshotKey::Subaccount(pk)` never collide,
/// even when the account snapshot is pointed at the subaccount's *derived*
/// address: each resolves to an independent lane entry with its own data.
#[test]
#[cfg(feature = "sbf_rust")]
fn test_program_sbf_account_snapshot_vs_subaccount_snapshot_distinct_keys() {
    use solana_transaction_context::create_subaccount_address;

    let (bank, mut bank_client, bank_forks, mint_keypair, program_id) =
        deploy_subaccount_program(1_000_000_000);

    let payer_pubkey = mint_keypair.pubkey();
    let base_pubkey = Pubkey::new_unique();
    let subaccount_data: &[u8] = b"subaccount-lane-distinct-value_"; // 31 bytes
    let account_data: &[u8] = b"account-lane-distinct-value-different-bytes"; // 43 bytes

    // The subaccount's owner-facing derived address.
    let derived = create_subaccount_address(&[base_pubkey.as_ref(), SUBACCOUNT_SEED_TAG], &program_id)
        .expect("derive subaccount address");

    // Block A — create the subaccount (writes `subaccount_data` to the
    // subaccount storage), and independently store a regular account *at the
    // derived address itself* with different bytes.
    let ix_create = subaccount_create_instruction(
        program_id,
        base_pubkey,
        payer_pubkey,
        0,
        subaccount_data,
        vec![],
    );
    let (status, _, logs) = run_subaccount_tx(&bank, &mint_keypair, ix_create);
    assert!(
        status.is_ok(),
        "subaccount create failed: {status:?}\nlogs:\n{}",
        logs.join("\n")
    );
    bank.store_account(
        &derived,
        &make_account(99, &Pubkey::new_unique(), account_data),
    );

    let bank = bank_client
        .advance_slot(1, bank_forks.as_ref(), &Pubkey::default())
        .expect("advance slot for snapshot read");

    // disc=22 — base ++ derived ++ len(account_data) as u16 LE ++ account_data
    //           ++ subaccount_data. Reads both snapshots and checks each lane.
    let mut data = vec![22u8];
    data.extend_from_slice(base_pubkey.as_ref());
    data.extend_from_slice(derived.as_ref());
    data.extend_from_slice(&(account_data.len() as u16).to_le_bytes());
    data.extend_from_slice(account_data);
    data.extend_from_slice(subaccount_data);
    let ix = Instruction::new_with_bytes(program_id, &data, vec![]);
    let (status, _, logs) = run_subaccount_tx(&bank, &mint_keypair, ix);
    assert!(
        status.is_ok(),
        "account/subaccount snapshot keys must not collide: {status:?}\nlogs:\n{}",
        logs.join("\n")
    );
}

#[test]
#[cfg(feature = "sbf_rust")]
fn test_program_sbf_subaccount_cant_modify_readonly_data() {
    let (bank, mut bank_client, bank_forks, mint_keypair, program_id) =
        deploy_subaccount_program(1_000_000_000);

    let payload_v1: &[u8] = b"persist-marker-v1______________"; // 31 bytes
    let payload_v2: &[u8] = b"###tx2-overwrite-payload###____"; // 31 bytes, same len
    assert_eq!(payload_v1.len(), payload_v2.len());

    let payer_pubkey = mint_keypair.pubkey();
    let base_pubkey = Pubkey::new_unique();

    // Tx1 — create + write v1.
    let ix1 =
        subaccount_create_instruction(program_id, base_pubkey, payer_pubkey, 0, payload_v1, vec![]);
    let (status, _, logs) = run_subaccount_tx(&bank, &mint_keypair, ix1);
    assert!(
        status.is_ok(),
        "Tx1 failed: {status:?}\nlogs:\n{}",
        logs.join("\n")
    );

    // Advance into a new bank slot so Tx2 runs on a fresh frame.
    let bank = bank_client
        .advance_slot(1, bank_forks.as_ref(), &Pubkey::default())
        .expect("advance slot for Tx2");

    // Tx2 — load existing + overwrite with v2.
    let ix2 = subaccount_instruction(program_id, base_pubkey, false, 1, payload_v2, vec![]);
    let (status, _, logs) = run_subaccount_tx(&bank, &mint_keypair, ix2);
    assert_eq!(
        status,
        Err(TransactionError::InstructionError(
            0,
            InstructionError::ReadonlyDataModified
        )),
        "Tx2 status mismatch: {status:?}\nlogs:\n{}",
        logs.join("\n"),
    );

    let storage_addr = subaccount_storage_addr_for(&base_pubkey, &program_id);
    let stored = bank
        .get_account(&storage_addr)
        .expect("subaccount must still exist after Tx2");
    assert_eq!(
        stored.data(),
        payload_v1,
        "Tx2's overwrite must not be on-chain (proves readonly data was not modified)",
    );
    // Lamports are unchanged from Tx1 — Tx2 did no funding.
    assert_eq!(stored.lamports(), SUBACCOUNT_FUNDING_LAMPORTS);
    assert_eq!(stored.owner(), &program_id);
}

#[test]
#[cfg(feature = "sbf_rust")]
fn test_program_sbf_subaccount_cant_modify_out_of_bounds() {
    let (bank, mut bank_client, bank_forks, mint_keypair, program_id) =
        deploy_subaccount_program(1_000_000_000);

    let payload_v1: &[u8] = b"persist-marker-v1______________"; // 31 bytes
    let payload_v2: &[u8] = b"###tx2-overwrite-payload###____"; // 31 bytes, same len
    assert_eq!(payload_v1.len(), payload_v2.len());

    let payer_pubkey = mint_keypair.pubkey();
    let base_pubkey = Pubkey::new_unique();

    // Tx1 — create + write v1.
    let ix1 =
        subaccount_create_instruction(program_id, base_pubkey, payer_pubkey, 0, payload_v1, vec![]);
    let (status, _, logs) = run_subaccount_tx(&bank, &mint_keypair, ix1);
    assert!(
        status.is_ok(),
        "Tx1 failed: {status:?}\nlogs:\n{}",
        logs.join("\n")
    );

    // Advance into a new bank slot so Tx2 runs on a fresh frame.
    let bank = bank_client
        .advance_slot(1, bank_forks.as_ref(), &Pubkey::default())
        .expect("advance slot for Tx2");

    // Tx2 — load existing + increment counter at offset
    let offset_bytes = ((payload_v1.len() - 7) as u64).to_le_bytes(); // offset past the end of the subaccount data
    let ix2 = subaccount_instruction(program_id, base_pubkey, true, 5, &offset_bytes, vec![]);
    let (status, _, logs) = run_subaccount_tx(&bank, &mint_keypair, ix2);
    // "Access violation in input section at address"
    assert_eq!(
        status,
        Err(TransactionError::InstructionError(
            0,
            InstructionError::InvalidRealloc
        )),
        "Tx2 status mismatch: {status:?}\nlogs:\n{}",
        logs.join("\n"),
    );

    let storage_addr = subaccount_storage_addr_for(&base_pubkey, &program_id);
    let stored = bank
        .get_account(&storage_addr)
        .expect("subaccount must still exist after Tx2");
    assert_eq!(
        stored.data(),
        payload_v1,
        "Tx2's overwrite must not be on-chain (proves readonly data was not modified)",
    );
    // Lamports are unchanged from Tx1 — Tx2 did no funding.
    assert_eq!(stored.lamports(), SUBACCOUNT_FUNDING_LAMPORTS);
    assert_eq!(stored.owner(), &program_id);
}

/// Scenario 3a: a second `sol_create_subaccount` with the same seeds in a
/// single transaction must fail (`AccountAlreadyInitialized`), and the
/// original subaccount state must not be corrupted.
#[test]
#[cfg(feature = "sbf_rust")]
fn test_program_sbf_subaccount_create_twice_fails() {
    let (bank, _bank_client, _bank_forks, mint_keypair, program_id) =
        deploy_subaccount_program(1_000_000_000);

    let payload: &[u8] = b"double-create-payload";
    let payer_pubkey = mint_keypair.pubkey();
    let base_pubkey = Pubkey::new_unique();

    let ix =
        subaccount_create_instruction(program_id, base_pubkey, payer_pubkey, 2, payload, vec![]);
    let (status, _, logs) = run_subaccount_tx(&bank, &mint_keypair, ix);
    assert_eq!(
        status,
        Err(TransactionError::InstructionError(
            0,
            InstructionError::AccountAlreadyInitialized
        )),
        "expected tx to fail, but it succeeded\nlogs:\n{}",
        logs.join("\n"),
    );

    // First create did partially run (the second create is the one that
    // fails); the host tx is aborted, so the subaccount must NOT be
    // persisted to accounts-db.
    let storage_addr = subaccount_storage_addr_for(&base_pubkey, &program_id);
    assert!(
        bank.get_account(&storage_addr).is_none(),
        "failed tx must not persist any subaccount state",
    );
}

#[test]
#[cfg(feature = "sbf_rust")]
fn test_program_sbf_subaccount_load_twice_fails() {
    let (bank, _bank_client, _bank_forks, mint_keypair, program_id) =
        deploy_subaccount_program(1_000_000_000);

    let payload: &[u8] = b"double-load-payload";
    let payer_pubkey = mint_keypair.pubkey();
    let base_pubkey = Pubkey::new_unique();

    let ix =
        subaccount_create_instruction(program_id, base_pubkey, payer_pubkey, 8, payload, vec![]);
    let (status, _, logs) = run_subaccount_tx(&bank, &mint_keypair, ix);
    assert_eq!(
        status,
        Err(TransactionError::InstructionError(
            0,
            InstructionError::AccountAlreadyInitialized
        )),
        "expected tx to fail, but it succeeded\nlogs:\n{}",
        logs.join("\n"),
    );

    // First create did partially run (the second create is the one that
    // fails); the host tx is aborted, so the subaccount must NOT be
    // persisted to accounts-db.
    let storage_addr = subaccount_storage_addr_for(&base_pubkey, &program_id);
    assert!(
        bank.get_account(&storage_addr).is_none(),
        "failed tx must not persist any subaccount state",
    );
}

/// Scenario 3b: a second `sol_unload_subaccount` on an already-freed slot
/// must fail (`InvalidArgument`: slot at vm_header_addr is not loaded).
#[test]
#[cfg(feature = "sbf_rust")]
fn test_program_sbf_subaccount_unload_twice_fails() {
    let (bank, _bank_client, _bank_forks, mint_keypair, program_id) =
        deploy_subaccount_program(1_000_000_000);

    let payload: &[u8] = b"double-unload-payload";
    let payer_pubkey = mint_keypair.pubkey();
    let base_pubkey = Pubkey::new_unique();

    let ix =
        subaccount_create_instruction(program_id, base_pubkey, payer_pubkey, 3, payload, vec![]);
    let (status, _, logs) = run_subaccount_tx(&bank, &mint_keypair, ix);
    assert_eq!(
        status,
        Err(TransactionError::InstructionError(
            0,
            InstructionError::InvalidArgument
        )),
        "expected tx to fail on second unload, but it succeeded\nlogs:\n{}",
        logs.join("\n"),
    );
}

/// Scenario 3b: try `sol_unload_subaccount` on an oversized data
/// must fail (`InvalidRealloc`).
#[test]
#[cfg(feature = "sbf_rust")]
fn test_program_sbf_subaccount_unload_oversized_data_fails() {
    let (bank, _bank_client, _bank_forks, mint_keypair, program_id) =
        deploy_subaccount_program(1_000_000_000);

    let payload: &[u8] = b"oversized-data-payload";
    let payer_pubkey = mint_keypair.pubkey();
    let base_pubkey = Pubkey::new_unique();

    let ix =
        subaccount_create_instruction(program_id, base_pubkey, payer_pubkey, 6, payload, vec![]);
    let (status, _, logs) = run_subaccount_tx(&bank, &mint_keypair, ix);
    assert_eq!(
        status,
        Err(TransactionError::InstructionError(
            0,
            InstructionError::InvalidRealloc
        )),
        "expected tx to fail on unload, but it succeeded\nlogs:\n{}",
        logs.join("\n"),
    );

    // The host tx is aborted, so the subaccount must NOT be persisted to accounts-db.
    let storage_addr = subaccount_storage_addr_for(&base_pubkey, &program_id);
    let account = bank.get_account(&storage_addr);
    assert!(
        account.is_none(),
        "failed tx must not persist any subaccount state: {account:?}",
    );
}

/// Scenario 3c: payer has fewer lamports than the requested funding amount.
/// The embedded system_program::Transfer CPI inside `sol_create_subaccount`
/// must fail, the outer tx aborts, and no subaccount is persisted.
#[test]
#[cfg(feature = "sbf_rust")]
fn test_program_sbf_subaccount_insufficient_payer() {
    // Mint balance < SUBACCOUNT_FUNDING_LAMPORTS (2_000_000). The program is
    // deployed with this mint as payer, so the deploy needs to succeed first;
    // we choose a value that's enough for the loader-v4 deploy but less than
    // the funding amount the test instruction will attempt to transfer.
    let (bank, _bank_client, _bank_forks, mint_keypair, program_id) =
        deploy_subaccount_program(1_000_000_000);

    // Drain the mint down so its remaining balance can't cover FUNDING_LAMPORTS.
    let payer_pubkey = mint_keypair.pubkey();
    let base_pubkey = Pubkey::new_unique();
    let drain_target_keypair = Keypair::new();
    {
        use solana_system_interface::instruction as system_instruction;
        let remaining = bank.get_balance(&payer_pubkey);
        let keep = SUBACCOUNT_FUNDING_LAMPORTS / 2; // strictly less than funding
        let to_drain = remaining.saturating_sub(keep);
        let transfer_ix =
            system_instruction::transfer(&payer_pubkey, &drain_target_keypair.pubkey(), to_drain);
        let blockhash = bank.last_blockhash();
        let tx = Transaction::new_signed_with_payer(
            &[transfer_ix],
            Some(&payer_pubkey),
            &[&mint_keypair],
            blockhash,
        );
        let (status, _, logs, _) = process_transaction_and_record_inner(&bank, tx);
        assert!(
            status.is_ok(),
            "drain tx failed: {status:?}\nlogs:\n{}",
            logs.join("\n")
        );
    }

    let payload: &[u8] = b"insufficient-payer";
    let ix =
        subaccount_create_instruction(program_id, base_pubkey, payer_pubkey, 0, payload, vec![]);
    let (status, _, logs) = run_subaccount_tx(&bank, &mint_keypair, ix);
    assert_eq!(
        status,
        Err(TransactionError::InstructionError(
            0,
            InstructionError::Custom(1)
        )), // the custom error code for insufficient funds in the system program
        "expected tx to fail on insufficient payer, but it succeeded\nlogs:\n{}",
        logs.join("\n"),
    );

    let storage_addr = subaccount_storage_addr_for(&payer_pubkey, &program_id);
    assert!(
        bank.get_account(&storage_addr).is_none(),
        "failed tx must not persist any subaccount state",
    );
}

/// Scenario 4: a callee invoked via CPI loads the caller's subaccount and
/// mutates it; on-chain state after the tx reflects the callee's write.
///
/// The "callee" is the same program reentered via self-CPI with a
/// different discriminator (BPF→BPF CPI can't expose a subaccount through
/// `AccountMeta`s — `prepare_next_instruction` resolves pubkeys against
/// the main account lane only — so the callee must derive the subaccount
/// pubkey itself via `sol_load_subaccount`; using the same program means
/// `program_id` is identical and the PDA collapses to the same persisted
/// entry).
///
/// Flow (handled in [programs/sbf/rust/subaccount/src/lib.rs] disc 4 / 5):
///   1. Outer (disc=4): create subaccount sized for one u64; load +
///      write `initial` + unload; self-CPI to disc=5 with payer as the
///      sole `AccountMeta`.
///   2. Inner (disc=5): load same subaccount (same seeds + program_id ⇒
///      same `subaccount_storage_address`); read u64; write u64+1; unload.
///   3. On-chain data after the tx must equal `initial + 1`.
#[test]
#[cfg(feature = "sbf_rust")]
fn test_program_sbf_subaccount_cpi_increment() {
    let (bank, _bank_client, _bank_forks, mint_keypair, program_id) =
        deploy_subaccount_program(1_000_000_000);

    let initial_value: u64 = 41;
    let payload = initial_value.to_le_bytes();
    let payer_pubkey = mint_keypair.pubkey();
    let base_pubkey = Pubkey::new_unique();

    // For self-CPI the callee program account must be present in the outer
    // instruction's accounts (see invoke_context.rs:485-498 — the runtime
    // requires `find_index_of_account(callee_program_id)` to resolve AND
    // the callee to be listed in the caller's `instruction_accounts`).
    let extra_metas = vec![AccountMeta::new_readonly(program_id, false)];
    let ix = subaccount_create_instruction(
        program_id,
        base_pubkey,
        payer_pubkey,
        4,
        &payload,
        extra_metas,
    );
    let (status, _, logs) = run_subaccount_tx(&bank, &mint_keypair, ix);
    assert!(
        status.is_ok(),
        "tx failed: {status:?}\nlogs:\n{}",
        logs.join("\n"),
    );

    let storage_addr = subaccount_storage_addr_for(&base_pubkey, &program_id);
    let stored = bank
        .get_account(&storage_addr)
        .expect("subaccount must exist");
    let on_chain = u64::from_le_bytes(
        stored
            .data()
            .try_into()
            .expect("subaccount data must be exactly 8 bytes"),
    );
    assert_eq!(
        on_chain,
        initial_value + 1,
        "inner CPI must have incremented the counter (initial={initial_value})",
    );
    assert_eq!(stored.owner(), &program_id);
    assert_eq!(stored.lamports(), SUBACCOUNT_FUNDING_LAMPORTS);
}

// ----------------------------------------------------------------------------
// `sol_read_subaccount` tests (disc=9). The program creates a subaccount with
// `content`, then reads a `[offset, offset+length)` window back and verifies
// the bytes. On success the tx is Ok; an out-of-range or missing-subaccount
// read aborts the instruction with `InstructionError::InvalidArgument`.
// ----------------------------------------------------------------------------

/// Builds the disc=9 payload: `do_create` flag, `offset`/`length` (u64 LE),
/// followed by the subaccount `content`.
#[cfg(feature = "sbf_rust")]
fn subaccount_read_payload(
    do_create: bool,
    do_load: bool,
    offset: u64,
    length: u64,
    content: &[u8],
) -> Vec<u8> {
    let mut payload = Vec::with_capacity(18 + content.len());
    payload.push(do_create as u8);
    payload.push(do_load as u8);
    payload.extend_from_slice(&offset.to_le_bytes());
    payload.extend_from_slice(&length.to_le_bytes());
    payload.extend_from_slice(content);
    payload
}

/// Happy path: create a subaccount with `content`, then read the full range
/// back through `sol_read_subaccount`. The program asserts the bytes match,
/// so an Ok status proves the syscall returned the exact stored data.
#[test]
#[cfg(feature = "sbf_rust")]
fn test_program_sbf_subaccount_read_success() {
    let (bank, _bank_client, _bank_forks, mint_keypair, program_id) =
        deploy_subaccount_program(1_000_000_000);

    let payer_pubkey = mint_keypair.pubkey();
    let base_pubkey = Pubkey::new_unique();
    let content: &[u8] = b"read-subaccount-content-32-bytes";
    assert_eq!(content.len(), 32);

    let payload = subaccount_read_payload(true, false, 0, content.len() as u64, content);
    let ix =
        subaccount_create_instruction(program_id, base_pubkey, payer_pubkey, 9, &payload, vec![]);
    let (status, _, logs) = run_subaccount_tx(&bank, &mint_keypair, ix);
    assert!(
        status.is_ok(),
        "read of full range must succeed: {status:?}\nlogs:\n{}",
        logs.join("\n"),
    );

    // The subaccount was created, so the stored data must equal `content`.
    let storage_addr = subaccount_storage_addr_for(&base_pubkey, &program_id);
    let stored = bank
        .get_account(&storage_addr)
        .expect("subaccount must exist after a successful read tx");
    assert_eq!(stored.data(), content);
}

/// Happy path with a non-zero offset: read a window from the middle of the
/// stored data. The program verifies the returned bytes equal
/// `content[offset..offset+length]`.
#[test]
#[cfg(feature = "sbf_rust")]
fn test_program_sbf_subaccount_read_success_offset() {
    let (bank, _bank_client, _bank_forks, mint_keypair, program_id) =
        deploy_subaccount_program(1_000_000_000);

    let payer_pubkey = mint_keypair.pubkey();
    let base_pubkey = Pubkey::new_unique();
    let content: &[u8] = b"read-subaccount-content-32-bytes";

    let instruction =
        subaccount_create_instruction(program_id, base_pubkey, payer_pubkey, 0, content, vec![]);
    let (status, _inner_instructions, log_messages) =
        run_subaccount_tx(&bank, &mint_keypair, instruction);
    assert!(
        status.is_ok(),
        "tx failed: {status:?}\nlogs:\n{}",
        log_messages.join("\n"),
    );

    let storage_addr = subaccount_storage_addr_for(&base_pubkey, &program_id);
    let stored = bank
        .get_account(&storage_addr)
        .expect("subaccount must exist after a successful read tx");
    assert_eq!(stored.data(), content);

    // Read 8 bytes starting at offset 10 — strictly inside the 32-byte buffer.
    let payload = subaccount_read_payload(false, false, 10, 8, content);
    let ix =
        subaccount_create_instruction(program_id, base_pubkey, payer_pubkey, 9, &payload, vec![]);
    let (status, _, logs) = run_subaccount_tx(&bank, &mint_keypair, ix);
    assert!(
        status.is_ok(),
        "in-range read at offset 10 must succeed: {status:?}\nlogs:\n{}",
        logs.join("\n"),
    );
}

/// Happy path with a non-zero offset: read a window from the middle of the
/// stored data. The program verifies the returned bytes equal
/// `content[offset..offset+length]`.
#[test]
#[cfg(feature = "sbf_rust")]
fn test_program_sbf_subaccount_read_success_already_loaded() {
    let (bank, _bank_client, _bank_forks, mint_keypair, program_id) =
        deploy_subaccount_program(1_000_000_000);

    let payer_pubkey = mint_keypair.pubkey();
    let base_pubkey = Pubkey::new_unique();
    let content: &[u8] = b"read-subaccount-content-32-bytes";

    // Read 8 bytes starting at offset 10 — strictly inside the 32-byte buffer.
    let payload = subaccount_read_payload(true, true, 10, 8, content);
    let ix =
        subaccount_create_instruction(program_id, base_pubkey, payer_pubkey, 9, &payload, vec![]);
    let (status, _, logs) = run_subaccount_tx(&bank, &mint_keypair, ix);
    assert!(
        status.is_ok(),
        "in-range read at offset 10 must succeed: {status:?}\nlogs:\n{}",
        logs.join("\n"),
    );
}

/// Edge case — no subaccount: reading bytes from a subaccount that was never
/// created sees empty data, so any positive-length read is out of range and
/// the syscall fails with `InvalidArgument`.
#[test]
#[cfg(feature = "sbf_rust")]
fn test_program_sbf_subaccount_read_missing_subaccount() {
    let (bank, _bank_client, _bank_forks, mint_keypair, program_id) =
        deploy_subaccount_program(1_000_000_000);

    let payer_pubkey = mint_keypair.pubkey();
    let base_pubkey = Pubkey::new_unique();

    // do_create = false ⇒ the subaccount does not exist; read 8 bytes.
    let payload = subaccount_read_payload(false, false, 0, 8, &[]);
    let ix =
        subaccount_create_instruction(program_id, base_pubkey, payer_pubkey, 9, &payload, vec![]);
    let (status, _, logs) = run_subaccount_tx(&bank, &mint_keypair, ix);
    assert_eq!(
        status,
        Err(TransactionError::InstructionError(
            0,
            InstructionError::InvalidArgument
        )),
        "read of a missing subaccount must fail with InvalidArgument\nlogs:\n{}",
        logs.join("\n"),
    );

    // The subaccount wasn't created, so the stored data must be empty.
    let storage_addr = subaccount_storage_addr_for(&base_pubkey, &program_id);
    assert!(
        bank.get_account(&storage_addr).is_none(),
        "failed tx must not persist any subaccount state",
    );
}

/// Edge case — no subaccount: reading zero bytes from a subaccount that was never
/// created sees empty data, so the syscall succeeds.
#[test]
#[cfg(feature = "sbf_rust")]
fn test_program_sbf_subaccount_read_zero_length_missing_subaccount() {
    let (bank, _bank_client, _bank_forks, mint_keypair, program_id) =
        deploy_subaccount_program(1_000_000_000);

    let payer_pubkey = mint_keypair.pubkey();
    let base_pubkey = Pubkey::new_unique();

    // do_create = false ⇒ the subaccount does not exist; read 0 bytes.
    let payload = subaccount_read_payload(false, false, 0, 0, &[]);
    let ix =
        subaccount_create_instruction(program_id, base_pubkey, payer_pubkey, 9, &payload, vec![]);
    let (status, _, logs) = run_subaccount_tx(&bank, &mint_keypair, ix);
    assert!(
        status.is_ok(),
        "zero-length read of a missing subaccount must succeed\nlogs:\n{}",
        logs.join("\n"),
    );

    // The subaccount wasn't created, so the stored data must be empty.
    let storage_addr = subaccount_storage_addr_for(&base_pubkey, &program_id);
    assert!(
        bank.get_account(&storage_addr).is_none(),
        "failed tx must not persist any subaccount state",
    );
}

/// Edge case — fully out of range: the read window starts at `offset == len`,
/// so none of the requested bytes exist. The syscall fails with
/// `InvalidArgument`.
#[test]
#[cfg(feature = "sbf_rust")]
fn test_program_sbf_subaccount_read_out_of_range_full() {
    let (bank, _bank_client, _bank_forks, mint_keypair, program_id) =
        deploy_subaccount_program(1_000_000_000);

    let payer_pubkey = mint_keypair.pubkey();
    let base_pubkey = Pubkey::new_unique();
    let content: &[u8] = b"read-subaccount-content-32-bytes";

    // offset == content.len() ⇒ the entire [offset, offset+8) range is past
    // the end of the data.
    let payload = subaccount_read_payload(true, false, content.len() as u64, 8, content);
    let ix =
        subaccount_create_instruction(program_id, base_pubkey, payer_pubkey, 9, &payload, vec![]);
    let (status, _, logs) = run_subaccount_tx(&bank, &mint_keypair, ix);
    assert_eq!(
        status,
        Err(TransactionError::InstructionError(
            0,
            InstructionError::InvalidArgument
        )),
        "fully out-of-range read must fail with InvalidArgument\nlogs:\n{}",
        logs.join("\n"),
    );
}

/// Edge case — partially out of range: the read window starts inside the data
/// but extends past the end. The syscall must reject the whole read with
/// `InvalidArgument` (no partial copy).
#[test]
#[cfg(feature = "sbf_rust")]
fn test_program_sbf_subaccount_read_out_of_range_partial() {
    let (bank, _bank_client, _bank_forks, mint_keypair, program_id) =
        deploy_subaccount_program(1_000_000_000);

    let payer_pubkey = mint_keypair.pubkey();
    let base_pubkey = Pubkey::new_unique();
    let content: &[u8] = b"read-subaccount-content-32-bytes";

    // offset 28 + length 8 = 36 > 32 ⇒ starts inside the data but runs past
    // the end.
    let payload = subaccount_read_payload(true, false, 28, 8, content);
    let ix =
        subaccount_create_instruction(program_id, base_pubkey, payer_pubkey, 9, &payload, vec![]);
    let (status, _, logs) = run_subaccount_tx(&bank, &mint_keypair, ix);
    assert_eq!(
        status,
        Err(TransactionError::InstructionError(
            0,
            InstructionError::InvalidArgument
        )),
        "partially out-of-range read must fail with InvalidArgument\nlogs:\n{}",
        logs.join("\n"),
    );
}

/// Contrasts the two persistence paths through the dirty filter in
/// `TransactionAccounts::deconstruct_into_keyed_account_shared_data`, which only
/// drains *touched* subaccounts:
///
///   * `read_sub` is only read (`sol_read_subaccount`) in Tx2. An immutable
///     read never sets the touched flag, so the account is NOT re-stored — its
///     modified-slot stays at Tx1's slot.
///   * `mod_sub` is loaded writable and overwritten in Tx2. A writable load
///     marks the subaccount touched, so it IS re-stored — its modified-slot
///     advances to Tx2's slot.
///
/// Both subaccounts are created in the same Tx1 slot, so comparing their
/// modified-slots after Tx2 pins the per-subaccount granularity of the filter.
#[test]
#[cfg(feature = "sbf_rust")]
fn test_program_sbf_subaccount_read_only_does_not_restore() {
    let (bank, mut bank_client, bank_forks, mint_keypair, program_id) =
        deploy_subaccount_program(1_000_000_000);

    let payer_pubkey = mint_keypair.pubkey();
    let read_base = Pubkey::new_unique();
    let mod_base = Pubkey::new_unique();
    let content: &[u8] = b"read-subaccount-content-32-bytes";
    let content_v2: &[u8] = b"OVERWRITTEN-subaccount-32-bytes!";
    assert_eq!(content.len(), 32);
    assert_eq!(content_v2.len(), content.len());

    // Tx1 — create both subaccounts (two transactions in the same slot).
    let ix1a =
        subaccount_create_instruction(program_id, read_base, payer_pubkey, 0, content, vec![]);
    let (status, _, logs) = run_subaccount_tx(&bank, &mint_keypair, ix1a);
    assert!(
        status.is_ok(),
        "Tx1 create (read_sub) failed: {status:?}\nlogs:\n{}",
        logs.join("\n"),
    );
    let ix1b =
        subaccount_create_instruction(program_id, mod_base, payer_pubkey, 0, content, vec![]);
    let (status, _, logs) = run_subaccount_tx(&bank, &mint_keypair, ix1b);
    assert!(
        status.is_ok(),
        "Tx1 create (mod_sub) failed: {status:?}\nlogs:\n{}",
        logs.join("\n"),
    );

    let read_storage = subaccount_storage_addr_for(&read_base, &program_id);
    let mod_storage = subaccount_storage_addr_for(&mod_base, &program_id);
    let (_read_acc, read_created_slot) = bank
        .get_account_modified_slot(&read_storage)
        .expect("read_sub must exist after create");
    let (_mod_acc, mod_created_slot) = bank
        .get_account_modified_slot(&mod_storage)
        .expect("mod_sub must exist after create");

    // Advance into a new slot so a re-store in Tx2 is observable as a changed
    // modified-slot.
    let bank = bank_client
        .advance_slot(1, bank_forks.as_ref(), &Pubkey::default())
        .expect("advance slot for Tx2");
    let tx2_slot = bank.slot();
    assert_ne!(
        tx2_slot, read_created_slot,
        "Tx2 must run in a different slot than the create tx",
    );

    // Tx2a — read-only `sol_read_subaccount` of read_sub (no create, no load).
    let payload = subaccount_read_payload(false, false, 0, content.len() as u64, content);
    let ix2a =
        subaccount_create_instruction(program_id, read_base, payer_pubkey, 9, &payload, vec![]);
    let (status, _, logs) = run_subaccount_tx(&bank, &mint_keypair, ix2a);
    assert!(
        status.is_ok(),
        "Tx2 read failed: {status:?}\nlogs:\n{}",
        logs.join("\n"),
    );

    // Tx2b — writable load + same-length overwrite of mod_sub (disc=1).
    let ix2b = subaccount_instruction(program_id, mod_base, true, 1, content_v2, vec![]);
    let (status, _, logs) = run_subaccount_tx(&bank, &mint_keypair, ix2b);
    assert!(
        status.is_ok(),
        "Tx2 modify failed: {status:?}\nlogs:\n{}",
        logs.join("\n"),
    );

    // read_sub: unchanged data, modified-slot still at Tx1's slot.
    let (read_stored, read_modified_slot) = bank
        .get_account_modified_slot(&read_storage)
        .expect("read_sub must still exist after a read-only tx");
    assert_eq!(
        read_stored.data(),
        content,
        "a read must not change the stored subaccount data",
    );
    assert_eq!(
        read_modified_slot, read_created_slot,
        "a read-only sol_read_subaccount must not re-store the subaccount \
         (modified-slot advanced {read_created_slot} -> {read_modified_slot})",
    );

    // mod_sub: overwritten data, modified-slot advanced to Tx2's slot.
    let (mod_stored, mod_modified_slot) = bank
        .get_account_modified_slot(&mod_storage)
        .expect("mod_sub must still exist after the modifying tx");
    assert_eq!(
        mod_stored.data(),
        content_v2,
        "the writable overwrite must be on-chain",
    );
    assert_ne!(
        mod_modified_slot, mod_created_slot,
        "a modified subaccount must be re-stored (modified-slot must advance \
         from the create slot {mod_created_slot})",
    );
    assert_eq!(
        mod_modified_slot, tx2_slot,
        "a modified subaccount must be re-stored at the modifying tx's slot \
         (expected {tx2_slot}, got {mod_modified_slot})",
    );
}

/// PRS-155: end-to-end check that a subaccount-touching transaction surfaces
/// its subaccount addresses through the balance-recording pipeline and into
/// `TransactionStatusMeta` / `UiTransactionStatusMeta`.
///
/// Executes a real `sol_create_subaccount` + `sol_load_subaccount` tx with
/// transaction balance recording enabled, then verifies:
///   (a) `subaccount_addresses` is non-empty in the (Ui)meta;
///   (b) the recorded address equals the owner-facing pubkey
///       `sol_load_subaccount` derives from the seeds + program id;
///   (c) the tail of `pre_balances`/`post_balances` (positions
///       `[account_keys.len()..)`) lines up one-for-one with
///       `subaccount_addresses`, carrying the subaccount's pre/post lamports.
#[test]
#[cfg(feature = "sbf_rust")]
#[cfg(any())] // excluded: needs fork-only TransactionStatusMeta.subaccount_addresses (RPC status-meta feature not ported to agave-4.0)
fn test_program_sbf_subaccount_addresses_in_status_meta() {
    use {
        solana_ledger::transaction_balances::compile_collected_balances,
        solana_transaction_context::create_subaccount_address,
        solana_transaction_status::{
            option_serializer::OptionSerializer, TransactionStatusMeta, UiTransactionStatusMeta,
        },
    };

    let (bank, _bank_client, _bank_forks, mint_keypair, program_id) =
        deploy_subaccount_program(1_000_000_000);

    let base_pubkey = Pubkey::new_unique();
    let payer_pubkey = mint_keypair.pubkey();
    let payload: &[u8] = b"subaccount-meta-payload";

    // disc=0 — create + load + write + unload. The `sol_load_subaccount` here
    // is what materializes the subaccount lane the balance collector reads.
    let instruction =
        subaccount_create_instruction(program_id, base_pubkey, payer_pubkey, 0, payload, vec![]);
    let tx = Transaction::new_signed_with_payer(
        &[instruction],
        Some(&payer_pubkey),
        &[&mint_keypair],
        bank.last_blockhash(),
    );
    let account_keys_len = tx.message.account_keys.len();

    let tx_batch = bank.prepare_batch_for_tests(vec![tx]);
    let (mut commit_results, balance_collector) = bank.load_execute_and_commit_transactions(
        &tx_batch,
        MAX_PROCESSING_AGE,
        ExecutionRecordingConfig {
            enable_cpi_recording: false,
            enable_log_recording: true,
            enable_return_data_recording: false,
            enable_transaction_balance_recording: true,
        },
        &mut ExecuteTimings::default(),
        None,
    );

    let committed = commit_results.pop().unwrap().expect("tx must commit");
    let logs = committed.log_messages.clone().unwrap_or_default();
    assert!(
        committed.status.is_ok(),
        "tx failed: {:?}\nlogs:\n{}",
        committed.status,
        logs.join("\n"),
    );

    let balance_collector =
        balance_collector.expect("balance recording was enabled, collector must exist");
    let (balances, _token_balances, subaccount_keys, _unchanged_subaccount_keys) =
        compile_collected_balances(balance_collector);

    // Single-transaction batch ⇒ index 0.
    let pre_balances = &balances.pre_balances[0];
    let post_balances = &balances.post_balances[0];
    let tx_subaccount_keys = &subaccount_keys[0];

    // (b) The recorded subaccount key is the owner-facing pubkey the program
    // sees from `sol_load_subaccount`, derived from `[base, "test-sub"]`.
    let expected_owner =
        create_subaccount_address(&[base_pubkey.as_ref(), SUBACCOUNT_SEED_TAG], &program_id)
            .expect("derive owner-facing subaccount pubkey");
    assert_eq!(
        tx_subaccount_keys,
        &vec![expected_owner],
        "balance collector must record exactly the touched subaccount's owner pubkey",
    );

    // (c) `pre_balances`/`post_balances` carry one tail entry per subaccount,
    // appended after the `account_keys.len()` regular accounts, in the same
    // order as `subaccount_keys`.
    assert_eq!(
        pre_balances.len(),
        account_keys_len + tx_subaccount_keys.len(),
        "pre_balances must extend the {account_keys_len} account-key slots with one entry per subaccount",
    );
    assert_eq!(
        post_balances.len(),
        pre_balances.len(),
        "pre/post balance vectors must stay aligned",
    );
    // The subaccount is created in this tx, so its pre-lamports (read from the
    // loader cache before `update_accounts_for_executed_tx`) are 0, and its
    // post-lamports equal the funding the program transferred in.
    assert_eq!(
        &pre_balances[account_keys_len..],
        &[0],
        "subaccount pre-balance tail must be the pre-execution lamports (0 for a freshly created subaccount)",
    );
    assert_eq!(
        &post_balances[account_keys_len..],
        &[SUBACCOUNT_FUNDING_LAMPORTS],
        "subaccount post-balance tail must be the funded lamports",
    );

    // (a) The addresses survive into the meta and its Ui projection.
    let meta = TransactionStatusMeta {
        status: committed.status.clone(),
        pre_balances: pre_balances.clone(),
        post_balances: post_balances.clone(),
        subaccount_addresses: tx_subaccount_keys.clone(),
        ..TransactionStatusMeta::default()
    };
    assert!(
        !meta.subaccount_addresses.is_empty(),
        "meta.subaccount_addresses must be populated for a subaccount-touching tx",
    );

    let ui_meta: UiTransactionStatusMeta = meta.into();
    match ui_meta.subaccount_addresses {
        OptionSerializer::Some(ref addrs) => {
            assert_eq!(
                addrs,
                &vec![expected_owner.to_string()],
                "Ui meta must expose the owner-facing subaccount address as a base58 string",
            );
        }
        other => panic!("expected Ui subaccount_addresses to be Some, got {other:?}"),
    }
}

/// PRS-155: a read-only `sol_read_subaccount` of an existing subaccount must
/// surface that subaccount in the receipt's *unchanged* lane
/// (`unchanged_subaccount_addresses`, owner key only) and NOT in the *changed*
/// lane (`subaccount_addresses` + the pre/post balance tails). Complements
/// `test_program_sbf_subaccount_addresses_in_status_meta`, which covers the
/// changed lane.
#[test]
#[cfg(feature = "sbf_rust")]
#[cfg(any())] // excluded: needs fork-only TransactionStatusMeta.subaccount_addresses (RPC status-meta feature not ported to agave-4.0)
fn test_program_sbf_subaccount_unchanged_addresses_in_status_meta() {
    use {
        solana_ledger::transaction_balances::compile_collected_balances,
        solana_transaction_context::create_subaccount_address,
    };

    let (bank, _bank_client, _bank_forks, mint_keypair, program_id) =
        deploy_subaccount_program(1_000_000_000);

    let base_pubkey = Pubkey::new_unique();
    let payer_pubkey = mint_keypair.pubkey();
    let content: &[u8] = b"read-subaccount-content-32-bytes";

    // Tx1 — create + write the subaccount so it exists on-chain for Tx2 to read.
    let ix1 =
        subaccount_create_instruction(program_id, base_pubkey, payer_pubkey, 0, content, vec![]);
    let (status, _, logs) = run_subaccount_tx(&bank, &mint_keypair, ix1);
    assert!(
        status.is_ok(),
        "Tx1 create failed: {status:?}\nlogs:\n{}",
        logs.join("\n"),
    );

    // Tx2 — read-only `sol_read_subaccount` (do_create = false, do_load = false),
    // with transaction balance recording enabled.
    let payload = subaccount_read_payload(false, false, 0, content.len() as u64, content);
    let ix2 =
        subaccount_create_instruction(program_id, base_pubkey, payer_pubkey, 9, &payload, vec![]);
    let tx2 = Transaction::new_signed_with_payer(
        &[ix2],
        Some(&payer_pubkey),
        &[&mint_keypair],
        bank.last_blockhash(),
    );
    let account_keys_len = tx2.message.account_keys.len();

    let tx_batch = bank.prepare_batch_for_tests(vec![tx2]);
    let (mut commit_results, balance_collector) = bank.load_execute_and_commit_transactions(
        &tx_batch,
        MAX_PROCESSING_AGE,
        ExecutionRecordingConfig {
            enable_cpi_recording: false,
            enable_log_recording: true,
            enable_return_data_recording: false,
            enable_transaction_balance_recording: true,
        },
        &mut ExecuteTimings::default(),
        None,
    );

    let committed = commit_results.pop().unwrap().expect("read tx must commit");
    assert!(
        committed.status.is_ok(),
        "Tx2 read failed: {:?}\nlogs:\n{}",
        committed.status,
        committed
            .log_messages
            .clone()
            .unwrap_or_default()
            .join("\n"),
    );

    let balance_collector =
        balance_collector.expect("balance recording was enabled, collector must exist");
    let (balances, _token_balances, subaccount_keys, unchanged_subaccount_keys) =
        compile_collected_balances(balance_collector);

    let expected_owner =
        create_subaccount_address(&[base_pubkey.as_ref(), SUBACCOUNT_SEED_TAG], &program_id)
            .expect("derive owner-facing subaccount pubkey");

    // The read-only subaccount belongs to the unchanged lane, by owner key only.
    assert!(
        subaccount_keys[0].is_empty(),
        "a read-only tx must not record any changed subaccount, got {:?}",
        subaccount_keys[0],
    );
    assert_eq!(
        &unchanged_subaccount_keys[0],
        &vec![expected_owner],
        "the read-only subaccount must be reported in the unchanged lane by owner key",
    );

    // Unchanged subaccounts carry no balance tail — pre/post stay sized to the
    // transaction's account keys only.
    assert_eq!(
        balances.pre_balances[0].len(),
        account_keys_len,
        "unchanged subaccounts must not extend the pre_balances tail",
    );
    assert_eq!(
        balances.post_balances[0].len(),
        account_keys_len,
        "unchanged subaccounts must not extend the post_balances tail",
    );

    // And it survives into the receipt meta's unchanged lane.
    let meta = solana_transaction_status::TransactionStatusMeta {
        status: committed.status.clone(),
        unchanged_subaccount_addresses: unchanged_subaccount_keys[0].clone(),
        ..solana_transaction_status::TransactionStatusMeta::default()
    };
    let ui_meta: solana_transaction_status::UiTransactionStatusMeta = meta.into();
    match ui_meta.unchanged_subaccount_addresses {
        solana_transaction_status::option_serializer::OptionSerializer::Some(ref addrs) => {
            assert_eq!(addrs, &vec![expected_owner.to_string()]);
        }
        other => panic!("expected Ui unchanged_subaccount_addresses Some, got {other:?}"),
    }
}

/// PRS-155 regression: the subaccount **pre**-balance recorded for an *existing*
/// on-chain subaccount that is loaded for the first time in THIS transaction
/// must be its real pre-tx balance — NOT a cache-miss `0`.
///
/// `collect_subaccount_pre_balances` reads pre-state via
/// `account_loader.load_account(&subaccount_storage_address(owner))` and falls
/// back to `0` on a `None`. For a freshly-created subaccount `0` is correct,
/// but for an already-funded subaccount loaded fresh this tx, `load_account`
/// must fall through to accounts-db and return the funded balance. This test
/// funds a subaccount in Tx1, then in a later slot loads + mutates it (so it
/// lands in the *changed* lane, which carries pre/post balances) and asserts
/// the pre-balance tail equals the funded amount.
#[test]
#[cfg(feature = "sbf_rust")]
#[cfg(any())] // excluded: needs fork-only TransactionStatusMeta.subaccount_addresses (RPC status-meta feature not ported to agave-4.0)
fn test_program_sbf_subaccount_pre_balance_real_for_existing_loaded_fresh() {
    use {
        solana_ledger::transaction_balances::compile_collected_balances,
        solana_transaction_context::create_subaccount_address,
    };

    let (bank, mut bank_client, bank_forks, mint_keypair, program_id) =
        deploy_subaccount_program(1_000_000_000);

    let base_pubkey = Pubkey::new_unique();
    let payer_pubkey = mint_keypair.pubkey();
    let content_v1: &[u8] = b"read-subaccount-content-32-bytes"; // 32 bytes
    let content_v2: &[u8] = b"OVERWRITTEN-subaccount-32-bytes!"; // 32 bytes, same len
    assert_eq!(content_v1.len(), content_v2.len());

    // Tx1 — create + fund the subaccount so it lives on-chain at its storage
    // address with `SUBACCOUNT_FUNDING_LAMPORTS`.
    let ix1 =
        subaccount_create_instruction(program_id, base_pubkey, payer_pubkey, 0, content_v1, vec![]);
    let (status, _, logs) = run_subaccount_tx(&bank, &mint_keypair, ix1);
    assert!(
        status.is_ok(),
        "Tx1 create failed: {status:?}\nlogs:\n{}",
        logs.join("\n"),
    );
    let storage_addr = subaccount_storage_addr_for(&base_pubkey, &program_id);
    assert_eq!(
        bank.get_account(&storage_addr)
            .expect("subaccount must exist after create")
            .lamports(),
        SUBACCOUNT_FUNDING_LAMPORTS,
    );

    // Advance into a new slot so Tx2 loads the subaccount fresh (the loader
    // cache for this batch starts empty for its storage address).
    let bank = bank_client
        .advance_slot(1, bank_forks.as_ref(), &Pubkey::default())
        .expect("advance slot for Tx2");

    // Tx2 — load the existing subaccount + overwrite its data (same length, so
    // lamports stay put), with balance recording on. disc=1. The writable load
    // marks it touched, so it lands in the changed lane.
    let ix2 = subaccount_instruction(program_id, base_pubkey, true, 1, content_v2, vec![]);
    let tx2 = Transaction::new_signed_with_payer(
        &[ix2],
        Some(&payer_pubkey),
        &[&mint_keypair],
        bank.last_blockhash(),
    );
    let account_keys_len = tx2.message.account_keys.len();

    let tx_batch = bank.prepare_batch_for_tests(vec![tx2]);
    let (mut commit_results, balance_collector) = bank.load_execute_and_commit_transactions(
        &tx_batch,
        MAX_PROCESSING_AGE,
        ExecutionRecordingConfig {
            enable_cpi_recording: false,
            enable_log_recording: true,
            enable_return_data_recording: false,
            enable_transaction_balance_recording: true,
        },
        &mut ExecuteTimings::default(),
        None,
    );

    let committed = commit_results.pop().unwrap().expect("Tx2 must commit");
    assert!(
        committed.status.is_ok(),
        "Tx2 failed: {:?}\nlogs:\n{}",
        committed.status,
        committed
            .log_messages
            .clone()
            .unwrap_or_default()
            .join("\n"),
    );

    let balance_collector =
        balance_collector.expect("balance recording was enabled, collector must exist");
    let (balances, _token_balances, subaccount_keys, _unchanged_subaccount_keys) =
        compile_collected_balances(balance_collector);

    let expected_owner =
        create_subaccount_address(&[base_pubkey.as_ref(), SUBACCOUNT_SEED_TAG], &program_id)
            .expect("derive owner-facing subaccount pubkey");

    // The mutated existing subaccount is in the changed lane (carries balances).
    assert_eq!(
        &subaccount_keys[0],
        &vec![expected_owner],
        "a loaded + mutated existing subaccount must be in the changed lane",
    );

    let pre = &balances.pre_balances[0];
    let post = &balances.post_balances[0];
    assert_eq!(
        pre.len(),
        account_keys_len + 1,
        "one subaccount pre tail entry"
    );
    assert_eq!(post.len(), account_keys_len + 1);

    // The crux: the pre-balance is the REAL pre-tx funded amount, proving
    // `load_account` fell through to accounts-db rather than returning a
    // cache-miss 0 for an existing subaccount loaded fresh this tx.
    assert_eq!(
        pre[account_keys_len], SUBACCOUNT_FUNDING_LAMPORTS,
        "pre-balance of an existing subaccount loaded fresh must be its real \
         pre-tx balance, not a cache-miss 0",
    );
    // Only data changed this tx; lamports are unchanged.
    assert_eq!(post[account_keys_len], SUBACCOUNT_FUNDING_LAMPORTS);
}

/// PRS-155 B9 end-to-end: a single transaction that **creates** one subaccount
/// (ix 0, base A) and **loads + mutates an existing** one (ix 1, base B, funded
/// in a prior slot). Both land in the changed lane, so the receipt must:
///   - expose exactly two `subaccount_addresses`;
///   - keep them in lane order (A created first, then B), aligned one-for-one
///     with the `pre_balances`/`post_balances` tails at `[head + i]`.
///
/// The created subaccount A has pre-balance 0 (didn't exist) and post-balance
/// = funding; the existing subaccount B has pre = post = its prior funding.
/// That asymmetry pins the address↔balance correspondence by index.
#[test]
#[cfg(feature = "sbf_rust")]
#[cfg(any())] // excluded: needs fork-only TransactionStatusMeta.subaccount_addresses (RPC status-meta feature not ported to agave-4.0)
fn test_program_sbf_subaccount_create_and_load_existing_addresses_ordered() {
    use {
        solana_ledger::transaction_balances::compile_collected_balances,
        solana_transaction_context::create_subaccount_address,
        solana_transaction_status::TransactionStatusMeta,
    };

    let (bank, mut bank_client, bank_forks, mint_keypair, program_id) =
        deploy_subaccount_program(1_000_000_000);

    let payer_pubkey = mint_keypair.pubkey();
    let base_a = Pubkey::new_unique(); // created in the main tx
    let base_b = Pubkey::new_unique(); // funded now, loaded in the main tx
    let content_a: &[u8] = b"create-subaccount-A--32-bytes!!!"; // 32 bytes
    let content_b_v1: &[u8] = b"existing-subaccount-B-32-bytes!!"; // 32 bytes
    let content_b_v2: &[u8] = b"B-overwritten-in-main-tx-32bytes"; // 32 bytes
    assert_eq!(content_a.len(), 32);
    assert_eq!(content_b_v1.len(), content_b_v2.len());

    // Tx0 — bring subaccount B on-chain (create + fund) in an earlier slot.
    let ix_b0 =
        subaccount_create_instruction(program_id, base_b, payer_pubkey, 0, content_b_v1, vec![]);
    let (status, _, logs) = run_subaccount_tx(&bank, &mint_keypair, ix_b0);
    assert!(
        status.is_ok(),
        "Tx0 create B failed: {status:?}\nlogs:\n{}",
        logs.join("\n"),
    );

    // Advance so the main tx loads B fresh.
    let bank = bank_client
        .advance_slot(1, bank_forks.as_ref(), &Pubkey::default())
        .expect("advance slot for main tx");

    // Main tx — ix0 creates A, ix1 loads + overwrites existing B. Two
    // instructions, one transaction; the subaccount lane accumulates both in
    // call order (A then B).
    let ix_create_a =
        subaccount_create_instruction(program_id, base_a, payer_pubkey, 0, content_a, vec![]);
    let ix_load_b = subaccount_instruction(program_id, base_b, true, 1, content_b_v2, vec![]);
    let tx = Transaction::new_signed_with_payer(
        &[ix_create_a, ix_load_b],
        Some(&payer_pubkey),
        &[&mint_keypair],
        bank.last_blockhash(),
    );
    let head = tx.message.account_keys.len();

    let tx_batch = bank.prepare_batch_for_tests(vec![tx]);
    let (mut commit_results, balance_collector) = bank.load_execute_and_commit_transactions(
        &tx_batch,
        MAX_PROCESSING_AGE,
        ExecutionRecordingConfig {
            enable_cpi_recording: false,
            enable_log_recording: true,
            enable_return_data_recording: false,
            enable_transaction_balance_recording: true,
        },
        &mut ExecuteTimings::default(),
        None,
    );

    let committed = commit_results.pop().unwrap().expect("main tx must commit");
    assert!(
        committed.status.is_ok(),
        "main tx failed: {:?}\nlogs:\n{}",
        committed.status,
        committed
            .log_messages
            .clone()
            .unwrap_or_default()
            .join("\n"),
    );

    let balance_collector =
        balance_collector.expect("balance recording was enabled, collector must exist");
    let (balances, _token_balances, subaccount_keys, _unchanged_subaccount_keys) =
        compile_collected_balances(balance_collector);

    let owner_a =
        create_subaccount_address(&[base_a.as_ref(), SUBACCOUNT_SEED_TAG], &program_id).unwrap();
    let owner_b =
        create_subaccount_address(&[base_b.as_ref(), SUBACCOUNT_SEED_TAG], &program_id).unwrap();

    let pre = &balances.pre_balances[0];
    let post = &balances.post_balances[0];

    // Build the receipt meta exactly as the status service does.
    let meta = TransactionStatusMeta {
        status: committed.status.clone(),
        pre_balances: pre.clone(),
        post_balances: post.clone(),
        subaccount_addresses: subaccount_keys[0].clone(),
        ..TransactionStatusMeta::default()
    };

    // (1) Two subaccounts touched ⇒ two addresses in the receipt.
    assert_eq!(
        meta.subaccount_addresses.len(),
        2,
        "a tx touching two subaccounts must record two addresses, got {:?}",
        meta.subaccount_addresses,
    );

    // (2) Lane order: A (created) first, then B (existing).
    assert_eq!(
        meta.subaccount_addresses,
        vec![owner_a, owner_b],
        "subaccount_addresses must be in lane order: created-then-loaded",
    );

    // (3) Address[i] corresponds to (pre_balances[head + i], post_balances[head + i]).
    assert_eq!(pre.len(), head + 2, "two subaccount pre tail entries");
    assert_eq!(post.len(), head + 2);
    // A (index 0): created this tx ⇒ pre 0, post funded.
    assert_eq!(
        pre[head], 0,
        "created subaccount A pre-balance must be 0 (did not exist pre-tx)",
    );
    assert_eq!(post[head], SUBACCOUNT_FUNDING_LAMPORTS);
    // B (index 1): existed ⇒ pre funded; only data changed ⇒ post funded.
    assert_eq!(
        pre[head + 1],
        SUBACCOUNT_FUNDING_LAMPORTS,
        "existing subaccount B pre-balance must be its real pre-tx funding",
    );
    assert_eq!(post[head + 1], SUBACCOUNT_FUNDING_LAMPORTS);

    // Both subaccounts persisted with their expected data.
    let stored_a = bank
        .get_account(&subaccount_storage_addr_for(&base_a, &program_id))
        .expect("A must be on-chain");
    assert_eq!(stored_a.data(), content_a);
    let stored_b = bank
        .get_account(&subaccount_storage_addr_for(&base_b, &program_id))
        .expect("B must be on-chain");
    assert_eq!(stored_b.data(), content_b_v2);
}
