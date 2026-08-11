//! Evidence JSON + provenance helpers for claim4-census.

use serde::Serialize;
use std::fs::{self, File};
use std::io::{self, Write};
use std::path::Path;
use std::process::{Command, exit};
use std::time::{SystemTime, UNIX_EPOCH};

#[derive(Serialize)]
pub struct Evidence {
    pub gate: &'static str,
    pub date: String,
    pub git_sha: String,
    pub git_dirty: Option<bool>,
    pub log: String,
    pub symbol: String,
    pub trade_date: u32,
    pub expected_events: u64,
    pub events_applied: u64,
    pub eof: bool,
    pub applied_seq: u64,
    pub applied_ts: u64,
    pub wall_s: f64,
    pub events_per_sec: f64,
    pub refresh: serde_json::Value,
    pub checks: serde_json::Value,
    pub notes: String,
    pub verdict: &'static str,
}

pub fn write_out(out: Option<&Path>, json: &str) {
    match out {
        None => io::stdout()
            .lock()
            .write_all(json.as_bytes())
            .unwrap_or_else(|e| panic!("claim4-census: stdout: {e}")),
        Some(path) => {
            if let Some(p) = path.parent()
                && !p.as_os_str().is_empty()
            {
                fs::create_dir_all(p)
                    .unwrap_or_else(|e| die(&format!("create {}: {e}", p.display())));
            }
            File::create(path)
                .and_then(|mut f| f.write_all(json.as_bytes()))
                .unwrap_or_else(|e| die(&format!("write {}: {e}", path.display())));
            eprintln!("claim4-census: wrote {}", path.display());
        }
    }
}

pub fn die(msg: &str) -> ! {
    eprintln!("claim4-census: {msg}");
    exit(1);
}

pub fn git_info() -> (String, Option<bool>) {
    match (git(&["rev-parse", "HEAD"]), git(&["status", "--porcelain"])) {
        (Ok(sha), Ok(status)) => (sha.trim().to_string(), Some(!status.trim().is_empty())),
        (Err(e), _) | (_, Err(e)) => {
            eprintln!("claim4-census: WARNING no git provenance ({e})");
            ("unknown".into(), None)
        }
    }
}

fn git(args: &[&str]) -> Result<String, String> {
    let o = Command::new("git")
        .args(args)
        .output()
        .map_err(|e| e.to_string())?;
    if !o.status.success() {
        return Err(format!("git {args:?} failed"));
    }
    String::from_utf8(o.stdout).map_err(|e| e.to_string())
}

pub fn utc_date_string() -> String {
    // Prefer `date` for brevity; fall back to epoch day count.
    if let Ok(o) = Command::new("date").args(["-u", "+%Y-%m-%d"]).output()
        && o.status.success()
    {
        return String::from_utf8_lossy(&o.stdout).trim().to_string();
    }
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    format!("unix_day_{}", secs / 86_400)
}
