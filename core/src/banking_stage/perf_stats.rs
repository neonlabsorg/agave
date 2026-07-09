//! Ad-hoc instrumentation for the scheduler comparison runs (in-proc
//! hot-pinned vs external parasol-scheduler). Everything here is cumulative
//! since process start and is periodically dumped to the validator log with a
//! grep-able `PERF_` prefix:
//!
//! - `PERF_HIST pre_cost` / `PERF_HIST actual_cost`: histograms over
//!   committed non-vote transactions. `pre_cost` is the cost-model estimate
//!   reserved in the cost tracker before execution; `actual_cost` is the same
//!   cost with the execution component replaced by the actually consumed CUs
//!   from the commit confirmation. Diverging shapes between the two runs
//!   prove (or refute) a weight-based selection skew.
//! - `PERF_TRANSLATE`: wall time the external worker spends re-parsing/
//!   sanitizing/resolving transactions per send (`translate_transaction_batch`).
//!   Zero in the in-proc run.
//! - `PERF_INGEST_PARSE`: the equivalent parse/sanitize/resolve work the
//!   in-proc path does once at ingest (`translate_to_runtime_view`). Zero in
//!   the external run.
//! - `PERF_SLOT`: per-slot committed tx/CU line, emitted on slot roll. This
//!   is the in-proc counterpart of the external scheduler's own slot line
//!   (and in the external run doubles as a validator-side cross-check of it).
//!
//! The recording paths are one relaxed atomic add per event; the report is
//! rate-limited to one dump per 5s and runs on whichever worker thread
//! happens to trip the interval.

use {
    log::info,
    std::{
        fmt::Write,
        sync::{
            atomic::{AtomicU64, Ordering},
            Mutex, OnceLock,
        },
        time::{Duration, Instant},
    },
};

const BUCKET_WIDTH_CU: u64 = 20_000;
/// 70 linear buckets cover 0..1.4M CU (the max per-tx compute limit); the
/// last bucket collects overflow.
const NUM_BUCKETS: usize = 71;
const REPORT_INTERVAL: Duration = Duration::from_secs(5);

pub(crate) struct CuHistogram {
    buckets: [AtomicU64; NUM_BUCKETS],
    total: AtomicU64,
    sum_cu: AtomicU64,
}

impl CuHistogram {
    const fn new() -> Self {
        #[allow(clippy::declare_interior_mutable_const)]
        const ZERO: AtomicU64 = AtomicU64::new(0);
        Self {
            buckets: [ZERO; NUM_BUCKETS],
            total: ZERO,
            sum_cu: ZERO,
        }
    }

    pub(crate) fn record(&self, cu: u64) {
        let index = usize::min((cu / BUCKET_WIDTH_CU) as usize, NUM_BUCKETS - 1);
        self.buckets[index].fetch_add(1, Ordering::Relaxed);
        self.total.fetch_add(1, Ordering::Relaxed);
        self.sum_cu.fetch_add(cu, Ordering::Relaxed);
    }

    fn dump(&self, name: &str) {
        let mut parts = String::new();
        for (index, bucket) in self.buckets.iter().enumerate() {
            let count = bucket.load(Ordering::Relaxed);
            if count != 0 {
                let _ = write!(
                    parts,
                    " {}k:{}",
                    index as u64 * BUCKET_WIDTH_CU / 1_000,
                    count
                );
            }
        }
        info!(
            "PERF_HIST {name} total={} sum_cu={} buckets:{parts}",
            self.total.load(Ordering::Relaxed),
            self.sum_cu.load(Ordering::Relaxed),
        );
    }
}

pub(crate) static PRE_COST_HIST: CuHistogram = CuHistogram::new();
pub(crate) static ACTUAL_COST_HIST: CuHistogram = CuHistogram::new();

static TRANSLATE_NANOS: AtomicU64 = AtomicU64::new(0);
static TRANSLATE_TXS: AtomicU64 = AtomicU64::new(0);
static TRANSLATE_BATCHES: AtomicU64 = AtomicU64::new(0);

static INGEST_PARSE_NANOS: AtomicU64 = AtomicU64::new(0);
static INGEST_PARSE_TXS: AtomicU64 = AtomicU64::new(0);

/// External-worker path: time spent in `translate_transaction_batch` for one
/// batch (re-parse + sanitize + resolve on every send, including re-sends).
pub(crate) fn record_translate(elapsed: Duration, num_txs: usize) {
    TRANSLATE_NANOS.fetch_add(elapsed.as_nanos() as u64, Ordering::Relaxed);
    TRANSLATE_TXS.fetch_add(num_txs as u64, Ordering::Relaxed);
    TRANSLATE_BATCHES.fetch_add(1, Ordering::Relaxed);
}

/// In-proc ingest path: time spent in `translate_to_runtime_view` for one
/// packet (parse + sanitize + resolve, done once per transaction lifetime).
pub(crate) fn record_ingest_parse(elapsed: Duration) {
    INGEST_PARSE_NANOS.fetch_add(elapsed.as_nanos() as u64, Ordering::Relaxed);
    INGEST_PARSE_TXS.fetch_add(1, Ordering::Relaxed);
}

struct SlotAgg {
    slot: u64,
    txs: u64,
    actual_cu: u64,
}

static SLOT_AGG: Mutex<SlotAgg> = Mutex::new(SlotAgg {
    slot: 0,
    txs: 0,
    actual_cu: 0,
});

/// Accumulate committed non-vote (txs, actual CU) per slot; emit the previous
/// slot's line when the slot rolls. Workers race on the lock, but commits are
/// batch-grained so the contention is negligible.
pub(crate) fn record_committed_batch(slot: u64, num_txs: u64, actual_cu: u64) {
    let mut agg = SLOT_AGG.lock().unwrap();
    if agg.slot != slot {
        if agg.txs > 0 {
            info!(
                "PERF_SLOT slot={} executed_txs={} actual_cu={}",
                agg.slot, agg.txs, agg.actual_cu
            );
        }
        agg.slot = slot;
        agg.txs = 0;
        agg.actual_cu = 0;
    }
    agg.txs += num_txs;
    agg.actual_cu += actual_cu;
}

static LAST_REPORT_MICROS: AtomicU64 = AtomicU64::new(0);
static START: OnceLock<Instant> = OnceLock::new();

/// Rate-limited cumulative dump; call from any hot path, it exits in a couple
/// of atomic loads when the interval has not elapsed.
pub(crate) fn maybe_report() {
    let start = START.get_or_init(Instant::now);
    let now_micros = start.elapsed().as_micros() as u64;
    let last = LAST_REPORT_MICROS.load(Ordering::Relaxed);
    if now_micros.saturating_sub(last) < REPORT_INTERVAL.as_micros() as u64 {
        return;
    }
    if LAST_REPORT_MICROS
        .compare_exchange(last, now_micros, Ordering::Relaxed, Ordering::Relaxed)
        .is_err()
    {
        // Another thread is reporting this interval.
        return;
    }

    PRE_COST_HIST.dump("pre_cost");
    ACTUAL_COST_HIST.dump("actual_cost");
    info!(
        "PERF_TRANSLATE batches={} txs={} nanos={}",
        TRANSLATE_BATCHES.load(Ordering::Relaxed),
        TRANSLATE_TXS.load(Ordering::Relaxed),
        TRANSLATE_NANOS.load(Ordering::Relaxed),
    );
    info!(
        "PERF_INGEST_PARSE txs={} nanos={}",
        INGEST_PARSE_TXS.load(Ordering::Relaxed),
        INGEST_PARSE_NANOS.load(Ordering::Relaxed),
    );
}
