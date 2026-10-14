//! Reconnect backoff (spec art_d3wTgg6t §5): pure math, unit-testable
//! without a clock or randomness — the caller injects the jitter draw.

use std::time::Duration;

/// Base delay for the first retry (spec: 0.5 s × 2^attempt).
const BASE_MILLIS: u64 = 500;
/// Upper bound on any returned delay (spec: cap 30 s).
const CAP_MILLIS: u64 = 30_000;
/// Jitter spreads the delay ±20% around the exponential curve.
const JITTER_SPREAD: f32 = 0.2;

/// Exponential reconnect backoff: `0.5 s × 2^attempt`, capped at 30 s, then
/// spread ±20% by `jitter` (clamped to [0.0, 1.0]; 0.0 → 80% of the capped
/// base, 0.5 → exactly the base, 1.0 → up to the cap itself, so the result
/// never exceeds 30 s).
///
/// Pure and clock-free: tests pin every bound.
pub fn backoff(attempt: u32, jitter: f32) -> Duration {
    // Guard the shift, then saturate — attempt is unbounded from the outside.
    let exponential = BASE_MILLIS.saturating_mul(1u64 << attempt.min(63));
    let capped = exponential.min(CAP_MILLIS);
    let spread = 1.0 + (jitter.clamp(0.0, 1.0) * 2.0 - 1.0) * JITTER_SPREAD;
    let millis = (capped as f32 * spread) as u64;
    Duration::from_millis(millis.min(CAP_MILLIS))
}
