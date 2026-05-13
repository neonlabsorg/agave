use {
    solana_core::banking_stage::transaction_scheduler::scheduler_controller::HotAccount,
    solana_pubkey::Pubkey,
    std::{fs, path::Path},
};

#[derive(serde::Deserialize)]
struct HotAccountFileEntry {
    pubkey: String,
    #[serde(default)]
    weight: Option<u64>,
}

pub fn parse_hot_accounts_file(path: &Path) -> Result<Vec<HotAccount>, String> {
    let bytes = fs::read(path).map_err(|err| format!("read failed: {err}"))?;
    let entries: Vec<HotAccountFileEntry> =
        serde_json::from_slice(&bytes).map_err(|err| format!("invalid JSON: {err}"))?;
    entries
        .into_iter()
        .map(|entry| {
            let pubkey = entry
                .pubkey
                .parse::<Pubkey>()
                .map_err(|err| format!("invalid pubkey {:?}: {err}", entry.pubkey))?;
            Ok(HotAccount {
                pubkey,
                weight: entry.weight.unwrap_or(1),
            })
        })
        .collect()
}
