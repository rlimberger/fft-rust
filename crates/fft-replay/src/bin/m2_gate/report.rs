//! JSON evidence schema + provenance helpers for the M2 gate harness.

use serde::Serialize;
use std::process::Command;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// Latency distribution in milliseconds (sorted-sample percentiles).
#[derive(Debug, Clone, Serialize)]
pub struct DistMs {
    pub n: usize,
    pub p50_ms: f64,
    pub p95_ms: f64,
    pub p99_ms: f64,
    pub max_ms: f64,
    pub min_ms: f64,
    pub mean_ms: f64,
}

impl DistMs {
    pub fn from_durations(mut times: Vec<Duration>) -> Self {
        assert!(!times.is_empty(), "m2-gate: empty duration sample");
        times.sort_unstable();
        let n = times.len();
        let to_ms = |d: Duration| d.as_secs_f64() * 1_000.0;
        let sum: f64 = times.iter().map(|d| to_ms(*d)).sum();
        Self {
            n,
            p50_ms: to_ms(percentile(&times, 0.50)),
            p95_ms: to_ms(percentile(&times, 0.95)),
            p99_ms: to_ms(percentile(&times, 0.99)),
            max_ms: to_ms(*times.last().expect("non-empty")),
            min_ms: to_ms(times[0]),
            mean_ms: sum / n as f64,
        }
    }
}

fn percentile(sorted: &[Duration], p: f64) -> Duration {
    let n = sorted.len();
    if n == 1 {
        return sorted[0];
    }
    let idx = ((n as f64 - 1.0) * p).round() as usize;
    sorted[idx.min(n - 1)]
}

#[derive(Debug, Clone, Serialize)]
pub struct LogInfo {
    pub path: String,
    pub event_count: u64,
    pub checkpoint_count: usize,
    pub frame_count: usize,
    pub first_ts: u64,
    pub last_ts: u64,
    pub event_time_span_s: f64,
}

#[derive(Debug, Clone, Serialize)]
pub struct Mismatch {
    pub target_ts: u64,
    pub section: String,
    pub seek_applied_seq: u64,
    pub seek_applied_ts: u64,
    pub forward_applied_seq: u64,
    pub forward_applied_ts: u64,
}

#[derive(Debug, Clone, Serialize)]
pub struct VerifyReport {
    pub count: usize,
    pub passed: usize,
    pub failed: usize,
    pub mismatch: Option<Mismatch>,
}

#[derive(Debug, Clone, Serialize)]
pub struct Throughput {
    pub events: u64,
    pub wall_s: f64,
    pub events_per_sec: f64,
    pub event_time_span_s: f64,
    pub realtime_multiple: f64,
}

#[derive(Debug, Clone, Serialize)]
pub struct Budgets {
    pub seek_p95_ms: f64,
    pub realtime_multiple: f64,
}

#[derive(Debug, Clone, Serialize)]
pub struct Report {
    pub gate: String,
    pub date: String,
    pub binary: String,
    pub git_sha: String,
    pub git_dirty: Option<bool>,
    pub label: Option<String>,
    pub log: LogInfo,
    pub seed: u64,
    pub seeks: usize,
    pub methodology: String,
    pub cold: DistMs,
    pub warm: DistMs,
    pub verify: VerifyReport,
    pub throughput: Throughput,
    pub budgets: Budgets,
    pub verdict: String,
    pub notes: Option<String>,
}

pub fn git_info() -> (String, Option<bool>) {
    match (git(&["rev-parse", "HEAD"]), git(&["status", "--porcelain"])) {
        (Ok(sha), Ok(status)) => (sha.trim().to_string(), Some(!status.trim().is_empty())),
        (Err(err), _) | (_, Err(err)) => {
            eprintln!("m2-gate: WARNING no git provenance ({err}); sha=\"unknown\"");
            ("unknown".into(), None)
        }
    }
}

fn git(args: &[&str]) -> Result<String, String> {
    let output = Command::new("git")
        .args(args)
        .output()
        .map_err(|e| format!("git {}: {e}", args.join(" ")))?;
    if !output.status.success() {
        return Err(format!("git {} exited {}", args.join(" "), output.status));
    }
    String::from_utf8(output.stdout).map_err(|e| e.to_string())
}

pub fn rfc3339_now() -> String {
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let days = secs / 86_400;
    let day_secs = secs % 86_400;
    let (y, m, d) = civil_from_days(days as i64);
    let hh = day_secs / 3600;
    let mm = (day_secs % 3600) / 60;
    let ss = day_secs % 60;
    format!("{y:04}-{m:02}-{d:02}T{hh:02}:{mm:02}:{ss:02}Z")
}

/// Howard Hinnant civil_from_days (UTC proleptic Gregorian).
fn civil_from_days(z: i64) -> (i32, u32, u32) {
    let z = z + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    (y as i32, m as u32, d as u32)
}
