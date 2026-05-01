use {
    solana_message::SanitizedMessage,
    solana_pubkey::Pubkey,
    solana_sha256_hasher::hash,
    solana_transaction::sanitized::SanitizedTransaction,
    std::{
        collections::{HashMap, HashSet},
        sync::{LazyLock, RwLock},
    },
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RpcTxTypeRule {
    pub program_id: Pubkey,
    pub discriminator: [u8; 8],
    pub tx_type: String,
}

type TxTypeRuleMap = HashMap<(Pubkey, [u8; 8]), String>;

static TX_TYPE_RULES: LazyLock<RwLock<TxTypeRuleMap>> =
    LazyLock::new(|| RwLock::new(HashMap::new()));

pub fn set_rules(rules: Vec<RpcTxTypeRule>) {
    // Collect unique tx_type names before consuming rules so the Prometheus
    // label series can be pre-initialized to zero.
    let type_names: HashSet<String> = rules.iter().map(|r| r.tx_type.clone()).collect();

    let map = rules
        .into_iter()
        .map(|rule| ((rule.program_id, rule.discriminator), rule.tx_type))
        .collect::<TxTypeRuleMap>();
    if let Ok(mut guard) = TX_TYPE_RULES.write() {
        *guard = map;
    }

    let labels: Vec<&str> = type_names.iter().map(|s| s.as_str()).collect();
    solana_metrics::custom_metrics::init_tx_type_series(&labels);
}

/// Classify each instruction in `message` against configured rules and return
/// the resulting labels in iteration order. Duplicates are preserved so a
/// transaction with three matching instructions of the same type contributes
/// `+3` to its label counters. Instructions that do not match any rule are
/// skipped — no program-id fallback is applied.
pub fn infer_types_from_message(message: &SanitizedMessage) -> Vec<String> {
    let Ok(guard) = TX_TYPE_RULES.read() else {
        return Vec::new();
    };
    if guard.is_empty() {
        return Vec::new();
    }

    let mut types = Vec::new();
    for (program_id, instruction) in message.program_instructions_iter() {
        let Some(data) = instruction.data.get(0..8) else {
            continue;
        };
        let mut discriminator = [0_u8; 8];
        discriminator.copy_from_slice(data);
        if let Some(tx_type) = guard.get(&(*program_id, discriminator)) {
            types.push(tx_type.clone());
        }
    }
    types
}

pub fn infer_types_from_transaction(transaction: &SanitizedTransaction) -> Vec<String> {
    infer_types_from_message(transaction.message())
}

pub fn parse_discriminator(input: &str) -> Result<[u8; 8], String> {
    let value = input.trim();
    if value.is_empty() {
        return Err("empty discriminator".to_string());
    }

    let hex_value = value.strip_prefix("0x").unwrap_or(value);
    let is_hex = value.starts_with("0x")
        || hex_value
            .chars()
            .any(|c| matches!(c, 'a'..='f' | 'A'..='F'));
    if is_hex {
        if hex_value.len() != 16 {
            return Err(format!(
                "invalid discriminator hex length: expected 16, got {}",
                hex_value.len()
            ));
        }

        let mut out = [0_u8; 8];
        for (i, byte) in out.iter_mut().enumerate() {
            let start = i * 2;
            *byte = u8::from_str_radix(&hex_value[start..start + 2], 16)
                .map_err(|err| format!("invalid discriminator hex: {err}"))?;
        }
        return Ok(out);
    }

    let method_number = value
        .parse::<u64>()
        .map_err(|err| format!("invalid discriminator number `{value}`: {err}"))?;
    Ok(method_number.to_le_bytes())
}

pub fn anchor_discriminator_from_method_name(method_name: &str) -> [u8; 8] {
    let preimage = format!("global:{method_name}");
    let digest = hash(preimage.as_bytes()).to_bytes();
    let mut out = [0_u8; 8];
    out.copy_from_slice(&digest[..8]);
    out
}

#[cfg(test)]
mod tests {
    use {
        super::*,
        agave_reserved_account_keys::ReservedAccountKeys,
        serial_test::serial,
        solana_instruction::Instruction,
        solana_keypair::Keypair,
        solana_message::{LegacyMessage, Message, SanitizedMessage},
        solana_signer::Signer,
        solana_system_interface::instruction as system_instruction,
    };

    fn reset_rules(rules: Vec<RpcTxTypeRule>) {
        let map: TxTypeRuleMap = rules
            .into_iter()
            .map(|rule| ((rule.program_id, rule.discriminator), rule.tx_type))
            .collect();
        *TX_TYPE_RULES.write().unwrap() = map;
    }

    fn labelled_program() -> (Pubkey, [u8; 8]) {
        let mut disc = [0_u8; 8];
        disc[0] = 0xAB;
        (Pubkey::new_unique(), disc)
    }

    fn legacy_message(instructions: Vec<Instruction>) -> SanitizedMessage {
        let payer = Keypair::new();
        let message = Message::new(&instructions, Some(&payer.pubkey()));
        SanitizedMessage::Legacy(LegacyMessage::new(
            message,
            &ReservedAccountKeys::empty_key_set(),
        ))
    }

    #[test]
    #[serial]
    fn empty_rules_produce_empty_vec() {
        reset_rules(Vec::new());
        let msg = legacy_message(vec![Instruction::new_with_bytes(
            Pubkey::new_unique(),
            &[0_u8; 16],
            vec![],
        )]);
        assert!(infer_types_from_message(&msg).is_empty());
    }

    #[test]
    #[serial]
    fn single_matching_instruction_yields_single_label() {
        let (program_id, discriminator) = labelled_program();
        reset_rules(vec![RpcTxTypeRule {
            program_id,
            discriminator,
            tx_type: "place_order".into(),
        }]);

        let mut data = discriminator.to_vec();
        data.extend_from_slice(&[0_u8; 4]);
        let msg = legacy_message(vec![Instruction::new_with_bytes(program_id, &data, vec![])]);
        assert_eq!(
            infer_types_from_message(&msg),
            vec!["place_order".to_string()]
        );
    }

    #[test]
    #[serial]
    fn duplicate_matches_are_preserved() {
        let (program_id, discriminator) = labelled_program();
        reset_rules(vec![RpcTxTypeRule {
            program_id,
            discriminator,
            tx_type: "cancel".into(),
        }]);

        let mut data = discriminator.to_vec();
        data.extend_from_slice(&[0_u8; 4]);
        let ix = Instruction::new_with_bytes(program_id, &data, vec![]);
        let msg = legacy_message(vec![ix.clone(), ix.clone(), ix]);
        assert_eq!(
            infer_types_from_message(&msg),
            vec!["cancel".to_string(), "cancel".into(), "cancel".into()]
        );
    }

    #[test]
    #[serial]
    fn unmatched_instructions_do_not_fall_back() {
        let (matched_program, matched_disc) = labelled_program();
        reset_rules(vec![RpcTxTypeRule {
            program_id: matched_program,
            discriminator: matched_disc,
            tx_type: "trade".into(),
        }]);

        let mut matched_data = matched_disc.to_vec();
        matched_data.extend_from_slice(&[0_u8; 4]);
        let unmatched =
            system_instruction::transfer(&Keypair::new().pubkey(), &Keypair::new().pubkey(), 1);
        let matched_instr =
            Instruction::new_with_bytes(matched_program, &matched_data, vec![]);
        let msg = legacy_message(vec![unmatched, matched_instr]);
        assert_eq!(
            infer_types_from_message(&msg),
            vec!["trade".to_string()]
        );
    }

    #[test]
    #[serial]
    fn no_rules_means_system_transfer_yields_empty_vec() {
        reset_rules(Vec::new());
        let transfer_ix =
            system_instruction::transfer(&Keypair::new().pubkey(), &Keypair::new().pubkey(), 1);
        let msg = legacy_message(vec![transfer_ix]);
        assert!(infer_types_from_message(&msg).is_empty());
    }
}
