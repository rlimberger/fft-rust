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
    pub priors_completed: u64,
    pub prior_skips: u64,
    pub sessions: usize,
    pub re_set_source_sessions: usize,
    pub vm_rss_kb: u64,
    pub vm_hwm_kb: u64,
    pub peak_rss_kb: u64,
    pub speed: f64,
    pub cycle_secs_cap: u64,
    pub panic: Option<String>,
    pub notes: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct SummaryLine {
    pub kind: &'static str,
    pub cycles: u64,
    pub failures: u64,
    pub leak_suspects: u64,
    pub rss_ceiling_fails: u64,
    pub wall_secs: f64,
    pub peak_vm_rss_kb: u64,
    pub peak_vm_hwm_kb: u64,
    pub baseline_rss_kb: Option<u64>,
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

/// Leak math: last `window` RSS samples strictly increasing AND
/// (last − first) / baseline × 100 ≥ leak_pct. Baseline = cycle-3 end RSS.
pub fn leak_suspect(series: &[u64], baseline: u64, window: usize, leak_pct: f64) -> bool {
    if series.len() < window || baseline == 0 {
        return false;
    }
    let w = &series[series.len() - window..];
    for pair in w.windows(2) {
        if pair[1] <= pair[0] {
            return false;
        }
    }
    let first = w[0];
    let last = w[w.len() - 1];
    let climb_pct = (last.saturating_sub(first)) as f64 / baseline as f64 * 100.0;
    climb_pct >= leak_pct
}
