//! JSONL metrics lines + leak detection.

use std::fs::OpenOptions;
use std::io::Write;
use std::path::Path;

use serde::Serialize;

#[derive(Debug, Clone, Serialize)]
pub struct CycleLine {
    pub kind: &'static str,
    pub cycle: u64,
    pub ok: bool,
    pub wall_secs: f64,
    pub events_applied: u64,
    pub events_read: u64,
    pub gap_records: u64,
    pub publications: u64,
    pub seeks_executed: u64,
    pub seeks_issued: u64,
    pub seeks_final_answered: bool,
    pub priors_completed: u64,
    pub prior_skips: u64,
    pub expected_priors_accepted: u64,
    pub expected_prior_skips: u64,
    pub sessions: usize,
    pub re_set_source_sessions: usize,
    pub retention_ok: bool,
    pub current_ready: bool,
    pub rss_ceiling_ok: bool,
    pub vm_rss_kb: u64,
    pub vm_hwm_kb: u64,
    pub peak_rss_kb: u64,
    pub speed: f64,
    pub cycle_secs_cap: u64,
    pub panic: Option<String>,
    pub notes: Option<String>,
}

/// Mid-cycle wait-loop progress. `kind != "summary"` and never final —
/// after-soak-gates.sh only requires exactly one terminal summary record.
#[derive(Debug, Clone, Serialize)]
pub struct HeartbeatLine {
    pub kind: &'static str,
    pub cycle: u64,
    pub phase: &'static str,
    pub events_applied: u64,
    pub applied_ts: u64,
    pub vm_rss_kb: u64,
    pub secs_in_phase: f64,
}

#[derive(Debug, Clone, Serialize)]
pub struct SummaryLine {
    pub kind: &'static str,
    pub verdict: &'static str,
    pub cycles: u64,
    pub failures: u64,
    pub leak_suspects: u64,
    pub rss_ceiling_fails: u64,
    pub wall_secs: f64,
    pub peak_vm_rss_kb: u64,
    pub peak_vm_hwm_kb: u64,
    pub baseline_rss_kb: Option<u64>,
    pub leak_window: usize,
    pub leak_pct: f64,
    pub git_sha: String,
    pub git_dirty: Option<bool>,
    pub label: Option<String>,
    pub notes: Option<String>,
}

pub fn append_json_line(path: &Path, value: &impl Serialize) {
    let mut f = OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .unwrap_or_else(|e| panic!("m7-soak: open {}: {e}", path.display()));
    let line = serde_json::to_string(value).unwrap_or_else(|e| panic!("m7-soak: serialize: {e}"));
    writeln!(f, "{line}").unwrap_or_else(|e| panic!("m7-soak: write: {e}"));
    f.flush()
        .unwrap_or_else(|e| panic!("m7-soak: flush {}: {e}", path.display()));
}

/// Leak math over an in-cycle peak-RSS series (one sample per completed cycle).
///
/// Suspect when the last `window` samples show a **sustained** upward trend:
/// 1. net climb `(last − first) / baseline × 100 ≥ leak_pct`
/// 2. after a 3-point median smooth (kills single-sample sawtooth dips / plateaus
///    stay flat), consecutive steps are non-decreasing on ≥ 90% of pairs
///
/// Hard oscillation (large alternating spikes) fails the smoothed monotonicity
/// check and does not count as a leak. Baseline = cycle-3 in-cycle peak RSS.
pub fn leak_suspect(series: &[u64], baseline: u64, window: usize, leak_pct: f64) -> bool {
    if series.len() < window || baseline == 0 || window < 2 {
        return false;
    }
    let w = &series[series.len() - window..];
    let first = w[0];
    let last = w[w.len() - 1];
    if last <= first {
        return false;
    }
    let climb_pct = (last - first) as f64 / baseline as f64 * 100.0;
    if climb_pct < leak_pct {
        return false;
    }
    let smooth = median3_smooth(w);
    let steps = smooth.len() - 1;
    if steps == 0 {
        return false;
    }
    let non_dec = smooth.windows(2).filter(|p| p[1] >= p[0]).count();
    // ≥ 90% non-decreasing consecutive pairs on the smoothed series.
    non_dec * 10 >= steps * 9
}

/// 3-point median filter; edges keep the original sample.
fn median3_smooth(w: &[u64]) -> Vec<u64> {
    let n = w.len();
    if n == 0 {
        return Vec::new();
    }
    if n == 1 {
        return vec![w[0]];
    }
    let mut out = Vec::with_capacity(n);
    out.push(w[0]);
    for i in 1..n - 1 {
        out.push(median3(w[i - 1], w[i], w[i + 1]));
    }
    out.push(w[n - 1]);
    out
}

fn median3(a: u64, b: u64, c: u64) -> u64 {
    // Middle of three without alloc.
    if (a <= b && b <= c) || (c <= b && b <= a) {
        b
    } else if (b <= a && a <= c) || (c <= a && a <= b) {
        a
    } else {
        c
    }
}

#[cfg(test)]
mod tests {
    use super::leak_suspect;

    #[test]
    fn short_series_or_zero_baseline_never_suspect() {
        assert!(!leak_suspect(&[1, 2, 3], 100, 10, 10.0));
        assert!(!leak_suspect(&[100, 200, 300, 400, 500], 0, 5, 10.0));
        assert!(!leak_suspect(&[], 100, 2, 10.0));
    }

    #[test]
    fn strict_climb_above_threshold_is_suspect() {
        // 10 → 20 over window, baseline 50 → 20% climb ≥ 10%.
        let s = [10, 11, 12, 13, 14, 15, 16, 17, 18, 20];
        assert!(leak_suspect(&s, 50, 10, 10.0));
    }

    #[test]
    fn plateau_mid_trend_still_suspect() {
        // Plateaus would defeat a strict-increasing detector.
        let s = [100, 110, 110, 110, 120, 130, 130, 140, 150, 160];
        assert!(leak_suspect(&s, 100, 10, 10.0));
    }

    #[test]
    fn minor_sawtooth_still_suspect() {
        // One-step dips must not trivially evade.
        let s = [100, 120, 115, 140, 135, 160, 155, 180, 175, 200];
        assert!(leak_suspect(&s, 100, 10, 10.0));
    }

    #[test]
    fn flat_series_not_suspect() {
        let s = [200u64; 10];
        assert!(!leak_suspect(&s, 200, 10, 10.0));
    }

    #[test]
    fn climb_below_threshold_not_suspect() {
        // +5 on baseline 200 = 2.5% < 10%.
        let s = [200, 201, 202, 203, 204, 205];
        assert!(!leak_suspect(&s, 200, 6, 10.0));
    }

    #[test]
    fn end_below_start_not_suspect() {
        let s = [200, 250, 300, 280, 150];
        assert!(!leak_suspect(&s, 200, 5, 10.0));
    }

    #[test]
    fn chaotic_sawtooth_without_sustained_trend_not_suspect() {
        // Net climb exists but pairwise order is weak (oscillates hard).
        let s = [100, 200, 100, 200, 100, 200, 100, 200, 100, 220];
        assert!(!leak_suspect(&s, 100, 10, 10.0));
    }

    #[test]
    fn uses_trailing_window_only() {
        // Early noise ignored; trailing window is a clean climb.
        let mut s = vec![500u64, 100, 100, 100];
        s.extend_from_slice(&[100, 120, 140, 160, 180, 200]);
        assert!(leak_suspect(&s, 100, 6, 10.0));
    }
}
