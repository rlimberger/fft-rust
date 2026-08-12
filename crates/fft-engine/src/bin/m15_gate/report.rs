//! JSON evidence schema + provenance for the M1.5 sim-live gate.

use serde::Serialize;
use std::any::Any;
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

#[derive(Debug, Clone, Serialize)]
pub struct DistNs {
    pub n: usize,
    pub p50_ns: u64,
    pub p95_ns: u64,
    pub p99_ns: u64,
    pub max_ns: u64,
    pub min_ns: u64,
    pub mean_ns: f64,
}

impl DistNs {
    pub fn from_abs_samples(mut samples: Vec<u64>) -> Option<Self> {
        if samples.is_empty() {
            return None;
        }
        samples.sort_unstable();
        let n = samples.len();
        let sum: u128 = samples.iter().map(|v| u128::from(*v)).sum();
        Some(Self {
            n,
            p50_ns: percentile(&samples, 0.50),
            p95_ns: percentile(&samples, 0.95),
            p99_ns: percentile(&samples, 0.99),
            max_ns: *samples.last().expect("non-empty"),
            min_ns: samples[0],
            mean_ns: sum as f64 / n as f64,
        })
    }
}
fn percentile(sorted: &[u64], p: f64) -> u64 {
    let n = sorted.len();
    if n == 1 {
        return sorted[0];
    }
    sorted[(((n as f64 - 1.0) * p).round() as usize).min(n - 1)]
}

#[derive(Debug, Clone, Serialize)]
pub struct Budgets {
    pub apply_budget_ns: u64,
    pub gate_secs: u64,
    pub join_timeout_s: u64,
}

#[derive(Debug, Clone, Serialize)]
pub struct SourceCheck {
    pub requested_head_ts: u64,
    pub pinned_event_ts: u64,
    pub head_snap_back_ns: u64,
    pub session_open_ts: u64,
    pub first_event_ts: u64,
    pub last_event_ts: u64,
    pub events_through_head: u64,
    pub last_ts_through_head: u64,
    pub checkpoint_count: usize,
    pub head_in_log: bool,
    pub starts_at_session_open: bool,
    pub ok: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct JoinCheck {
    pub pinned_head_ts: u64,
    pub applied_ts: u64,
    pub applied_seq: u64,
    pub events_read: u64,
    pub events_applied: u64,
    pub seek_generation: u64,
    pub join_wall_s: f64,
    pub reached_head: bool,
    pub applied_from_open: bool,
    pub clean_coverage: bool,
    pub ok: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct LagCheck {
    pub distinct_publications_sampled: usize,
    pub abs_head_lag: Option<DistNs>,
    pub apply_budget_ns: u64,
    pub advanced_ts_ns: u64,
    pub clean_coverage: bool,
    pub ok: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct GoLiveCheck {
    pub scrub_target_ts: u64,
    pub scrubbed_ts: u64,
    pub tip_before_scrub_ts: u64,
    pub resumed_ts: u64,
    pub resumed_seek_generation: u64,
    pub resumed_abs_head_lag_ns: u64,
    pub scrubbed_behind_tip: bool,
    pub reached_prior_tip: bool,
    pub ok: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct GapCheck {
    pub injected_gap_ts: u64,
    pub injected_expected_seq: u64,
    pub injected_observed_seq: u64,
    pub gap_records: u64,
    pub applied_seq: u64,
    pub logged_seq: u64,
    pub refresh_order_id: u64,
    pub refresh_unavailable: bool,
    pub ok: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct IdentityCheck {
    pub replayed_events: u64,
    pub replayed_applied_seq: u64,
    pub replayed_applied_ts: u64,
    pub compared_sections: Vec<String>,
    pub first_mismatch: Option<String>,
    pub ok: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct WatermarkEvidence {
    pub received_seq: u64,
    pub decoded_seq: u64,
    pub applied_seq: u64,
    pub logged_seq: u64,
    pub published_seq: u64,
}

#[derive(Debug, Clone, Serialize)]
pub struct LiveLifecycle {
    pub during_is_live: bool,
    pub during_index_source_live_recovery: bool,
    pub after_not_live: bool,
    pub after_index_source_footer: bool,
    pub after_recovery_none: bool,
    pub after_warnings_empty: bool,
}

impl LiveLifecycle {
    pub fn unavailable() -> Self {
        Self {
            during_is_live: false,
            during_index_source_live_recovery: false,
            after_not_live: false,
            after_index_source_footer: false,
            after_recovery_none: false,
            after_warnings_empty: false,
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct AppendCheck {
    pub live_out_bytes: u64,
    pub events_read: u64,
    pub events_applied: u64,
    pub gap_records: u64,
    pub watermarks: WatermarkEvidence,
    pub source_warnings: Vec<String>,
    pub live_lifecycle: LiveLifecycle,
    pub clean_coverage: bool,
    pub logged_through_applied: bool,
    pub ok: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct Evidence {
    pub schema_version: u32,
    pub gate: String,
    pub date: String,
    pub binary: String,
    pub git_sha: String,
    pub git_dirty: Option<bool>,
    pub replay: String,
    pub head_ts: u64,
    pub live_out: String,
    pub source: SourceCheck,
    pub join: JoinCheck,
    pub lag: LagCheck,
    pub go_live: GoLiveCheck,
    pub gap: GapCheck,
    pub append: AppendCheck,
    pub identity: IdentityCheck,
    pub budgets: Budgets,
    pub failures: Vec<String>,
    pub verdict: String,
    pub notes: Option<String>,
}

pub const NOTE_BASE: &str = "lag samples=distinct pubs; gap harness-spliced; identity=BOOK/FLOW/REFRESH/PROFILE/CVD/SESSION";

impl SourceCheck {
    pub fn unavailable(head_ts: u64) -> Self {
        Self {
            requested_head_ts: head_ts,
            pinned_event_ts: 0,
            head_snap_back_ns: 0,
            session_open_ts: 0,
            first_event_ts: 0,
            last_event_ts: 0,
            events_through_head: 0,
            last_ts_through_head: 0,
            checkpoint_count: 0,
            head_in_log: false,
            starts_at_session_open: false,
            ok: false,
        }
    }
}
impl JoinCheck {
    pub fn unavailable(pinned_head_ts: u64) -> Self {
        Self {
            pinned_head_ts,
            applied_ts: 0,
            applied_seq: 0,
            events_read: 0,
            events_applied: 0,
            seek_generation: 0,
            join_wall_s: 0.0,
            reached_head: false,
            applied_from_open: false,
            clean_coverage: false,
            ok: false,
        }
    }
}
impl LagCheck {
    pub fn unavailable(apply_budget_ns: u64) -> Self {
        Self {
            distinct_publications_sampled: 0,
            abs_head_lag: None,
            apply_budget_ns,
            advanced_ts_ns: 0,
            clean_coverage: false,
            ok: false,
        }
    }
}
impl GoLiveCheck {
    pub fn unavailable() -> Self {
        Self {
            scrub_target_ts: 0,
            scrubbed_ts: 0,
            tip_before_scrub_ts: 0,
            resumed_ts: 0,
            resumed_seek_generation: 0,
            resumed_abs_head_lag_ns: u64::MAX,
            scrubbed_behind_tip: false,
            reached_prior_tip: false,
            ok: false,
        }
    }
}
impl GapCheck {
    pub fn unavailable() -> Self {
        Self {
            injected_gap_ts: 0,
            injected_expected_seq: 0,
            injected_observed_seq: 0,
            gap_records: 0,
            applied_seq: 0,
            logged_seq: 0,
            refresh_order_id: 0,
            refresh_unavailable: false,
            ok: false,
        }
    }
}
impl AppendCheck {
    pub fn unavailable() -> Self {
        Self {
            live_out_bytes: 0,
            events_read: 0,
            events_applied: 0,
            gap_records: 0,
            watermarks: WatermarkEvidence {
                received_seq: 0,
                decoded_seq: 0,
                applied_seq: 0,
                logged_seq: 0,
                published_seq: 0,
            },
            source_warnings: Vec::new(),
            live_lifecycle: LiveLifecycle::unavailable(),
            clean_coverage: false,
            logged_through_applied: false,
            ok: false,
        }
    }
}
impl IdentityCheck {
    pub fn unavailable() -> Self {
        Self {
            replayed_events: 0,
            replayed_applied_seq: 0,
            replayed_applied_ts: 0,
            compared_sections: Vec::new(),
            first_mismatch: Some("unavailable".into()),
            ok: false,
        }
    }
}

pub struct EvidenceInput<'a> {
    pub replay: &'a str,
    pub head_ts: u64,
    pub live_out: &'a str,
    pub source: SourceCheck,
    pub join: JoinCheck,
    pub lag: LagCheck,
    pub go_live: GoLiveCheck,
    pub gap: GapCheck,
    pub append: AppendCheck,
    pub identity: IdentityCheck,
    pub budgets: Budgets,
    pub notes: Option<String>,
}

pub fn assemble_evidence(input: EvidenceInput<'_>) -> Evidence {
    let mut failures = Vec::new();
    push_failure(&mut failures, input.source.ok, "source/head validation");
    push_failure(
        &mut failures,
        input.join.ok,
        "join from session open to pinned head",
    );
    push_failure(&mut failures, input.lag.ok, "absolute 1x wall pin");
    push_failure(
        &mut failures,
        input.go_live.ok,
        "GoLive catch-up to wall head",
    );
    push_failure(
        &mut failures,
        input.gap.ok,
        "injected-gap loudness/unavailable classification",
    );
    push_failure(
        &mut failures,
        input.append.ok,
        "live append coverage/watermarks",
    );
    push_failure(
        &mut failures,
        input.identity.ok,
        "append-log six-section replay identity",
    );
    finish_evidence(input, failures)
}

#[derive(Debug, Clone, Default)]
pub struct PartialChecks {
    pub source: Option<SourceCheck>,
    pub join: Option<JoinCheck>,
    pub lag: Option<LagCheck>,
    pub go_live: Option<GoLiveCheck>,
    pub gap: Option<GapCheck>,
}

pub struct RuntimeFail<'a> {
    pub replay: &'a str,
    pub head_ts: u64,
    pub live_out: &'a str,
    pub budgets: Budgets,
    pub dimension: &'a str,
    pub diagnostic: &'a str,
    pub partial: PartialChecks,
}

pub fn runtime_fail_evidence(fail: RuntimeFail<'_>) -> Evidence {
    let source = fail
        .partial
        .source
        .unwrap_or_else(|| SourceCheck::unavailable(fail.head_ts));
    let pinned = source.pinned_event_ts;
    let input = EvidenceInput {
        replay: fail.replay,
        head_ts: fail.head_ts,
        live_out: fail.live_out,
        source,
        join: fail
            .partial
            .join
            .unwrap_or_else(|| JoinCheck::unavailable(pinned)),
        lag: fail
            .partial
            .lag
            .unwrap_or_else(|| LagCheck::unavailable(fail.budgets.apply_budget_ns)),
        go_live: fail
            .partial
            .go_live
            .unwrap_or_else(GoLiveCheck::unavailable),
        gap: fail.partial.gap.unwrap_or_else(GapCheck::unavailable),
        append: AppendCheck::unavailable(),
        identity: IdentityCheck::unavailable(),
        budgets: fail.budgets,
        notes: Some(format!("{NOTE_BASE}; {}", fail.diagnostic)),
    };
    finish_evidence(input, vec![fail.dimension.to_string()])
}

fn finish_evidence(input: EvidenceInput<'_>, failures: Vec<String>) -> Evidence {
    let (git_sha, git_dirty) = git_info();
    Evidence {
        schema_version: 1,
        gate: "m15-simlive".into(),
        date: rfc3339_now(),
        binary: "m15-gate".into(),
        git_sha,
        git_dirty,
        replay: input.replay.to_string(),
        head_ts: input.head_ts,
        live_out: input.live_out.to_string(),
        source: input.source,
        join: input.join,
        lag: input.lag,
        go_live: input.go_live,
        gap: input.gap,
        append: input.append,
        identity: input.identity,
        budgets: input.budgets,
        verdict: if failures.is_empty() { "PASS" } else { "FAIL" }.into(),
        failures,
        notes: input.notes,
    }
}

fn push_failure(failures: &mut Vec<String>, ok: bool, dimension: &str) {
    if !ok {
        failures.push(dimension.to_string());
    }
}

pub fn panic_message(payload: &(dyn Any + Send)) -> String {
    payload
        .downcast_ref::<&str>()
        .map(|msg| (*msg).to_string())
        .or_else(|| payload.downcast_ref::<String>().cloned())
        .unwrap_or_else(|| "non-string panic payload".into())
}

pub fn git_info() -> (String, Option<bool>) {
    match (git(&["rev-parse", "HEAD"]), git(&["status", "--porcelain"])) {
        (Ok(sha), Ok(status)) => (sha.trim().to_string(), Some(!status.trim().is_empty())),
        (Err(err), _) | (_, Err(err)) => {
            eprintln!("m15-gate: WARNING no git provenance ({err}); sha=\"unknown\"");
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
