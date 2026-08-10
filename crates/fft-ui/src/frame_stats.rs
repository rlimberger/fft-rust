//! Frame-time measurement harness: log-scale histogram of per-frame durations against a
//! deadline derived from the display refresh interval, missed-deadline counter, and a
//! p50/p99/max summary. Pure — no GPUI types, so the math is unit-testable headless.

use std::fmt;
use std::time::Duration;

/// Sub-bucket resolution: 2^5 = 32 log-linear sub-buckets per octave (~3.1 % worst-case
/// relative error on reported percentiles).
const SUB_BITS: u32 = 5;
const SUB: u64 = 1 << SUB_BITS;
/// Covers the full `u64` nanosecond range: one linear region + 59 octaves × 32 sub-buckets.
const NUM_BUCKETS: usize = ((64 - SUB_BITS as usize) + 1) << SUB_BITS;

/// Log-scale histogram bucket index for a value in nanoseconds.
fn bucket_index(ns: u64) -> usize {
    if ns < SUB {
        return ns as usize;
    }
    let msb = 63 - u64::from(ns.leading_zeros());
    let octave = msb - u64::from(SUB_BITS);
    let mantissa = (ns >> octave) & (SUB - 1);
    (((octave + 1) << SUB_BITS) + mantissa) as usize
}

/// Inclusive `(lower, upper)` nanosecond bounds of bucket `index`.
fn bucket_bounds(index: usize) -> (u64, u64) {
    let i = index as u64;
    if i < 2 * SUB {
        return (i, i);
    }
    let octave = (i >> SUB_BITS) - 1;
    let mantissa = i & (SUB - 1);
    let lower = (SUB + mantissa) << octave;
    let width = 1u64 << octave;
    // `(width - 1)` first: `lower + width` is 2^64 in the top bucket.
    (lower, lower + (width - 1))
}

/// Records per-frame durations against a fixed deadline.
pub struct FrameStats {
    deadline: Duration,
    buckets: Box<[u64; NUM_BUCKETS]>,
    frames: u64,
    missed: u64,
    max_ns: u64,
}

impl FrameStats {
    /// `deadline` is derived by the caller from the display refresh interval
    /// (a frame that takes longer than the deadline is a missed frame).
    pub fn new(deadline: Duration) -> Self {
        Self {
            deadline,
            buckets: Box::new([0; NUM_BUCKETS]),
            frames: 0,
            missed: 0,
            max_ns: 0,
        }
    }

    /// Record one frame's duration.
    pub fn record(&mut self, frame_time: Duration) {
        let ns = u64::try_from(frame_time.as_nanos()).unwrap_or(u64::MAX);
        self.buckets[bucket_index(ns)] += 1;
        self.frames += 1;
        self.max_ns = self.max_ns.max(ns);
        if frame_time > self.deadline {
            self.missed += 1;
        }
    }

    pub fn frames(&self) -> u64 {
        self.frames
    }

    pub fn missed(&self) -> u64 {
        self.missed
    }

    /// Upper bound of the histogram bucket containing the `p`-quantile (0.0 ≤ p ≤ 1.0).
    /// Panics if no frames were recorded or `p` is out of range.
    pub fn quantile(&self, p: f64) -> Duration {
        assert!((0.0..=1.0).contains(&p), "quantile out of range: {p}");
        assert!(self.frames > 0, "quantile of empty FrameStats");
        // Rank of the p-quantile frame, 1-based, ceiling — p=0.5 of 4 frames is frame 2.
        let rank = ((p * self.frames as f64).ceil() as u64).max(1);
        let mut seen = 0u64;
        for (i, &count) in self.buckets.iter().enumerate() {
            seen += count;
            if seen >= rank {
                return Duration::from_nanos(bucket_bounds(i).1);
            }
        }
        unreachable!("histogram counts diverged from frame count");
    }

    pub fn summary(&self) -> Summary {
        Summary {
            deadline: self.deadline,
            frames: self.frames,
            missed: self.missed,
            p50: self.quantile(0.50),
            p99: self.quantile(0.99),
            max: Duration::from_nanos(self.max_ns),
        }
    }
}

/// Point-in-time digest of a `FrameStats`.
#[derive(Clone, Copy, Debug)]
pub struct Summary {
    pub deadline: Duration,
    pub frames: u64,
    pub missed: u64,
    pub p50: Duration,
    pub p99: Duration,
    pub max: Duration,
}

impl fmt::Display for Summary {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "frames={} missed={} deadline={:.3}ms p50={:.3}ms p99={:.3}ms max={:.3}ms",
            self.frames,
            self.missed,
            self.deadline.as_secs_f64() * 1e3,
            self.p50.as_secs_f64() * 1e3,
            self.p99.as_secs_f64() * 1e3,
            self.max.as_secs_f64() * 1e3,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bucket_index_is_monotonic_and_contiguous() {
        // Every value maps to the same bucket as, or the one after, its predecessor.
        let mut prev = bucket_index(0);
        assert_eq!(prev, 0);
        for ns in 1..4096u64 {
            let i = bucket_index(ns);
            assert!(i == prev || i == prev + 1, "gap at {ns}: {prev} -> {i}");
            prev = i;
        }
        // Spot-check across octaves, up to u64::MAX.
        let mut prev = 0;
        for shift in 12..64 {
            let i = bucket_index(1u64 << shift);
            assert!(i > prev);
            prev = i;
        }
        assert!(bucket_index(u64::MAX) < NUM_BUCKETS);
    }

    #[test]
    fn bucket_bounds_roundtrip() {
        // Both edges of every reachable bucket map back to that bucket.
        for ns in [0, 1, 31, 32, 63, 64, 65, 1_000, 16_666_667, u64::MAX] {
            let i = bucket_index(ns);
            let (lo, hi) = bucket_bounds(i);
            assert!(lo <= ns && ns <= hi, "{ns} outside bucket {i} [{lo}, {hi}]");
            assert_eq!(bucket_index(lo), i);
            assert_eq!(bucket_index(hi), i);
        }
    }

    #[test]
    fn bucket_relative_error_is_bounded() {
        // Bucket width / lower bound ≤ 2^-SUB_BITS for all values past the linear region.
        for shift in 6..63 {
            let ns = (1u64 << shift) + 12345 % (1u64 << shift);
            let (lo, hi) = bucket_bounds(bucket_index(ns));
            assert!((hi - lo) as f64 / lo as f64 <= 1.0 / SUB as f64);
        }
    }

    #[test]
    fn missed_deadline_counting() {
        let deadline = Duration::from_millis(8);
        let mut stats = FrameStats::new(deadline);
        stats.record(Duration::from_millis(7));
        stats.record(deadline); // exactly on deadline is not a miss
        stats.record(Duration::from_millis(9));
        stats.record(Duration::from_millis(20));
        assert_eq!(stats.frames(), 4);
        assert_eq!(stats.missed(), 2);
    }

    #[test]
    fn quantiles_from_known_distribution() {
        let mut stats = FrameStats::new(Duration::from_millis(8));
        // 98 fast frames, 1 medium, 1 slow.
        for _ in 0..98 {
            stats.record(Duration::from_millis(1));
        }
        stats.record(Duration::from_millis(4));
        stats.record(Duration::from_millis(100));

        let s = stats.summary();
        assert_eq!(s.frames, 100);
        assert_eq!(s.missed, 1);
        assert_eq!(s.max, Duration::from_millis(100));

        // Reported quantile is the bucket upper bound: within +3.2 % of the true value.
        let one_ms = Duration::from_millis(1).as_nanos() as f64;
        let p50 = s.p50.as_nanos() as f64;
        assert!(p50 >= one_ms && p50 <= one_ms * (1.0 + 1.0 / SUB as f64));
        // p99 rank is frame 99 of 100 — the 4 ms frame.
        let four_ms = Duration::from_millis(4).as_nanos() as f64;
        let p99 = s.p99.as_nanos() as f64;
        assert!(p99 >= four_ms && p99 <= four_ms * (1.0 + 1.0 / SUB as f64));
        // p100 is the slow frame's bucket.
        assert!(stats.quantile(1.0) >= Duration::from_millis(100));
    }

    #[test]
    fn zero_and_extreme_durations_do_not_panic() {
        let mut stats = FrameStats::new(Duration::from_millis(8));
        stats.record(Duration::ZERO);
        stats.record(Duration::from_secs(u64::MAX / 2_000_000_000));
        assert_eq!(stats.frames(), 2);
        let s = stats.summary();
        assert_eq!(s.missed, 1);
        assert!(s.max > Duration::from_secs(1_000_000));
    }
}
