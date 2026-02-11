use {
    crate::commands::{FromClapArgMatches, Result},
    clap::{value_t, ArgMatches},
    serde::Deserialize,
    solana_accounts_db::accounts_index::AccountSecondaryIndexes,
    solana_rpc::{
        rpc::{JsonRpcConfig, RpcBigtableConfig},
        tx_type_rules::{anchor_discriminator_from_method_name, parse_discriminator, RpcTxTypeRule},
    },
    std::{fs, str::FromStr},
};

impl FromClapArgMatches for JsonRpcConfig {
    fn from_clap_arg_match(matches: &ArgMatches) -> Result<Self> {
        let rpc_bigtable_config = if matches.is_present("enable_rpc_bigtable_ledger_storage")
            || matches.is_present("enable_bigtable_ledger_upload")
        {
            Some(RpcBigtableConfig::from_clap_arg_match(matches)?)
        } else {
            None
        };
        let tx_type_rules = load_tx_type_rules(matches)?;

        Ok(JsonRpcConfig {
            enable_rpc_transaction_history: matches.is_present("enable_rpc_transaction_history"),
            enable_extended_tx_metadata_storage: matches
                .is_present("enable_extended_tx_metadata_storage"),
            faucet_addr: matches
                .value_of("rpc_faucet_addr")
                .map(|address| {
                    solana_net_utils::parse_host_port(address).map_err(|err| {
                        crate::commands::Error::Dynamic(Box::<dyn std::error::Error>::from(
                            format!("failed to parse rpc_faucet_addr: {err}"),
                        ))
                    })
                })
                .transpose()?,
            health_check_slot_distance: value_t!(matches, "health_check_slot_distance", u64)?,
            skip_preflight_health_check: matches.is_present("skip_preflight_health_check"),
            rpc_bigtable_config,
            max_multiple_accounts: Some(value_t!(matches, "rpc_max_multiple_accounts", usize)?),
            account_indexes: AccountSecondaryIndexes::from_clap_arg_match(matches)?,
            rpc_threads: value_t!(matches, "rpc_threads", usize)?,
            rpc_blocking_threads: value_t!(matches, "rpc_blocking_threads", usize)?,
            rpc_niceness_adj: value_t!(matches, "rpc_niceness_adj", i8)?,
            full_api: matches.is_present("full_rpc_api"),
            rpc_scan_and_fix_roots: matches.is_present("rpc_scan_and_fix_roots"),
            max_request_body_size: Some(value_t!(matches, "rpc_max_request_body_size", usize)?),
            tx_type_rules,
            disable_health_check: false,
        })
    }
}

#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum DiscriminatorInput {
    String(String),
    Number(u64),
}

impl DiscriminatorInput {
    fn as_text(&self) -> String {
        match self {
            Self::String(value) => value.clone(),
            Self::Number(value) => value.to_string(),
        }
    }
}

#[derive(Debug, Deserialize)]
struct RpcTxTypeRuleSerde {
    program_id: String,
    discriminator: Option<DiscriminatorInput>,
    method_name: Option<String>,
    tx_type: String,
}

#[derive(Debug, Deserialize)]
struct RpcTxTypeRuleFile {
    rules: Vec<RpcTxTypeRuleSerde>,
}

fn parse_tx_type_rule(
    program_id: &str,
    discriminator_input: Option<&str>,
    method_name: Option<&str>,
    tx_type: &str,
) -> Result<RpcTxTypeRule> {
    let program_id = solana_pubkey::Pubkey::from_str(program_id).map_err(|err| {
        crate::commands::Error::Dynamic(Box::<dyn std::error::Error>::from(format!(
            "invalid program_id `{program_id}` in tx type rule: {err}"
        )))
    })?;
    let discriminator = if let Some(method_name) = method_name {
        anchor_discriminator_from_method_name(method_name)
    } else if let Some(discriminator_input) = discriminator_input {
        parse_discriminator(discriminator_input).map_err(|err| {
            crate::commands::Error::Dynamic(Box::<dyn std::error::Error>::from(format!(
                "invalid discriminator `{discriminator_input}` in tx type rule: {err}"
            )))
        })?
    } else {
        return Err(crate::commands::Error::Dynamic(
            Box::<dyn std::error::Error>::from(
                "tx type rule must provide either `method_name` or `discriminator`".to_string(),
            ),
        ));
    };
    Ok(RpcTxTypeRule {
        program_id,
        discriminator,
        tx_type: tx_type.to_string(),
    })
}

fn parse_inline_rule(rule: &str) -> Result<RpcTxTypeRule> {
    let mut parts = rule.splitn(3, ':');
    let Some(program_id) = parts.next() else {
        unreachable!();
    };
    let Some(method_or_discriminator) = parts.next() else {
        return Err(crate::commands::Error::Dynamic(
            Box::<dyn std::error::Error>::from(format!(
                "invalid `--rpc-tx-type-map-rule` format `{rule}`; expected PROGRAM_ID:METHOD_NAME|DISCRIMINATOR_HEX|DISCRIMINATOR_U64:TX_TYPE"
            )),
        ));
    };
    let Some(tx_type) = parts.next() else {
        return Err(crate::commands::Error::Dynamic(
            Box::<dyn std::error::Error>::from(format!(
                "invalid `--rpc-tx-type-map-rule` format `{rule}`; expected PROGRAM_ID:METHOD_NAME|DISCRIMINATOR_HEX|DISCRIMINATOR_U64:TX_TYPE"
            )),
        ));
    };
    let method_name = if parse_discriminator(method_or_discriminator).is_err() {
        Some(method_or_discriminator)
    } else {
        None
    };
    let discriminator_input = if method_name.is_none() {
        Some(method_or_discriminator)
    } else {
        None
    };
    parse_tx_type_rule(program_id, discriminator_input, method_name, tx_type)
}

pub fn load_tx_type_rules(matches: &ArgMatches) -> Result<Vec<RpcTxTypeRule>> {
    let mut rules = Vec::new();

    if let Some(path) = matches.value_of("rpc_tx_type_map_config") {
        let content = fs::read_to_string(path).map_err(|err| {
            crate::commands::Error::Dynamic(Box::<dyn std::error::Error>::from(format!(
                "failed to read --rpc-tx-type-map-config `{path}`: {err}"
            )))
        })?;

        if let Ok(file) = serde_yaml::from_str::<RpcTxTypeRuleFile>(&content) {
            for rule in file.rules {
                let discriminator_text = rule.discriminator.as_ref().map(DiscriminatorInput::as_text);
                rules.push(parse_tx_type_rule(
                    &rule.program_id,
                    discriminator_text.as_deref(),
                    rule.method_name.as_deref(),
                    &rule.tx_type,
                )?);
            }
        } else if let Ok(file_rules) = serde_yaml::from_str::<Vec<RpcTxTypeRuleSerde>>(&content) {
            for rule in file_rules {
                let discriminator_text = rule.discriminator.as_ref().map(DiscriminatorInput::as_text);
                rules.push(parse_tx_type_rule(
                    &rule.program_id,
                    discriminator_text.as_deref(),
                    rule.method_name.as_deref(),
                    &rule.tx_type,
                )?);
            }
        } else {
            return Err(crate::commands::Error::Dynamic(
                Box::<dyn std::error::Error>::from(format!(
                    "failed to parse --rpc-tx-type-map-config `{path}`; expected YAML/JSON list of rules or {{rules: [...]}}"
                )),
            ));
        }
    }

    if let Some(values) = matches.values_of("rpc_tx_type_map_rule") {
        for value in values {
            rules.push(parse_inline_rule(value)?);
        }
    }

    Ok(rules)
}

#[cfg(test)]
mod tests {
    #[cfg(not(target_os = "linux"))]
    use crate::commands::run::args::tests::verify_args_struct_by_command_run_is_error_with_identity_setup;
    use {
        super::*,
        crate::commands::run::args::{
            tests::verify_args_struct_by_command_run_with_identity_setup, DefaultArgs, RunArgs,
        },
        solana_rpc::rpc_pubsub_service::PubSubConfig,
        std::{
            net::{Ipv4Addr, SocketAddr},
            num::NonZeroUsize,
        },
    };

    #[test]
    fn verify_args_struct_by_command_run_with_enable_rpc_transaction_history() {
        {
            let default_run_args = crate::commands::run::args::RunArgs::default();
            let expected_args = RunArgs {
                json_rpc_config: JsonRpcConfig {
                    enable_rpc_transaction_history: true,
                    ..default_run_args.json_rpc_config.clone()
                },
                ..default_run_args.clone()
            };
            verify_args_struct_by_command_run_with_identity_setup(
                default_run_args,
                vec!["--enable-rpc-transaction-history"],
                expected_args,
            );
        }
    }

    #[test]
    fn verify_args_struct_by_command_run_with_enable_extended_tx_metadata_storage() {
        {
            let default_run_args = crate::commands::run::args::RunArgs::default();
            let expected_args = RunArgs {
                json_rpc_config: JsonRpcConfig {
                    enable_rpc_transaction_history: true,
                    enable_extended_tx_metadata_storage: true,
                    ..default_run_args.json_rpc_config.clone()
                },
                ..default_run_args.clone()
            };
            verify_args_struct_by_command_run_with_identity_setup(
                default_run_args,
                vec![
                    "--enable-rpc-transaction-history", // required by enable_extended_tx_metadata_storage
                    "--enable-extended-tx-metadata-storage",
                ],
                expected_args,
            );
        }
    }

    #[test]
    fn verify_args_struct_by_command_run_with_rpc_faucet_addr() {
        {
            let default_run_args = crate::commands::run::args::RunArgs::default();
            let expected_args = RunArgs {
                json_rpc_config: JsonRpcConfig {
                    faucet_addr: Some(SocketAddr::from((Ipv4Addr::LOCALHOST, 8000))),
                    ..default_run_args.json_rpc_config.clone()
                },
                ..default_run_args.clone()
            };
            verify_args_struct_by_command_run_with_identity_setup(
                default_run_args,
                vec!["--rpc-faucet-address", "127.0.0.1:8000"],
                expected_args,
            );
        }
    }

    #[test]
    fn verify_args_struct_by_command_run_with_health_check_slot_distance() {
        {
            let default_run_args = crate::commands::run::args::RunArgs::default();
            let expected_args = RunArgs {
                json_rpc_config: JsonRpcConfig {
                    health_check_slot_distance: 100,
                    ..default_run_args.json_rpc_config.clone()
                },
                ..default_run_args.clone()
            };
            verify_args_struct_by_command_run_with_identity_setup(
                default_run_args,
                vec!["--health-check-slot-distance", "100"],
                expected_args,
            );
        }
    }

    #[test]
    fn verify_args_struct_by_command_run_with_skip_preflight_health_check() {
        {
            let default_run_args = crate::commands::run::args::RunArgs::default();
            let expected_args = RunArgs {
                json_rpc_config: JsonRpcConfig {
                    skip_preflight_health_check: true,
                    ..default_run_args.json_rpc_config.clone()
                },
                ..default_run_args.clone()
            };
            verify_args_struct_by_command_run_with_identity_setup(
                default_run_args,
                vec!["--skip-preflight-health-check"],
                expected_args,
            );
        }
    }

    #[test]
    fn verify_args_struct_by_command_run_with_max_multiple_accounts() {
        {
            let default_run_args = crate::commands::run::args::RunArgs::default();
            let expected_args = RunArgs {
                json_rpc_config: JsonRpcConfig {
                    max_multiple_accounts: Some(9999),
                    ..default_run_args.json_rpc_config.clone()
                },
                ..default_run_args.clone()
            };
            verify_args_struct_by_command_run_with_identity_setup(
                default_run_args,
                vec!["--rpc-max-multiple-accounts", "9999"],
                expected_args,
            );
        }
    }

    #[test]
    fn verify_args_struct_by_command_run_with_rpc_threads() {
        {
            let default_run_args = crate::commands::run::args::RunArgs::default();
            let expected_args = RunArgs {
                json_rpc_config: JsonRpcConfig {
                    rpc_threads: 10,
                    ..default_run_args.json_rpc_config.clone()
                },
                ..default_run_args.clone()
            };
            verify_args_struct_by_command_run_with_identity_setup(
                default_run_args,
                vec!["--rpc-threads", "10"],
                expected_args,
            );
        }
    }

    #[test]
    fn verify_args_struct_by_command_run_with_rpc_blocking_threads() {
        {
            let default_run_args = crate::commands::run::args::RunArgs::default();
            let expected_args = RunArgs {
                json_rpc_config: JsonRpcConfig {
                    rpc_blocking_threads: 999,
                    ..default_run_args.json_rpc_config.clone()
                },
                ..default_run_args.clone()
            };
            verify_args_struct_by_command_run_with_identity_setup(
                default_run_args,
                vec!["--rpc-blocking-threads", "999"],
                expected_args,
            );
        }
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn verify_args_struct_by_command_run_with_rpc_niceness_adj() {
        {
            let default_run_args = crate::commands::run::args::RunArgs::default();
            let expected_args = RunArgs {
                json_rpc_config: JsonRpcConfig {
                    rpc_niceness_adj: 10,
                    ..default_run_args.json_rpc_config.clone()
                },
                ..default_run_args.clone()
            };
            verify_args_struct_by_command_run_with_identity_setup(
                default_run_args,
                vec!["--rpc-niceness-adjustment", "10"],
                expected_args,
            );
        }
    }

    #[cfg(not(target_os = "linux"))]
    #[test]
    fn verify_args_struct_by_command_run_with_rpc_niceness_adj() {
        verify_args_struct_by_command_run_is_error_with_identity_setup(
            crate::commands::run::args::RunArgs::default(),
            vec!["--rpc-niceness-adjustment", "10"],
        );
    }

    #[test]
    fn verify_args_struct_by_command_run_with_full_api() {
        {
            let default_args = DefaultArgs::new();
            let default_run_args = crate::commands::run::args::RunArgs::default();
            let expected_args = RunArgs {
                json_rpc_config: JsonRpcConfig {
                    full_api: true,
                    ..default_run_args.json_rpc_config.clone()
                },
                pub_sub_config: PubSubConfig {
                    notification_threads: Some(
                        NonZeroUsize::new(
                            default_args
                                .rpc_pubsub_notification_threads
                                .parse::<usize>()
                                .unwrap(),
                        )
                        .unwrap(),
                    ),
                    ..default_run_args.pub_sub_config.clone()
                },
                ..default_run_args.clone()
            };
            verify_args_struct_by_command_run_with_identity_setup(
                default_run_args,
                vec!["--full-rpc-api"],
                expected_args,
            );
        }
    }

    #[test]
    fn verify_args_struct_by_command_run_with_rpc_scan_and_fix_roots() {
        {
            let default_run_args = crate::commands::run::args::RunArgs::default();
            let expected_args = RunArgs {
                json_rpc_config: JsonRpcConfig {
                    enable_rpc_transaction_history: true,
                    rpc_scan_and_fix_roots: true,
                    ..default_run_args.json_rpc_config.clone()
                },
                ..default_run_args.clone()
            };
            verify_args_struct_by_command_run_with_identity_setup(
                default_run_args,
                vec![
                    "--enable-rpc-transaction-history", // required by --rpc-scan-and-fix-roots
                    "--rpc-scan-and-fix-roots",
                ],
                expected_args,
            );
        }
    }

    #[test]
    fn verify_args_struct_by_command_run_with_rpc_max_request_body_size() {
        // long arg
        {
            let default_run_args = RunArgs::default();
            let expected_args = RunArgs {
                json_rpc_config: JsonRpcConfig {
                    max_request_body_size: Some(999),
                    ..default_run_args.json_rpc_config.clone()
                },
                ..default_run_args.clone()
            };
            verify_args_struct_by_command_run_with_identity_setup(
                default_run_args,
                vec!["--rpc-max-request-body-size", "999"],
                expected_args,
            );
        }
    }
}
