//! Backoff math tests (spec art_d3wTgg6t §5): 0.5 s × 2^attempt, 30 s cap,
//! ±20% jitter bounds.

use std::time::Duration;

use chat_core::backoff;

#[test]
fn attempt_zero_is_half_a_second() {
    assert_eq!(backoff(0, 0.5), Duration::from_millis(500));
}

#[test]
fn jitter_bounds_hold() {
    assert_eq!(backoff(0, 0.0), Duration::from_millis(400));
    assert_eq!(backoff(0, 1.0), Duration::from_millis(600));
}

#[test]
fn caps_at_thirty_seconds() {
    // 0.5 s × 2^10 ≈ 512 s → pinned to the 30 s cap on both sides.
    assert_eq!(backoff(10, 0.5), Duration::from_millis(30_000));
    assert_eq!(backoff(10, 1.0), Duration::from_millis(30_000));
    assert_eq!(backoff(10, 0.0), Duration::from_millis(24_000));
}

#[test]
fn bounded_for_all_attempts_and_jitters() {
    for attempt in [0u32, 1, 5, 10, 20, 63, u32::MAX] {
        for jitter in [0.0f32, 0.25, 0.5, 0.75, 1.0] {
            let delay = backoff(attempt, jitter);
            assert!(
                delay >= Duration::from_millis(400),
                "attempt {attempt} jitter {jitter}: {delay:?} below the ±20% floor"
            );
            assert!(
                delay <= Duration::from_millis(30_000),
                "attempt {attempt} jitter {jitter}: {delay:?} above the 30 s cap"
            );
        }
    }
}

#[test]
fn non_decreasing_in_attempt_until_cap() {
    let mut previous = Duration::ZERO;
    for attempt in 0..=8u32 {
        let delay = backoff(attempt, 0.5);
        assert!(delay >= previous, "backoff shrank at attempt {attempt}");
        previous = delay;
    }
    assert_eq!(previous, Duration::from_millis(30_000));
}
