use {
    solana_clock::Slot,
    solana_metrics::custom_metrics,
    solana_signature::Signature,
    std::{
        collections::HashMap,
        sync::{
            atomic::{AtomicBool, Ordering},
            Arc, LazyLock, Mutex,
        },
        thread::{self, Builder, JoinHandle},
        time::{Duration, Instant},
    },
};

const CONFIRMATION_TIMEOUT: Duration = Duration::from_secs(60);
const SWEEP_INTERVAL: Duration = Duration::from_secs(5);

#[derive(Clone, Copy)]
struct SignatureLifecycle {
    t1: Instant,
    t3_proxy: Instant,
    t3_exact: Option<Instant>,
    processed: bool,
    confirmed: bool,
    finalized: bool,
    slot: Option<Slot>,
}

#[derive(Default)]
struct SignatureMetricsTracker {
    by_signature: HashMap<Signature, SignatureLifecycle>,
    by_slot: HashMap<Slot, Vec<Signature>>,
    last_sweep: Option<Instant>,
}

impl SignatureMetricsTracker {
    fn register(&mut self, signature: Signature, t1: Instant, t3_proxy: Instant) {
        self.by_signature.insert(
            signature,
            SignatureLifecycle {
                t1,
                t3_proxy,
                t3_exact: None,
                processed: false,
                confirmed: false,
                finalized: false,
                slot: None,
            },
        );
    }

    fn effective_t3(signature: &Signature, state: &mut SignatureLifecycle) -> Instant {
        if state.t3_exact.is_none() {
            state.t3_exact = custom_metrics::get_tx_acceptance_time(signature);
        }
        state.t3_exact.unwrap_or(state.t3_proxy)
    }

    fn observe_processed_once(state: &mut SignatureLifecycle, now: Instant) {
        if !state.processed {
            custom_metrics::observe_time_to_processed_us(
                now.saturating_duration_since(state.t1).as_micros() as u64,
            );
            state.processed = true;
        }
    }

    fn remove_signature_from_slot(&mut self, slot: Slot, signature: &Signature) {
        if let Some(signatures) = self.by_slot.get_mut(&slot) {
            signatures.retain(|entry| entry != signature);
            if signatures.is_empty() {
                self.by_slot.remove(&slot);
            }
        }
    }

    fn mark_processed(&mut self, signature: &Signature, now: Instant) {
        if let Some(state) = self.by_signature.get_mut(signature) {
            Self::observe_processed_once(state, now);
        }
    }

    fn mark_processed_with_slot(&mut self, signature: &Signature, slot: Slot, now: Instant) {
        if let Some(state) = self.by_signature.get_mut(signature) {
            Self::observe_processed_once(state, now);
            if state.slot.is_none() {
                state.slot = Some(slot);
                self.by_slot
                    .entry(slot)
                    .or_default()
                    .push(signature.to_owned());
                let t3 = Self::effective_t3(signature, state);
                custom_metrics::observe_tx_execution_in_block_latency_us(
                    now.saturating_duration_since(t3).as_micros() as u64,
                );
            }
        }
    }

    fn mark_confirmed(&mut self, signature: &Signature, now: Instant) {
        if let Some(state) = self.by_signature.get_mut(signature) {
            if !state.confirmed {
                custom_metrics::observe_time_to_confirmed_us(
                    now.saturating_duration_since(state.t1).as_micros() as u64,
                );
                state.confirmed = true;
            }
        }
    }

    fn mark_confirmed_up_to_slot(&mut self, slot: Slot, now: Instant) {
        for state in self.by_signature.values_mut() {
            if state.slot.is_some_and(|signature_slot| signature_slot <= slot) && !state.confirmed {
                custom_metrics::observe_time_to_confirmed_us(
                    now.saturating_duration_since(state.t1).as_micros() as u64,
                );
                state.confirmed = true;
            }
        }
    }

    fn mark_finalized(&mut self, signature: &Signature, now: Instant) {
        if let Some(state) = self.by_signature.remove(signature) {
            if let Some(slot) = state.slot {
                self.remove_signature_from_slot(slot, signature);
            }
            custom_metrics::clear_tx_acceptance_time(signature);
            if !state.finalized {
                custom_metrics::observe_time_to_finalized_us(
                    now.saturating_duration_since(state.t1).as_micros() as u64,
                );
            }
        }
    }

    fn mark_finalized_up_to_slot(&mut self, slot: Slot, now: Instant) {
        let mut finalized_signatures = Vec::new();
        for (signature, state) in &self.by_signature {
            if state.slot.is_some_and(|signature_slot| signature_slot <= slot) {
                finalized_signatures.push(signature.to_owned());
            }
        }

        for signature in finalized_signatures {
            self.mark_finalized(&signature, now);
        }
    }

    fn mark_block_produced(&mut self, slot: Slot, now: Instant) {
        let Some(signatures) = self.by_slot.remove(&slot) else {
            return;
        };
        for signature in signatures {
            if let Some(state) = self.by_signature.get_mut(&signature) {
                let t3 = Self::effective_t3(&signature, state);
                custom_metrics::observe_block_production_latency_us(
                    now.saturating_duration_since(t3).as_micros() as u64,
                );
            }
        }
    }

    fn maybe_sweep_timeouts(&mut self, now: Instant) {
        if self
            .last_sweep
            .is_some_and(|last| now.saturating_duration_since(last) < SWEEP_INTERVAL)
        {
            return;
        }
        self.last_sweep = Some(now);

        let mut timed_out = 0_u64;
        let mut expired_signatures = Vec::new();
        for (signature, state) in &self.by_signature {
            if !state.confirmed
                && now.saturating_duration_since(state.t1) >= CONFIRMATION_TIMEOUT
            {
                timed_out += 1;
                expired_signatures.push((signature.to_owned(), state.slot));
            }
        }
        for (signature, maybe_slot) in expired_signatures {
            self.by_signature.remove(&signature);
            if let Some(slot) = maybe_slot {
                self.remove_signature_from_slot(slot, &signature);
            }
            custom_metrics::clear_tx_acceptance_time(&signature);
        }
        if timed_out > 0 {
            custom_metrics::inc_confirmation_timeout_rate(timed_out);
        }
    }
}

static TRACKER: LazyLock<Mutex<SignatureMetricsTracker>> =
    LazyLock::new(|| Mutex::new(SignatureMetricsTracker::default()));

// Fast-path gate: `register_signature` flips this to `true` on first use. Until
// then every read-side method (`mark_*`, `is_tracked`) returns without taking
// the global Mutex, so a not-yet-wired-up tracker does not produce contention
// on the banking_stage / RPC notification hot paths.
static TRACKER_ACTIVE: AtomicBool = AtomicBool::new(false);

fn tracker_active() -> bool {
    TRACKER_ACTIVE.load(Ordering::Relaxed)
}

pub fn register_signature(signature: Signature, t1: Instant, t3_proxy: Instant) {
    if let Ok(mut tracker) = TRACKER.lock() {
        TRACKER_ACTIVE.store(true, Ordering::Relaxed);
        tracker.maybe_sweep_timeouts(Instant::now());
        tracker.register(signature, t1, t3_proxy);
    }
}

pub fn mark_processed(signature: &Signature) {
    if !tracker_active() {
        return;
    }
    if let Ok(mut tracker) = TRACKER.lock() {
        tracker.mark_processed(signature, Instant::now());
    }
}

pub fn mark_processed_with_slot(signature: &Signature, slot: Slot) {
    if !tracker_active() {
        return;
    }
    if let Ok(mut tracker) = TRACKER.lock() {
        tracker.mark_processed_with_slot(signature, slot, Instant::now());
    }
}

pub fn mark_confirmed(signature: &Signature) {
    if !tracker_active() {
        return;
    }
    if let Ok(mut tracker) = TRACKER.lock() {
        tracker.mark_confirmed(signature, Instant::now());
    }
}

pub fn mark_confirmed_up_to_slot(slot: Slot) {
    if !tracker_active() {
        return;
    }
    if let Ok(mut tracker) = TRACKER.lock() {
        tracker.mark_confirmed_up_to_slot(slot, Instant::now());
    }
}

pub fn mark_finalized(signature: &Signature) {
    if !tracker_active() {
        return;
    }
    if let Ok(mut tracker) = TRACKER.lock() {
        tracker.mark_finalized(signature, Instant::now());
    }
}

pub fn mark_finalized_up_to_slot(slot: Slot) {
    if !tracker_active() {
        return;
    }
    if let Ok(mut tracker) = TRACKER.lock() {
        tracker.mark_finalized_up_to_slot(slot, Instant::now());
    }
}

pub fn mark_block_produced(slot: Slot) {
    if !tracker_active() {
        return;
    }
    if let Ok(mut tracker) = TRACKER.lock() {
        tracker.mark_block_produced(slot, Instant::now());
    }
}

pub fn is_tracked(signature: &Signature) -> bool {
    if !tracker_active() {
        return false;
    }
    if let Ok(tracker) = TRACKER.lock() {
        tracker.by_signature.contains_key(signature)
    } else {
        false
    }
}

pub fn start_timeout_sweeper(exit: Arc<AtomicBool>) -> JoinHandle<()> {
    Builder::new()
        .name("solSigMetricsSweep".to_string())
        .spawn(move || {
            while !exit.load(Ordering::Relaxed) {
                if tracker_active() {
                    if let Ok(mut tracker) = TRACKER.lock() {
                        tracker.maybe_sweep_timeouts(Instant::now());
                    }
                }
                thread::sleep(SWEEP_INTERVAL);
            }
        })
        .expect("signature timeout sweeper thread must start")
}
