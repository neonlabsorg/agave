//! Process-wide test-only offset added to the `Clock` sysvar's
//! `unix_timestamp` field. Lets external tests advance the on-chain clock
//! without manipulating slot production, voting or leader scheduling.
//!
//! Two atomics are tracked:
//!   * `CURRENT_OFFSET` — set externally via the `parasol_setClockOffset`
//!     JSON-RPC method; the new value is applied to the next slot's Clock.
//!   * `LAST_APPLIED_OFFSET` — internal bookkeeping. Records the offset
//!     that was already baked into the prior slot's Clock sysvar account.
//!     `Bank::update_clock` subtracts this from the ancestor timestamp
//!     before the monotonic-floor check, so a shrinking offset doesn't
//!     get pinned by the previous slot's larger value, and a stable
//!     offset doesn't compound slot-over-slot.

use std::sync::atomic::{AtomicI64, Ordering};

static CURRENT_OFFSET: AtomicI64 = AtomicI64::new(0);
static LAST_APPLIED_OFFSET: AtomicI64 = AtomicI64::new(0);

pub fn set(seconds: i64) {
    CURRENT_OFFSET.store(seconds, Ordering::Relaxed);
}

pub fn get() -> i64 {
    CURRENT_OFFSET.load(Ordering::Relaxed)
}

pub fn last_applied() -> i64 {
    LAST_APPLIED_OFFSET.load(Ordering::Relaxed)
}

pub fn set_last_applied(seconds: i64) {
    LAST_APPLIED_OFFSET.store(seconds, Ordering::Relaxed);
}
