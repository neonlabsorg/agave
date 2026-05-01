//! Native solana-metrics datapoint emissions for Parasol fork observability.
//!
//! Mirrors the public API of the Prometheus-based `custom_metrics` module on
//! `parasol-dev` so call sites stay structurally identical, but every counter /
//! gauge / histogram observation is funnelled through `datapoint_info!` rather
//! than a Prometheus registry. There is no exposition endpoint — the values
//! land in InfluxDB via the standard `SOLANA_METRICS_CONFIG` write_url path.

use {
    solana_signature::Signature,
    std::{
        collections::{HashMap, HashSet},
        sync::{
            atomic::{AtomicU64, Ordering},
            LazyLock, Mutex,
        },
        time::Instant,
    },
};

const MAX_TX_TYPE_SERIES: usize = 32;

static TX_TYPE_KNOWN: LazyLock<Mutex<HashSet<String>>> =
    LazyLock::new(|| Mutex::new(HashSet::new()));
static TX_ACCEPTANCE_TIMES: LazyLock<Mutex<HashMap<Signature, Instant>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));
static LOCKED_ACCOUNTS_TOTAL: AtomicU64 = AtomicU64::new(0);
static LOCKED_ACCOUNTS_SAMPLES: AtomicU64 = AtomicU64::new(0);

fn bounded_tx_type(tx_type: &str) -> String {
    let tx_type = normalize_tx_type(tx_type);
    let mut known = TX_TYPE_KNOWN.lock().unwrap();
    if known.contains(&tx_type) {
        return tx_type;
    }
    if known.len() < MAX_TX_TYPE_SERIES {
        known.insert(tx_type.clone());
        return tx_type;
    }
    known.insert("other".to_string());
    "other".to_string()
}

fn normalize_tx_type(tx_type: &str) -> String {
    if tx_type.is_empty() {
        return "unknown".to_string();
    }
    let normalized = tx_type
        .chars()
        .map(|ch| match ch {
            'a'..='z' | '0'..='9' | '_' => ch,
            'A'..='Z' => ch.to_ascii_lowercase(),
            '-' | ' ' | ':' | '/' => '_',
            _ => '_',
        })
        .collect::<String>();
    let normalized = normalized.trim_matches('_');
    if normalized.is_empty() {
        "unknown".to_string()
    } else {
        normalized.chars().take(48).collect::<String>()
    }
}

pub fn register_tx_acceptance_time(signature: Signature, accepted_at: Instant) {
    if let Ok(mut values) = TX_ACCEPTANCE_TIMES.lock() {
        values.insert(signature, accepted_at);
    }
}

pub fn get_tx_acceptance_time(signature: &Signature) -> Option<Instant> {
    TX_ACCEPTANCE_TIMES
        .lock()
        .ok()
        .and_then(|values| values.get(signature).copied())
}

pub fn clear_tx_acceptance_time(signature: &Signature) {
    if let Ok(mut values) = TX_ACCEPTANCE_TIMES.lock() {
        values.remove(signature);
    }
}

/// On the Prometheus version this pre-touches label-vec series so they appear
/// with value 0 even before any matching transaction arrives. Native datapoints
/// have no pre-registration model — series materialise on first emit. The
/// signature is kept identical to preserve call-site parity with parasol-dev.
pub fn init_tx_type_series(_labels: &[&str]) {
    // no-op for native datapoints
}

pub fn inc_tx_accepted_total(value: u64) {
    crate::datapoint_info!("tx_accepted_total", ("count", value as i64, i64));
}

pub fn inc_tx_accepted_total_with_type(value: u64, tx_type: &str) {
    let tx_type = bounded_tx_type(tx_type);
    crate::datapoint_info!(
        "tx_accepted_total",
        ("count", value as i64, i64),
        ("tx_type", tx_type, String)
    );
}

pub fn inc_tx_executed_total(value: u64) {
    crate::datapoint_info!("tx_executed_total", ("count", value as i64, i64));
}

pub fn inc_tx_executed_total_with_type(value: u64, tx_type: &str) {
    let tx_type = bounded_tx_type(tx_type);
    crate::datapoint_info!(
        "tx_executed_total",
        ("count", value as i64, i64),
        ("tx_type", tx_type, String)
    );
}

pub fn inc_tx_failed_total(value: u64) {
    crate::datapoint_info!("tx_failed_total", ("count", value as i64, i64));
}

pub fn inc_tx_failed_total_with_type(value: u64, tx_type: &str) {
    let tx_type = bounded_tx_type(tx_type);
    crate::datapoint_info!(
        "tx_failed_total",
        ("count", value as i64, i64),
        ("tx_type", tx_type, String)
    );
}

pub fn inc_tx_dropped_total(value: u64) {
    crate::datapoint_info!("tx_dropped_total", ("count", value as i64, i64));
}

pub fn inc_tx_dropped_with_reason(value: u64, reason: &str) {
    crate::datapoint_info!(
        "tx_dropped_total",
        ("count", value as i64, i64),
        ("reason", reason, String)
    );
}

pub fn inc_tx_expired_total(value: u64) {
    crate::datapoint_info!("tx_expired_total", ("count", value as i64, i64));
}

pub fn inc_tx_expired_total_with_type(value: u64, tx_type: &str) {
    let tx_type = bounded_tx_type(tx_type);
    crate::datapoint_info!(
        "tx_expired_total",
        ("count", value as i64, i64),
        ("tx_type", tx_type, String)
    );
}

pub fn inc_confirmation_timeout_rate(value: u64) {
    crate::datapoint_info!("confirmation_timeout_rate", ("count", value as i64, i64));
}

pub fn inc_account_lock_conflict_rate(value: u64) {
    crate::datapoint_info!("account_lock_conflict_rate", ("count", value as i64, i64));
}

pub fn inc_retry_due_to_account_in_use_rate(value: u64) {
    crate::datapoint_info!(
        "retry_due_to_account_in_use_rate",
        ("count", value as i64, i64)
    );
}

pub fn inc_fork_rate(value: u64) {
    crate::datapoint_info!("fork_rate", ("count", value as i64, i64));
}

pub fn inc_duplicate_confirmed_blocks(value: u64) {
    crate::datapoint_info!("duplicate_confirmed_blocks", ("count", value as i64, i64));
}

pub fn set_mempool_size(size: u64) {
    crate::datapoint_info!("mempool_size", ("value", size as i64, i64));
}

pub fn set_scheduler_buffer_size(size: u64) {
    crate::datapoint_info!("scheduler_buffer_size", ("value", size as i64, i64));
}

pub fn set_scheduler_buffer_queue_size(size: u64) {
    crate::datapoint_info!("scheduler_buffer_queue_size", ("value", size as i64, i64));
}

pub fn set_scheduler_buffer_capacity(capacity: u64) {
    crate::datapoint_info!("scheduler_buffer_capacity", ("value", capacity as i64, i64));
}

pub fn observe_avg_locked_accounts_per_tx(locked_accounts: u64) {
    LOCKED_ACCOUNTS_TOTAL.fetch_add(locked_accounts, Ordering::Relaxed);
    let samples = LOCKED_ACCOUNTS_SAMPLES.fetch_add(1, Ordering::Relaxed) + 1;
    let total = LOCKED_ACCOUNTS_TOTAL.load(Ordering::Relaxed);
    let avg = if samples == 0 {
        0.0
    } else {
        total as f64 / samples as f64
    };
    crate::datapoint_info!("avg_locked_accounts_per_tx", ("value", avg, f64));
}

pub fn observe_blockhash_fetch_latency_us(value_us: u64) {
    crate::datapoint_info!(
        "blockhash_fetch_latency_seconds",
        ("value_us", value_us as i64, i64)
    );
}

pub fn observe_blockhash_age_at_submit_slots(value_slots: u64) {
    crate::datapoint_info!(
        "blockhash_age_at_submit_slots",
        ("value_slots", value_slots as i64, i64)
    );
}

pub fn observe_blockhash_remaining_validity_slots(value_slots: u64) {
    crate::datapoint_info!(
        "blockhash_remaining_validity_slots",
        ("value_slots", value_slots as i64, i64)
    );
}

pub fn observe_time_to_processed_us(value_us: u64) {
    crate::datapoint_info!(
        "time_to_processed_seconds",
        ("value_us", value_us as i64, i64)
    );
}

pub fn observe_time_to_confirmed_us(value_us: u64) {
    crate::datapoint_info!(
        "time_to_confirmed_seconds",
        ("value_us", value_us as i64, i64)
    );
}

pub fn observe_time_to_finalized_us(value_us: u64) {
    crate::datapoint_info!(
        "time_to_finalized_seconds",
        ("value_us", value_us as i64, i64)
    );
}

pub fn observe_node_ingress_latency_us(value_us: u64) {
    crate::datapoint_info!(
        "node_ingress_latency_seconds",
        ("value_us", value_us as i64, i64)
    );
}

pub fn observe_node_ingress_latency_us_with_type(value_us: u64, tx_type: &str) {
    let tx_type = bounded_tx_type(tx_type);
    crate::datapoint_info!(
        "node_ingress_latency_seconds",
        ("value_us", value_us as i64, i64),
        ("tx_type", tx_type, String)
    );
}

pub fn observe_mempool_acceptance_latency_us(value_us: u64) {
    crate::datapoint_info!(
        "mempool_acceptance_latency_seconds",
        ("value_us", value_us as i64, i64)
    );
}

pub fn observe_mempool_acceptance_latency_us_with_type(value_us: u64, tx_type: &str) {
    let tx_type = bounded_tx_type(tx_type);
    crate::datapoint_info!(
        "mempool_acceptance_latency_seconds",
        ("value_us", value_us as i64, i64),
        ("tx_type", tx_type, String)
    );
}

pub fn observe_decision_response_latency_us(value_us: u64) {
    crate::datapoint_info!(
        "decision_response_latency_seconds",
        ("value_us", value_us as i64, i64)
    );
}

pub fn observe_decision_response_latency_us_with_type(value_us: u64, tx_type: &str) {
    let tx_type = bounded_tx_type(tx_type);
    crate::datapoint_info!(
        "decision_response_latency_seconds",
        ("value_us", value_us as i64, i64),
        ("tx_type", tx_type, String)
    );
}

pub fn observe_node_to_decision_response_latency_us(value_us: u64) {
    crate::datapoint_info!(
        "node_to_decision_response_latency_seconds",
        ("value_us", value_us as i64, i64)
    );
}

pub fn observe_node_to_decision_response_latency_us_with_type(value_us: u64, tx_type: &str) {
    let tx_type = bounded_tx_type(tx_type);
    crate::datapoint_info!(
        "node_to_decision_response_latency_seconds",
        ("value_us", value_us as i64, i64),
        ("tx_type", tx_type, String)
    );
}

pub fn observe_tx_execution_in_block_latency_us(value_us: u64) {
    crate::datapoint_info!(
        "tx_execution_in_block_latency_seconds",
        ("value_us", value_us as i64, i64)
    );
}

pub fn observe_block_production_latency_us(value_us: u64) {
    crate::datapoint_info!(
        "block_production_latency_seconds",
        ("value_us", value_us as i64, i64)
    );
}

pub fn observe_state_update_notification_latency_us(value_us: u64) {
    crate::datapoint_info!(
        "state_update_notification_latency_seconds",
        ("value_us", value_us as i64, i64)
    );
}

pub fn observe_acknowledge_latency_us(value_us: u64) {
    crate::datapoint_info!(
        "acknowledge_latency_seconds",
        ("value_us", value_us as i64, i64)
    );
}

pub fn observe_acknowledge_latency_us_with_type(value_us: u64, tx_type: &str) {
    let tx_type = bounded_tx_type(tx_type);
    crate::datapoint_info!(
        "acknowledge_latency_seconds",
        ("value_us", value_us as i64, i64),
        ("tx_type", tx_type, String)
    );
}
