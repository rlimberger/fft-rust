//! Event-time pacing helpers shared by plain replay and sim-live.

use std::time::{Duration, Instant};

/// Per-slice apply budget (doctrine rule 4: time, never event counts).
pub const APPLY_BUDGET: Duration = Duration::from_millis(4);

/// Absolute wall-pin lag: `(applied_ts - head_ts) - (now - wall_at_head)`, ns.
pub(crate) fn head_lag_ns(
    head_ts: u64,
    wall_at_head: Instant,
    applied_ts: u64,
    now: Instant,
) -> i64 {
    let applied_delta = i128::from(applied_ts) - i128::from(head_ts);
    let wall_delta =
        i128::try_from(now.saturating_duration_since(wall_at_head).as_nanos()).unwrap_or(i128::MAX);
    let lag = applied_delta.saturating_sub(wall_delta);
    lag.clamp(i128::from(i64::MIN), i128::from(i64::MAX)) as i64
}

/// Wall-clock event-time head under an absolute origin pin.
pub(crate) fn wall_head_ts(head_ts: u64, wall_at_head: Instant, now: Instant) -> u64 {
    let elapsed = now.saturating_duration_since(wall_at_head);
    head_ts.saturating_add(elapsed.as_nanos() as u64)
}

/// True when `event_ts` is due under the absolute wall pin at 1×.
pub(crate) fn wall_pin_due(
    head_ts: u64,
    wall_at_head: Instant,
    event_ts: u64,
    now: Instant,
) -> bool {
    event_ts <= wall_head_ts(head_ts, wall_at_head, now)
}

/// Relative speed pacing due-check (scrubbed-back / plain replay).
pub(crate) fn speed_due(
    event_origin: u64,
    wall_origin: Instant,
    speed: f64,
    event_ts: u64,
    now: Instant,
) -> bool {
    let event_delta = event_ts.saturating_sub(event_origin);
    let due = wall_origin + Duration::from_nanos((event_delta as f64 / speed) as u64);
    now >= due
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn head_lag_preserves_backward_event_time() {
        let wall = Instant::now();
        let now = wall + Duration::from_nanos(25);
        assert_eq!(head_lag_ns(100, wall, 90, now), -35);
    }

    #[test]
    fn head_lag_saturates_instead_of_wrapping() {
        let wall = Instant::now();
        assert_eq!(head_lag_ns(0, wall, u64::MAX, wall), i64::MAX);
        assert_eq!(head_lag_ns(u64::MAX, wall, 0, wall), i64::MIN);
    }

    #[test]
    fn head_lag_uses_absolute_wall_origin() {
        let wall = Instant::now();
        let now = wall + Duration::from_nanos(40);
        assert_eq!(head_lag_ns(1_000, wall, 1_030, now), -10);
    }
}
