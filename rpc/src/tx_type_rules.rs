use {
    solana_message::SanitizedMessage,
    solana_pubkey::Pubkey,
    solana_sha256_hasher::hash,
    solana_transaction::sanitized::SanitizedTransaction,
    std::{
        collections::HashMap,
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
    let map = rules
        .into_iter()
        .map(|rule| ((rule.program_id, rule.discriminator), rule.tx_type))
        .collect::<TxTypeRuleMap>();
    if let Ok(mut guard) = TX_TYPE_RULES.write() {
        *guard = map;
    }
}

pub fn infer_from_message(message: &SanitizedMessage, is_simple_vote: bool) -> String {
    if is_simple_vote {
        return "vote".to_string();
    }

    if let Ok(guard) = TX_TYPE_RULES.read() {
        for (program_id, instruction) in message.program_instructions_iter() {
            let Some(data) = instruction.data.get(0..8) else {
                continue;
            };
            let mut discriminator = [0_u8; 8];
            discriminator.copy_from_slice(data);
            if let Some(tx_type) = guard.get(&(*program_id, discriminator)) {
                return tx_type.clone();
            }
        }
    }

    if let Some((program_id, _)) = message.program_instructions_iter().next() {
        if *program_id == solana_system_interface::program::id() {
            "system".to_string()
        } else if *program_id == spl_generic_token::token::id() {
            "spl_token".to_string()
        } else if *program_id == spl_generic_token::token_2022::id() {
            "spl_token_2022".to_string()
        } else if *program_id == solana_stake_program::id() {
            "stake".to_string()
        } else if *program_id == solana_vote_program::id() {
            "vote_program".to_string()
        } else {
            "unknown".to_string()
        }
    } else {
        "unknown".to_string()
    }
}

pub fn infer_from_transaction(transaction: &SanitizedTransaction) -> String {
    infer_from_message(
        transaction.message(),
        transaction.is_simple_vote_transaction(),
    )
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
        for i in 0..8 {
            let start = i * 2;
            let byte = u8::from_str_radix(&hex_value[start..start + 2], 16)
                .map_err(|err| format!("invalid discriminator hex: {err}"))?;
            out[i] = byte;
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
