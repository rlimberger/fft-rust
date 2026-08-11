//! JSON evidence schema + provenance helpers for the M1 data-plane gate harness.

use std::path::PathBuf;
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

#[derive(Debug, Clone)]
pub struct DayStat {
    pub path: PathBuf,
    pub trade_date: u32,
    pub trade_date_ymd: String,
    pub symbol: String,
    pub event_count: u64,
    pub file_bytes: u64,
    pub bytes_per_event: f64,
    pub legacy_bytes: Option<u64>,
    pub legacy_ratio: Option<f64>,
    pub legacy_status: String,
}

#[derive(Debug)]
pub struct ApplyResult {
    pub events: u64,
    pub seconds: f64,
    pub book_bytes: Vec<u8>,
    pub flow_bytes: Vec<u8>,
    pub refresh_bytes: Vec<u8>,
    pub profile_bytes: Vec<u8>,
    pub cvd_bytes: Vec<u8>,
    pub session_bytes: Vec<u8>,
    pub applied_seq: u64,
    pub applied_ts: u64,
}

#[derive(Debug)]
pub struct DiffTrial {
    pub trial: usize,
    pub n_chunks: usize,
    pub chunk_sizes: Vec<usize>,
    pub match_oneshot: bool,
    pub seconds: f64,
    pub fail_reason: Option<String>,
}

pub fn capture_git_sha() -> String {
    let output = Command::new("git")
        .args(["rev-parse", "HEAD"])
        .output()
        .ok();
    match output {
        Some(o) if o.status.success() => String::from_utf8_lossy(&o.stdout).trim().to_string(),
        _ => "unknown".into(),
    }
}

pub fn utc_date_string() -> String {
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let days = (secs / 86_400) as u32;
    ymd_from_unix_days(days)
}

/// Civil YYYY-MM-DD from days since Unix epoch (UTC). Howard Hinnant algorithm.
pub fn ymd_from_unix_days(days: u32) -> String {
    let z = i64::from(days) + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    format!("{y:04}-{m:02}-{d:02}")
}

fn json_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 8);
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if c.is_control() => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out
}

#[allow(clippy::too_many_arguments)]
pub fn render_json(
    git_sha: &str,
    date_utc: &str,
    timing_label: &str,
    smoke: bool,
    seed: u64,
    days: &[DayStat],
    busiest: &DayStat,
    oneshot: &ApplyResult,
    apply_pass: bool,
    apply_budget_s: f64,
    trials: &[DiffTrial],
    differential_pass: bool,
    size_claim: &str,
    size_ratio_unverified: bool,
) -> String {
    let mut s = String::with_capacity(4096);
    s.push_str("{\n");
    s.push_str("  \"gate\": \"M1-data-plane\",\n");
    s.push_str(&format!("  \"git_sha\": \"{}\",\n", json_escape(git_sha)));
    s.push_str(&format!("  \"date\": \"{}\",\n", json_escape(date_utc)));
    s.push_str(&format!(
        "  \"timing_label\": \"{}\",\n",
        json_escape(timing_label)
    ));
    s.push_str(&format!(
        "  \"smoke\": {},\n",
        if smoke { "true" } else { "false" }
    ));
    s.push_str(&format!("  \"seed\": {seed},\n"));
    s.push_str("  \"note\": \"SMOKE timings are not claimable; only QUIET_BOX runs on an idle host count as gate evidence\",\n");

    s.push_str("  \"days\": [\n");
    for (i, d) in days.iter().enumerate() {
        s.push_str("    {\n");
        s.push_str(&format!(
            "      \"path\": \"{}\",\n",
            json_escape(&d.path.display().to_string())
        ));
        s.push_str(&format!("      \"trade_date\": {},\n", d.trade_date));
        s.push_str(&format!(
            "      \"trade_date_ymd\": \"{}\",\n",
            json_escape(&d.trade_date_ymd)
        ));
        s.push_str(&format!(
            "      \"symbol\": \"{}\",\n",
            json_escape(&d.symbol)
        ));
        s.push_str(&format!("      \"event_count\": {},\n", d.event_count));
        s.push_str(&format!("      \"file_bytes\": {},\n", d.file_bytes));
        s.push_str(&format!(
            "      \"bytes_per_event\": {:.6},\n",
            d.bytes_per_event
        ));
        match d.legacy_bytes {
            Some(b) => s.push_str(&format!("      \"legacy_bytes\": {b},\n")),
            None => s.push_str("      \"legacy_bytes\": null,\n"),
        }
        match d.legacy_ratio {
            Some(r) => s.push_str(&format!("      \"legacy_ratio\": {r:.6},\n")),
            None => s.push_str("      \"legacy_ratio\": null,\n"),
        }
        s.push_str(&format!(
            "      \"legacy_status\": \"{}\"\n",
            json_escape(&d.legacy_status)
        ));
        s.push_str("    }");
        if i + 1 < days.len() {
            s.push(',');
        }
        s.push('\n');
    }
    s.push_str("  ],\n");

    s.push_str("  \"busiest_day\": {\n");
    s.push_str(&format!(
        "    \"trade_date_ymd\": \"{}\",\n",
        json_escape(&busiest.trade_date_ymd)
    ));
    s.push_str(&format!("    \"event_count\": {},\n", busiest.event_count));
    s.push_str(&format!(
        "    \"path\": \"{}\"\n",
        json_escape(&busiest.path.display().to_string())
    ));
    s.push_str("  },\n");

    s.push_str("  \"forward_apply\": {\n");
    s.push_str(&format!("    \"events\": {},\n", oneshot.events));
    s.push_str(&format!("    \"seconds\": {:.9},\n", oneshot.seconds));
    s.push_str(&format!("    \"budget_seconds\": {apply_budget_s},\n"));
    s.push_str(&format!(
        "    \"verdict\": \"{}\",\n",
        if apply_pass { "PASS" } else { "FAIL" }
    ));
    s.push_str(&format!(
        "    \"timing_label\": \"{}\",\n",
        json_escape(timing_label)
    ));
    s.push_str(&format!("    \"applied_seq\": {},\n", oneshot.applied_seq));
    s.push_str(&format!("    \"applied_ts\": {},\n", oneshot.applied_ts));
    s.push_str(&format!(
        "    \"serialize_book_bytes\": {},\n",
        oneshot.book_bytes.len()
    ));
    s.push_str(&format!(
        "    \"serialize_flow_bytes\": {},\n",
        oneshot.flow_bytes.len()
    ));
    s.push_str(&format!(
        "    \"serialize_refresh_bytes\": {},\n",
        oneshot.refresh_bytes.len()
    ));
    s.push_str(&format!(
        "    \"profile_section_bytes\": {},\n",
        oneshot.profile_bytes.len()
    ));
    s.push_str(&format!(
        "    \"cvd_section_bytes\": {},\n",
        oneshot.cvd_bytes.len()
    ));
    s.push_str(&format!(
        "    \"session_section_bytes\": {}\n",
        oneshot.session_bytes.len()
    ));
    s.push_str("  },\n");

    s.push_str("  \"differential\": {\n");
    s.push_str(&format!(
        "    \"verdict\": \"{}\",\n",
        if differential_pass { "PASS" } else { "FAIL" }
    ));
    s.push_str(&format!("    \"trials\": {},\n", trials.len()));
    s.push_str("    \"results\": [\n");
    for (i, t) in trials.iter().enumerate() {
        s.push_str("      {\n");
        s.push_str(&format!("        \"trial\": {},\n", t.trial));
        s.push_str(&format!("        \"n_chunks\": {},\n", t.n_chunks));
        s.push_str("        \"chunk_sizes\": [");
        for (j, c) in t.chunk_sizes.iter().enumerate() {
            if j > 0 {
                s.push_str(", ");
            }
            s.push_str(&c.to_string());
        }
        s.push_str("],\n");
        s.push_str(&format!(
            "        \"match_oneshot\": {},\n",
            if t.match_oneshot { "true" } else { "false" }
        ));
        s.push_str(&format!("        \"seconds\": {:.9},\n", t.seconds));
        match &t.fail_reason {
            Some(r) => s.push_str(&format!(
                "        \"fail_reason\": \"{}\"\n",
                json_escape(r)
            )),
            None => s.push_str("        \"fail_reason\": null\n"),
        }
        s.push_str("      }");
        if i + 1 < trials.len() {
            s.push(',');
        }
        s.push('\n');
    }
    s.push_str("    ]\n");
    s.push_str("  },\n");

    s.push_str("  \"log_size\": {\n");
    s.push_str(&format!(
        "    \"claim_leq_half_legacy\": \"{}\",\n",
        json_escape(size_claim)
    ));
    s.push_str(&format!(
        "    \"ratio_unverified\": {},\n",
        if size_ratio_unverified {
            "true"
        } else {
            "false"
        }
    ));
    s.push_str("    \"bytes_per_event_table\": [\n");
    for (i, d) in days.iter().enumerate() {
        s.push_str("      {\n");
        s.push_str(&format!(
            "        \"trade_date_ymd\": \"{}\",\n",
            json_escape(&d.trade_date_ymd)
        ));
        s.push_str(&format!("        \"event_count\": {},\n", d.event_count));
        s.push_str(&format!("        \"v2_bytes\": {},\n", d.file_bytes));
        s.push_str(&format!(
            "        \"v2_bytes_per_event\": {:.6},\n",
            d.bytes_per_event
        ));
        match d.legacy_bytes {
            Some(b) => s.push_str(&format!("        \"legacy_bytes\": {b},\n")),
            None => s.push_str("        \"legacy_bytes\": null,\n"),
        }
        match d.legacy_ratio {
            Some(r) => s.push_str(&format!("        \"v2_over_legacy\": {r:.6},\n")),
            None => s.push_str("        \"v2_over_legacy\": null,\n"),
        }
        s.push_str(&format!(
            "        \"legacy_status\": \"{}\"\n",
            json_escape(&d.legacy_status)
        ));
        s.push_str("      }");
        if i + 1 < days.len() {
            s.push(',');
        }
        s.push('\n');
    }
    s.push_str("    ]\n");
    s.push_str("  }\n");
    s.push_str("}\n");
    s
}
