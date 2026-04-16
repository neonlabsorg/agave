use {
    prometheus::{
        Encoder, Gauge, Histogram, HistogramOpts, HistogramVec, IntCounter, IntCounterVec, Opts,
        Registry, TextEncoder,
    },
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

const LATENCY_BUCKETS_US: &[u64] = &[
    50, 100, 250, 500, 1_000, 2_500, 5_000, 10_000, 25_000, 50_000, 100_000, 250_000, 500_000,
    1_000_000, 2_500_000, 5_000_000, 10_000_000,
];
const BLOCKHASH_AGE_BUCKETS_SLOTS: &[u64] = &[0, 1, 2, 4, 8, 16, 32, 64, 96, 128, 160, 192];
const BLOCKHASH_REMAINING_VALIDITY_BUCKETS_SLOTS: &[u64] = &[
    0, 1, 2, 4, 8, 16, 32, 64, 96, 128, 160, 192, 256, 512, 1_024, 2_048, 4_096, 8_192, 16_384,
    32_768, 65_536, 131_072, 262_144, 524_288, 1_048_576, 2_097_152, 4_194_304,
];
const MAX_TX_TYPE_SERIES: usize = 32;

fn latency_buckets_seconds() -> Vec<f64> {
    LATENCY_BUCKETS_US
        .iter()
        .map(|value| *value as f64 / 1_000_000.0)
        .collect()
}

fn slots_buckets(values: &[u64]) -> Vec<f64> {
    values.iter().map(|value| *value as f64).collect()
}

fn make_counter(name: &str, help: &str) -> IntCounter {
    IntCounter::with_opts(Opts::new(name, help)).expect("counter opts must be valid")
}

fn make_counter_vec(name: &str, help: &str) -> IntCounterVec {
    IntCounterVec::new(Opts::new(name, help), &["tx_type"]).expect("counter vec opts must be valid")
}

fn make_latency_histogram(name: &str, help: &str) -> Histogram {
    Histogram::with_opts(HistogramOpts::new(name, help).buckets(latency_buckets_seconds()))
        .expect("histogram opts must be valid")
}

fn make_latency_histogram_vec(name: &str, help: &str) -> HistogramVec {
    HistogramVec::new(
        HistogramOpts::new(name, help).buckets(latency_buckets_seconds()),
        &["tx_type"],
    )
    .expect("histogram vec opts must be valid")
}

fn make_slots_histogram(name: &str, help: &str, buckets: &[u64]) -> Histogram {
    Histogram::with_opts(HistogramOpts::new(name, help).buckets(slots_buckets(buckets)))
        .expect("histogram opts must be valid")
}

struct CustomMetrics {
    registry: Registry,
    labeled_registry: Registry,

    tx_accepted_total: IntCounter,
    tx_accepted_total_by_type: IntCounterVec,
    tx_executed_total: IntCounter,
    tx_executed_total_by_type: IntCounterVec,
    tx_failed_total: IntCounter,
    tx_failed_total_by_type: IntCounterVec,
    tx_dropped_total: IntCounter,
    tx_dropped_by_reason: IntCounterVec,
    tx_expired_total: IntCounter,
    tx_expired_total_by_type: IntCounterVec,
    confirmation_timeout_rate: IntCounter,
    account_lock_conflict_rate: IntCounter,
    retry_due_to_account_in_use_rate: IntCounter,
    fork_rate: IntCounter,
    duplicate_confirmed_blocks: IntCounter,

    mempool_size: Gauge,
    avg_locked_accounts_per_tx: Gauge,
    locked_accounts_total: AtomicU64,
    locked_accounts_samples: AtomicU64,

    blockhash_fetch_latency: Histogram,
    blockhash_age_at_submit_slots: Histogram,
    blockhash_remaining_validity_slots: Histogram,
    time_to_processed: Histogram,
    time_to_confirmed: Histogram,
    time_to_finalized: Histogram,
    node_ingress_latency: Histogram,
    node_ingress_latency_by_type: HistogramVec,
    mempool_acceptance_latency: Histogram,
    mempool_acceptance_latency_by_type: HistogramVec,
    decision_response_latency: Histogram,
    decision_response_latency_by_type: HistogramVec,
    node_to_decision_response_latency: Histogram,
    node_to_decision_response_latency_by_type: HistogramVec,
    tx_execution_in_block_latency: Histogram,
    block_production_latency: Histogram,
    state_update_notification_latency: Histogram,
    acknowledge_latency: Histogram,
    acknowledge_latency_by_type: HistogramVec,

    tx_type_known: Mutex<HashSet<String>>,
}

impl CustomMetrics {
    fn new() -> Self {
        let registry = Registry::new();
        let labeled_registry = Registry::new();

        let tx_accepted_total = make_counter(
            "tx_accepted_total",
            "Transactions accepted into retry/acceptance pipeline",
        );
        registry
            .register(Box::new(tx_accepted_total.clone()))
            .expect("metric registration must succeed");
        let tx_accepted_total_by_type =
            make_counter_vec("tx_accepted_total", "Transactions accepted by tx_type");
        labeled_registry
            .register(Box::new(tx_accepted_total_by_type.clone()))
            .expect("metric registration must succeed");

        let tx_executed_total = make_counter("tx_executed_total", "Successfully committed transactions");
        registry
            .register(Box::new(tx_executed_total.clone()))
            .expect("metric registration must succeed");
        let tx_executed_total_by_type =
            make_counter_vec("tx_executed_total", "Successfully committed transactions by tx_type");
        labeled_registry
            .register(Box::new(tx_executed_total_by_type.clone()))
            .expect("metric registration must succeed");

        let tx_failed_total = make_counter("tx_failed_total", "Failed transactions");
        registry
            .register(Box::new(tx_failed_total.clone()))
            .expect("metric registration must succeed");
        let tx_failed_total_by_type =
            make_counter_vec("tx_failed_total", "Failed transactions by tx_type");
        labeled_registry
            .register(Box::new(tx_failed_total_by_type.clone()))
            .expect("metric registration must succeed");

        let tx_dropped_total = make_counter("tx_dropped_total", "Dropped transactions");
        registry
            .register(Box::new(tx_dropped_total.clone()))
            .expect("metric registration must succeed");
        let tx_dropped_by_reason = IntCounterVec::new(
            Opts::new("tx_dropped_total", "Dropped transactions by reason"),
            &["reason"],
        )
        .expect("counter vec opts must be valid");
        labeled_registry
            .register(Box::new(tx_dropped_by_reason.clone()))
            .expect("metric registration must succeed");

        let tx_expired_total = make_counter("tx_expired_total", "Expired transactions");
        registry
            .register(Box::new(tx_expired_total.clone()))
            .expect("metric registration must succeed");
        let tx_expired_total_by_type =
            make_counter_vec("tx_expired_total", "Expired transactions by tx_type");
        labeled_registry
            .register(Box::new(tx_expired_total_by_type.clone()))
            .expect("metric registration must succeed");

        let confirmation_timeout_rate = make_counter(
            "confirmation_timeout_rate",
            "Transactions that timed out before confirmation",
        );
        registry
            .register(Box::new(confirmation_timeout_rate.clone()))
            .expect("metric registration must succeed");

        let account_lock_conflict_rate =
            make_counter("account_lock_conflict_rate", "Account lock conflict occurrences");
        registry
            .register(Box::new(account_lock_conflict_rate.clone()))
            .expect("metric registration must succeed");

        let retry_due_to_account_in_use_rate = make_counter(
            "retry_due_to_account_in_use_rate",
            "Retries due to account-in-use contention",
        );
        registry
            .register(Box::new(retry_due_to_account_in_use_rate.clone()))
            .expect("metric registration must succeed");

        let fork_rate = make_counter("fork_rate", "Fork failure signal");
        registry
            .register(Box::new(fork_rate.clone()))
            .expect("metric registration must succeed");

        let duplicate_confirmed_blocks =
            make_counter("duplicate_confirmed_blocks", "Duplicate confirmed block events");
        registry
            .register(Box::new(duplicate_confirmed_blocks.clone()))
            .expect("metric registration must succeed");

        let mempool_size =
            Gauge::with_opts(Opts::new("mempool_size", "Current number of transactions in the retry pool"))
                .expect("gauge opts must be valid");
        registry
            .register(Box::new(mempool_size.clone()))
            .expect("metric registration must succeed");

        let avg_locked_accounts_per_tx =
            Gauge::with_opts(Opts::new("avg_locked_accounts_per_tx", "Average locked accounts per transaction"))
                .expect("gauge opts must be valid");
        registry
            .register(Box::new(avg_locked_accounts_per_tx.clone()))
            .expect("metric registration must succeed");

        let blockhash_fetch_latency =
            make_latency_histogram("blockhash_fetch_latency_seconds", "Blockhash fetch latency");
        registry
            .register(Box::new(blockhash_fetch_latency.clone()))
            .expect("metric registration must succeed");

        let blockhash_age_at_submit_slots = make_slots_histogram(
            "blockhash_age_at_submit_slots",
            "Blockhash age at submit time in slots",
            BLOCKHASH_AGE_BUCKETS_SLOTS,
        );
        registry
            .register(Box::new(blockhash_age_at_submit_slots.clone()))
            .expect("metric registration must succeed");

        let blockhash_remaining_validity_slots = make_slots_histogram(
            "blockhash_remaining_validity_slots",
            "Remaining blockhash validity at submit time in slots",
            BLOCKHASH_REMAINING_VALIDITY_BUCKETS_SLOTS,
        );
        registry
            .register(Box::new(blockhash_remaining_validity_slots.clone()))
            .expect("metric registration must succeed");

        let time_to_processed =
            make_latency_histogram("time_to_processed_seconds", "Time to processed commitment");
        registry
            .register(Box::new(time_to_processed.clone()))
            .expect("metric registration must succeed");

        let time_to_confirmed =
            make_latency_histogram("time_to_confirmed_seconds", "Time to confirmed commitment");
        registry
            .register(Box::new(time_to_confirmed.clone()))
            .expect("metric registration must succeed");

        let time_to_finalized =
            make_latency_histogram("time_to_finalized_seconds", "Time to finalized commitment");
        registry
            .register(Box::new(time_to_finalized.clone()))
            .expect("metric registration must succeed");

        let node_ingress_latency =
            make_latency_histogram("node_ingress_latency_seconds", "Node ingress latency");
        registry
            .register(Box::new(node_ingress_latency.clone()))
            .expect("metric registration must succeed");
        let node_ingress_latency_by_type =
            make_latency_histogram_vec("node_ingress_latency_seconds", "Node ingress latency by tx_type");
        labeled_registry
            .register(Box::new(node_ingress_latency_by_type.clone()))
            .expect("metric registration must succeed");

        let mempool_acceptance_latency = make_latency_histogram(
            "mempool_acceptance_latency_seconds",
            "Mempool acceptance latency",
        );
        registry
            .register(Box::new(mempool_acceptance_latency.clone()))
            .expect("metric registration must succeed");
        let mempool_acceptance_latency_by_type = make_latency_histogram_vec(
            "mempool_acceptance_latency_seconds",
            "Mempool acceptance latency by tx_type",
        );
        labeled_registry
            .register(Box::new(mempool_acceptance_latency_by_type.clone()))
            .expect("metric registration must succeed");

        let decision_response_latency = make_latency_histogram(
            "decision_response_latency_seconds",
            "Decision response latency",
        );
        registry
            .register(Box::new(decision_response_latency.clone()))
            .expect("metric registration must succeed");
        let decision_response_latency_by_type = make_latency_histogram_vec(
            "decision_response_latency_seconds",
            "Decision response latency by tx_type",
        );
        labeled_registry
            .register(Box::new(decision_response_latency_by_type.clone()))
            .expect("metric registration must succeed");

        let node_to_decision_response_latency = make_latency_histogram(
            "node_to_decision_response_latency_seconds",
            "Node to decision response latency",
        );
        registry
            .register(Box::new(node_to_decision_response_latency.clone()))
            .expect("metric registration must succeed");
        let node_to_decision_response_latency_by_type = make_latency_histogram_vec(
            "node_to_decision_response_latency_seconds",
            "Node to decision response latency by tx_type",
        );
        labeled_registry
            .register(Box::new(node_to_decision_response_latency_by_type.clone()))
            .expect("metric registration must succeed");

        let tx_execution_in_block_latency = make_latency_histogram(
            "tx_execution_in_block_latency_seconds",
            "Execution in block latency",
        );
        registry
            .register(Box::new(tx_execution_in_block_latency.clone()))
            .expect("metric registration must succeed");

        let block_production_latency = make_latency_histogram(
            "block_production_latency_seconds",
            "Block production latency",
        );
        registry
            .register(Box::new(block_production_latency.clone()))
            .expect("metric registration must succeed");

        let state_update_notification_latency = make_latency_histogram(
            "state_update_notification_latency_seconds",
            "State update notification latency",
        );
        registry
            .register(Box::new(state_update_notification_latency.clone()))
            .expect("metric registration must succeed");

        let acknowledge_latency =
            make_latency_histogram("acknowledge_latency_seconds", "Acknowledge latency");
        registry
            .register(Box::new(acknowledge_latency.clone()))
            .expect("metric registration must succeed");
        let acknowledge_latency_by_type =
            make_latency_histogram_vec("acknowledge_latency_seconds", "Acknowledge latency by tx_type");
        labeled_registry
            .register(Box::new(acknowledge_latency_by_type.clone()))
            .expect("metric registration must succeed");

        Self {
            registry,
            labeled_registry,
            tx_accepted_total,
            tx_accepted_total_by_type,
            tx_executed_total,
            tx_executed_total_by_type,
            tx_failed_total,
            tx_failed_total_by_type,
            tx_dropped_total,
            tx_dropped_by_reason,
            tx_expired_total,
            tx_expired_total_by_type,
            confirmation_timeout_rate,
            account_lock_conflict_rate,
            retry_due_to_account_in_use_rate,
            fork_rate,
            duplicate_confirmed_blocks,
            mempool_size,
            avg_locked_accounts_per_tx,
            locked_accounts_total: AtomicU64::new(0),
            locked_accounts_samples: AtomicU64::new(0),
            blockhash_fetch_latency,
            blockhash_age_at_submit_slots,
            blockhash_remaining_validity_slots,
            time_to_processed,
            time_to_confirmed,
            time_to_finalized,
            node_ingress_latency,
            node_ingress_latency_by_type,
            mempool_acceptance_latency,
            mempool_acceptance_latency_by_type,
            decision_response_latency,
            decision_response_latency_by_type,
            node_to_decision_response_latency,
            node_to_decision_response_latency_by_type,
            tx_execution_in_block_latency,
            block_production_latency,
            state_update_notification_latency,
            acknowledge_latency,
            acknowledge_latency_by_type,
            tx_type_known: Mutex::new(HashSet::new()),
        }
    }

    fn bounded_tx_type(&self, tx_type: &str) -> String {
        let tx_type = normalize_tx_type(tx_type);
        let mut known = self.tx_type_known.lock().unwrap();
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

static CUSTOM_METRICS: LazyLock<CustomMetrics> = LazyLock::new(CustomMetrics::new);
static TX_ACCEPTANCE_TIMES: LazyLock<Mutex<HashMap<Signature, Instant>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

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

/// Pre-initialize all tx_type label series so they appear in Prometheus
/// with value 0 even before any matching transaction arrives.
pub fn init_tx_type_series(labels: &[&str]) {
    let m = &*CUSTOM_METRICS;
    for label in labels {
        let label = &*m.bounded_tx_type(label);
        // Touch each counter/histogram vec so the series is created
        m.tx_accepted_total_by_type.with_label_values(&[label]);
        m.tx_executed_total_by_type.with_label_values(&[label]);
        m.tx_failed_total_by_type.with_label_values(&[label]);
        m.tx_expired_total_by_type.with_label_values(&[label]);
        m.node_ingress_latency_by_type.with_label_values(&[label]);
        m.mempool_acceptance_latency_by_type.with_label_values(&[label]);
        m.decision_response_latency_by_type.with_label_values(&[label]);
        m.node_to_decision_response_latency_by_type.with_label_values(&[label]);
        m.acknowledge_latency_by_type.with_label_values(&[label]);
    }

    // Pre-initialize drop reason series
    let drop_reasons = [
        "without_parsing",
        "parsing_and_sanitization",
        "lock_validation",
        "compute_budget",
        "age",
        "already_processed",
        "fee_payer",
        "capacity",
        "retry_pool_full",
        "retry_overflow",
        "clean",
        "clear",
    ];
    for reason in &drop_reasons {
        m.tx_dropped_by_reason.with_label_values(&[reason]);
    }
}

pub fn inc_tx_accepted_total(value: u64) {
    CUSTOM_METRICS.tx_accepted_total.inc_by(value);
}

pub fn inc_tx_accepted_total_with_type(value: u64, tx_type: &str) {
    let tx_type = CUSTOM_METRICS.bounded_tx_type(tx_type);
    CUSTOM_METRICS
        .tx_accepted_total_by_type
        .with_label_values(&[tx_type.as_str()])
        .inc_by(value);
}

pub fn inc_tx_executed_total(value: u64) {
    CUSTOM_METRICS.tx_executed_total.inc_by(value);
}

pub fn inc_tx_executed_total_with_type(value: u64, tx_type: &str) {
    let tx_type = CUSTOM_METRICS.bounded_tx_type(tx_type);
    CUSTOM_METRICS
        .tx_executed_total_by_type
        .with_label_values(&[tx_type.as_str()])
        .inc_by(value);
}

pub fn inc_tx_failed_total(value: u64) {
    CUSTOM_METRICS.tx_failed_total.inc_by(value);
}

pub fn inc_tx_failed_total_with_type(value: u64, tx_type: &str) {
    let tx_type = CUSTOM_METRICS.bounded_tx_type(tx_type);
    CUSTOM_METRICS
        .tx_failed_total_by_type
        .with_label_values(&[tx_type.as_str()])
        .inc_by(value);
}

pub fn inc_tx_dropped_total(value: u64) {
    CUSTOM_METRICS.tx_dropped_total.inc_by(value);
}

pub fn inc_tx_dropped_with_reason(value: u64, reason: &str) {
    CUSTOM_METRICS
        .tx_dropped_by_reason
        .with_label_values(&[reason])
        .inc_by(value);
}

pub fn inc_tx_expired_total(value: u64) {
    CUSTOM_METRICS.tx_expired_total.inc_by(value);
}

pub fn inc_tx_expired_total_with_type(value: u64, tx_type: &str) {
    let tx_type = CUSTOM_METRICS.bounded_tx_type(tx_type);
    CUSTOM_METRICS
        .tx_expired_total_by_type
        .with_label_values(&[tx_type.as_str()])
        .inc_by(value);
}

pub fn inc_confirmation_timeout_rate(value: u64) {
    CUSTOM_METRICS.confirmation_timeout_rate.inc_by(value);
}

pub fn inc_account_lock_conflict_rate(value: u64) {
    CUSTOM_METRICS.account_lock_conflict_rate.inc_by(value);
}

pub fn inc_retry_due_to_account_in_use_rate(value: u64) {
    CUSTOM_METRICS.retry_due_to_account_in_use_rate.inc_by(value);
}

pub fn inc_fork_rate(value: u64) {
    CUSTOM_METRICS.fork_rate.inc_by(value);
}

pub fn inc_duplicate_confirmed_blocks(value: u64) {
    CUSTOM_METRICS.duplicate_confirmed_blocks.inc_by(value);
}

pub fn set_mempool_size(size: u64) {
    CUSTOM_METRICS.mempool_size.set(size as f64);
}

pub fn observe_avg_locked_accounts_per_tx(locked_accounts: u64) {
    CUSTOM_METRICS
        .locked_accounts_total
        .fetch_add(locked_accounts, Ordering::Relaxed);
    let samples = CUSTOM_METRICS
        .locked_accounts_samples
        .fetch_add(1, Ordering::Relaxed)
        + 1;
    let total = CUSTOM_METRICS.locked_accounts_total.load(Ordering::Relaxed);
    let avg = if samples == 0 {
        0.0
    } else {
        total as f64 / samples as f64
    };
    CUSTOM_METRICS.avg_locked_accounts_per_tx.set(avg);
}

pub fn observe_blockhash_fetch_latency_us(value_us: u64) {
    CUSTOM_METRICS
        .blockhash_fetch_latency
        .observe(value_us as f64 / 1_000_000.0);
}

pub fn observe_blockhash_age_at_submit_slots(value_slots: u64) {
    CUSTOM_METRICS
        .blockhash_age_at_submit_slots
        .observe(value_slots as f64);
}

pub fn observe_blockhash_remaining_validity_slots(value_slots: u64) {
    CUSTOM_METRICS
        .blockhash_remaining_validity_slots
        .observe(value_slots as f64);
}

pub fn observe_time_to_processed_us(value_us: u64) {
    CUSTOM_METRICS
        .time_to_processed
        .observe(value_us as f64 / 1_000_000.0);
}

pub fn observe_time_to_confirmed_us(value_us: u64) {
    CUSTOM_METRICS
        .time_to_confirmed
        .observe(value_us as f64 / 1_000_000.0);
}

pub fn observe_time_to_finalized_us(value_us: u64) {
    CUSTOM_METRICS
        .time_to_finalized
        .observe(value_us as f64 / 1_000_000.0);
}

pub fn observe_node_ingress_latency_us(value_us: u64) {
    CUSTOM_METRICS
        .node_ingress_latency
        .observe(value_us as f64 / 1_000_000.0);
}

pub fn observe_node_ingress_latency_us_with_type(value_us: u64, tx_type: &str) {
    let tx_type = CUSTOM_METRICS.bounded_tx_type(tx_type);
    CUSTOM_METRICS
        .node_ingress_latency_by_type
        .with_label_values(&[tx_type.as_str()])
        .observe(value_us as f64 / 1_000_000.0);
}

pub fn observe_mempool_acceptance_latency_us(value_us: u64) {
    CUSTOM_METRICS
        .mempool_acceptance_latency
        .observe(value_us as f64 / 1_000_000.0);
}

pub fn observe_mempool_acceptance_latency_us_with_type(value_us: u64, tx_type: &str) {
    let tx_type = CUSTOM_METRICS.bounded_tx_type(tx_type);
    CUSTOM_METRICS
        .mempool_acceptance_latency_by_type
        .with_label_values(&[tx_type.as_str()])
        .observe(value_us as f64 / 1_000_000.0);
}

pub fn observe_decision_response_latency_us(value_us: u64) {
    CUSTOM_METRICS
        .decision_response_latency
        .observe(value_us as f64 / 1_000_000.0);
}

pub fn observe_decision_response_latency_us_with_type(value_us: u64, tx_type: &str) {
    let tx_type = CUSTOM_METRICS.bounded_tx_type(tx_type);
    CUSTOM_METRICS
        .decision_response_latency_by_type
        .with_label_values(&[tx_type.as_str()])
        .observe(value_us as f64 / 1_000_000.0);
}

pub fn observe_node_to_decision_response_latency_us(value_us: u64) {
    CUSTOM_METRICS
        .node_to_decision_response_latency
        .observe(value_us as f64 / 1_000_000.0);
}

pub fn observe_node_to_decision_response_latency_us_with_type(value_us: u64, tx_type: &str) {
    let tx_type = CUSTOM_METRICS.bounded_tx_type(tx_type);
    CUSTOM_METRICS
        .node_to_decision_response_latency_by_type
        .with_label_values(&[tx_type.as_str()])
        .observe(value_us as f64 / 1_000_000.0);
}

pub fn observe_tx_execution_in_block_latency_us(value_us: u64) {
    CUSTOM_METRICS
        .tx_execution_in_block_latency
        .observe(value_us as f64 / 1_000_000.0);
}

pub fn observe_block_production_latency_us(value_us: u64) {
    CUSTOM_METRICS
        .block_production_latency
        .observe(value_us as f64 / 1_000_000.0);
}

pub fn observe_state_update_notification_latency_us(value_us: u64) {
    CUSTOM_METRICS
        .state_update_notification_latency
        .observe(value_us as f64 / 1_000_000.0);
}

pub fn observe_acknowledge_latency_us(value_us: u64) {
    CUSTOM_METRICS
        .acknowledge_latency
        .observe(value_us as f64 / 1_000_000.0);
}

pub fn observe_acknowledge_latency_us_with_type(value_us: u64, tx_type: &str) {
    let tx_type = CUSTOM_METRICS.bounded_tx_type(tx_type);
    CUSTOM_METRICS
        .acknowledge_latency_by_type
        .with_label_values(&[tx_type.as_str()])
        .observe(value_us as f64 / 1_000_000.0);
}

pub fn render_prometheus() -> String {
    let encoder = TextEncoder::new();

    let mut out = Vec::new();
    encoder
        .encode(&CUSTOM_METRICS.registry.gather(), &mut out)
        .expect("prometheus encoding must succeed");

    let mut labeled_out = Vec::new();
    encoder
        .encode(&CUSTOM_METRICS.labeled_registry.gather(), &mut labeled_out)
        .expect("prometheus encoding must succeed");

    let out_text = String::from_utf8(out).expect("prometheus text must be valid utf-8");
    let labeled_text =
        String::from_utf8(labeled_out).expect("prometheus text must be valid utf-8");

    let mut seen_meta = HashSet::new();
    for line in out_text.lines() {
        if let Some(name) = line
            .strip_prefix("# HELP ")
            .or_else(|| line.strip_prefix("# TYPE "))
            .and_then(|meta| meta.split_whitespace().next())
        {
            seen_meta.insert(name.to_string());
        }
    }

    let mut merged = out_text;
    if !merged.is_empty() && !labeled_text.is_empty() && !merged.ends_with('\n') {
        merged.push('\n');
    }
    for line in labeled_text.lines() {
        let maybe_meta_name = line
            .strip_prefix("# HELP ")
            .or_else(|| line.strip_prefix("# TYPE "))
            .and_then(|meta| meta.split_whitespace().next());
        if let Some(name) = maybe_meta_name {
            if seen_meta.contains(name) {
                continue;
            }
            seen_meta.insert(name.to_string());
        }
        merged.push_str(line);
        merged.push('\n');
    }

    merged
}
