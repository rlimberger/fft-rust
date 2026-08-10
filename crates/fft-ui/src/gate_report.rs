//! Machine-readable gate evidence: the JSON result file written by `--gate-out`, the
//! coverage exit line, and the pure PASS/FAIL rules behind the process exit code.
//!
//! A gate result that cannot identify itself is not evidence, so every report carries the
//! git SHA + dirty flag captured *before* the window opens, the replay path, and the
//! coverage counters of the run. Pure: no GPUI, no `FrameStats`, no clock — the caller
//! supplies measured values, so the schema is unit-testable headless.

use std::fmt;
use std::fs::File;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::Serialize;

/// Gate outcome. `FAIL` is decided by the rules in [`verdict`], independent of whether the
/// run was gated; `--gate` only decides whether a `FAIL` is fatal to the process.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "UPPERCASE")]
pub enum Verdict {
    Pass,
    Fail,
}

/// Event-coverage evidence for one replay run (`docs/ENGINE.md` §3).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
pub struct CoverageReport {
    pub events_read: u64,
    pub events_applied: u64,
    /// `events_read - events_applied`; the M3 gate requires zero.
    pub dropped: u64,
    /// Gap events encountered — loud data, not a drop, never gate-fatal.
    pub gap_records: u64,
}

impl CoverageReport {
    pub fn new(events_read: u64, events_applied: u64, gap_records: u64) -> Self {
        Self {
            events_read,
            events_applied,
            // Saturating: applied > read is an engine invariant violation, not a negative
            // drop count. The engine debug-asserts it; here it reads as zero drops.
            dropped: events_read.saturating_sub(events_applied),
            gap_records,
        }
    }
}

impl fmt::Display for CoverageReport {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "coverage events_read={} events_applied={} dropped={} gap_records={}",
            self.events_read, self.events_applied, self.dropped, self.gap_records
        )
    }
}

/// Measured frame-time result. Field names match the M0 gate artifact's `result` object.
#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
pub struct FrameResult {
    pub refresh_interval_ms: f64,
    pub refresh_hz: f64,
    pub deadline_ms: f64,
    /// Measured warmup duration (the elapsed time, not the configured budget).
    pub warmup_ms: f64,
    /// Frame gaps observed during warmup.
    pub warmup_samples: u64,
    pub frames: u64,
    pub missed: u64,
    pub p50_ms: f64,
    pub p95_ms: f64,
    pub p99_ms: f64,
    pub max_ms: f64,
}

/// Run provenance captured at startup, before the window opens.
#[derive(Debug, Clone)]
pub struct RunMeta {
    /// Human-readable gate description (M0 artifact field `gate`).
    pub gate: String,
    /// Reconstructed command line (M0 artifact field `binary`).
    pub binary: String,
    pub git: GitInfo,
    pub replay: Option<PathBuf>,
    pub trace: Option<PathBuf>,
    /// Perf-runner manifest path (`--manifest`); `None` ⇒ JSON `null`.
    pub manifest: Option<String>,
    /// Run conditions text (`--conditions`); `None` ⇒ JSON `null`.
    pub conditions: Option<String>,
}

/// Git provenance. `sha == "unknown"` (with `dirty == None`) is the only tolerated
/// degradation and always warns on stderr — see [`GitInfo::capture`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GitInfo {
    pub sha: String,
    pub dirty: Option<bool>,
}

impl GitInfo {
    /// Runs `git rev-parse HEAD` + `git status --porcelain`. Call this at startup: a gate
    /// result that identifies the wrong commit is worse than no result.
    pub fn capture() -> Self {
        match (git(&["rev-parse", "HEAD"]), git(&["status", "--porcelain"])) {
            (Ok(sha), Ok(status)) => Self {
                sha: sha.trim().to_string(),
                dirty: Some(!status.trim().is_empty()),
            },
            (Err(err), _) | (_, Err(err)) => {
                eprintln!(
                    "fft: WARNING no git provenance ({err}); gate evidence records sha \"unknown\""
                );
                Self {
                    sha: "unknown".to_string(),
                    dirty: None,
                }
            }
        }
    }
}

fn git(args: &[&str]) -> Result<String, String> {
    let label = || format!("git {}", args.join(" "));
    let output = Command::new("git")
        .args(args)
        .output()
        .map_err(|err| format!("{}: {err}", label()))?;
    if !output.status.success() {
        return Err(format!(
            "{} exited {}: {}",
            label(),
            output.status,
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    String::from_utf8(output.stdout).map_err(|err| format!("{}: non-UTF-8 output: {err}", label()))
}

/// The `--gate-out` JSON document. A superset of the M0 gate artifact: fields the binary
/// cannot know (perf-runner manifest, run conditions, notes) are emitted as `null` for
/// the runner to fill in, never guessed. `gpui_rev` is knowable at build time from
/// `Cargo.lock` and is always populated.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct GateReport {
    pub gate: String,
    /// RFC 3339 UTC, second resolution.
    pub date: String,
    pub binary: String,
    pub git_sha: String,
    /// `null` iff `git_sha == "unknown"`.
    pub git_dirty: Option<bool>,
    pub replay: Option<String>,
    pub trace: Option<String>,
    pub manifest: Option<String>,
    pub gpui_rev: Option<String>,
    pub conditions: Option<String>,
    pub notes: Option<String>,
    /// `null` when warmup never completed (no frames measured) — itself a `FAIL`.
    pub result: Option<FrameResult>,
    /// `null` when not replaying.
    pub coverage: Option<CoverageReport>,
    pub verdict: Verdict,
}

impl GateReport {
    pub fn new(
        meta: &RunMeta,
        date: String,
        result: Option<FrameResult>,
        coverage: Option<CoverageReport>,
    ) -> Self {
        Self {
            gate: meta.gate.clone(),
            date,
            binary: meta.binary.clone(),
            git_sha: meta.git.sha.clone(),
            git_dirty: meta.git.dirty,
            replay: path_string(meta.replay.as_deref()),
            trace: path_string(meta.trace.as_deref()),
            manifest: meta.manifest.clone(),
            gpui_rev: Some(env!("FFT_GPUI_REV").to_string()),
            conditions: meta.conditions.clone(),
            notes: None,
            verdict: verdict(result.as_ref(), coverage.as_ref()),
            result,
            coverage,
        }
    }

    /// Record that the engine thread died. The frame times measured before the death are
    /// kept — they are still evidence — but the run is a `FAIL` whatever they say, and the
    /// panic payload lands in `notes` so the JSON explains its own verdict.
    pub fn record_engine_panic(&mut self, detail: &str) {
        self.verdict = Verdict::Fail;
        self.notes = Some(format!("engine thread panicked: {detail}"));
    }
}

fn path_string(path: Option<&Path>) -> Option<String> {
    path.map(|path| path.display().to_string())
}

/// Gate rules, in one place: no frames measured, any missed deadline, or any dropped event
/// is a `FAIL` (`docs/ENGINE.md` §3 — the M3 gate requires zero drops). Gap records are
/// informational and never fail the gate.
pub fn verdict(result: Option<&FrameResult>, coverage: Option<&CoverageReport>) -> Verdict {
    let frames_clean = result.is_some_and(|result| result.frames > 0 && result.missed == 0);
    let coverage_clean = coverage.is_none_or(|coverage| coverage.dropped == 0);
    if frames_clean && coverage_clean {
        Verdict::Pass
    } else {
        Verdict::Fail
    }
}

/// A failing verdict is fatal to the process only when the run was gated.
pub fn gate_failed(gating: bool, verdict: Verdict) -> bool {
    gating && verdict == Verdict::Fail
}

/// Evidence file, opened (and truncated) at startup so an unwritable path fails before a
/// measured run is spent, never after it.
#[derive(Debug)]
pub struct GateOut {
    path: PathBuf,
    file: File,
}

impl GateOut {
    pub fn create(path: PathBuf) -> Self {
        let file = File::create(&path).unwrap_or_else(|err| {
            panic!(
                "fft: cannot open gate result file {}: {err}",
                path.display()
            )
        });
        Self { path, file }
    }

    /// Writes the report — including a `FAIL` (evidence of failure is evidence).
    pub fn write(mut self, report: &GateReport) {
        let json = serde_json::to_string_pretty(report)
            .unwrap_or_else(|err| panic!("fft: gate result serialization failed: {err}"));
        writeln!(self.file, "{json}").unwrap_or_else(|err| {
            panic!(
                "fft: gate result write failed {}: {err}",
                self.path.display()
            )
        });
        self.file.flush().unwrap_or_else(|err| {
            panic!(
                "fft: gate result flush failed {}: {err}",
                self.path.display()
            )
        });
        eprintln!("fft: gate result written to {}", self.path.display());
    }
}

/// Milliseconds, rounded to microsecond resolution (matches the M0 artifact's precision).
pub fn millis(duration: Duration) -> f64 {
    round3(duration.as_secs_f64() * 1e3)
}

pub fn round3(value: f64) -> f64 {
    (value * 1e3).round() / 1e3
}

/// Wall-clock now as RFC 3339 UTC. Panics if the system clock predates the Unix epoch.
pub fn now_rfc3339_utc() -> String {
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_else(|err| panic!("fft: system clock predates the Unix epoch: {err}"))
        .as_secs();
    rfc3339_utc(secs)
}

/// RFC 3339 UTC timestamp at second resolution (`2026-08-10T10:11:00Z`).
pub fn rfc3339_utc(unix_secs: u64) -> String {
    let days = (unix_secs / 86_400) as i64;
    let secs = unix_secs % 86_400;
    let (year, month, day) = civil_from_days(days);
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}Z",
        secs / 3600,
        (secs / 60) % 60,
        secs % 60
    )
}

/// Days since the Unix epoch to `(year, month, day)` (Hinnant's `civil_from_days`).
pub(crate) fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let month = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    let year = yoe + era * 400 + i64::from(month <= 2);
    (year, month, day)
}

/// Command line for the `binary` field: `argv[0]` reduced to its file name.
pub fn command_line<I: IntoIterator<Item = String>>(argv: I) -> String {
    let mut parts = argv.into_iter();
    let Some(program) = parts.next() else {
        return String::new();
    };
    let program = Path::new(&program).file_name().map_or_else(
        || program.clone(),
        |name| name.to_string_lossy().into_owned(),
    );
    std::iter::once(program)
        .chain(parts)
        .collect::<Vec<_>>()
        .join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_result() -> FrameResult {
        FrameResult {
            refresh_interval_ms: 4.167,
            refresh_hz: 240.0,
            deadline_ms: 6.25,
            warmup_ms: 501.2,
            warmup_samples: 120,
            frames: 14_400,
            missed: 0,
            p50_ms: 4.17,
            p95_ms: 4.2,
            p99_ms: 4.31,
            max_ms: 5.02,
        }
    }

    #[test]
    fn dropped_events_force_failure_even_with_zero_missed_frames() {
        let result = sample_result();
        assert_eq!(result.missed, 0);
        let dropped = CoverageReport::new(82_000_000, 81_999_999, 0);
        assert_eq!(dropped.dropped, 1);
        assert_eq!(verdict(Some(&result), Some(&dropped)), Verdict::Fail);
        assert!(gate_failed(true, verdict(Some(&result), Some(&dropped))));
        // Ungated runs report the same verdict but never fail the process.
        assert!(!gate_failed(false, verdict(Some(&result), Some(&dropped))));
    }

    #[test]
    fn clean_run_passes_and_gaps_are_not_drops() {
        let result = sample_result();
        let coverage = CoverageReport::new(1_000, 1_000, 17);
        assert_eq!(coverage.dropped, 0);
        assert_eq!(verdict(Some(&result), Some(&coverage)), Verdict::Pass);
        assert!(!gate_failed(true, Verdict::Pass));
        // Not replaying: absent coverage cannot fail the gate.
        assert_eq!(verdict(Some(&result), None), Verdict::Pass);
    }

    #[test]
    fn missed_frames_and_missing_frames_both_fail() {
        let missed = FrameResult {
            missed: 1,
            ..sample_result()
        };
        assert_eq!(verdict(Some(&missed), None), Verdict::Fail);
        let empty = FrameResult {
            frames: 0,
            ..sample_result()
        };
        assert_eq!(verdict(Some(&empty), None), Verdict::Fail);
        assert_eq!(verdict(None, None), Verdict::Fail);
        assert!(gate_failed(true, Verdict::Fail));
    }

    #[test]
    fn coverage_line_reports_read_minus_applied() {
        let coverage = CoverageReport::new(100, 97, 2);
        assert_eq!(coverage.dropped, 3);
        assert_eq!(
            coverage.to_string(),
            "coverage events_read=100 events_applied=97 dropped=3 gap_records=2"
        );
        // applied > read violates the engine invariant; it must not underflow.
        assert_eq!(CoverageReport::new(1, 5, 0).dropped, 0);
    }

    #[test]
    fn engine_panic_fails_a_clean_run_and_is_noted() {
        let meta = RunMeta {
            gate: "fft frame gate — 60.000 s".to_string(),
            binary: "fft --gate 60".to_string(),
            git: GitInfo {
                sha: "abc".to_string(),
                dirty: Some(false),
            },
            replay: None,
            trace: None,
            manifest: None,
            conditions: None,
        };
        let mut report = GateReport::new(
            &meta,
            rfc3339_utc(0),
            Some(sample_result()),
            Some(CoverageReport::new(1_000, 1_000, 0)),
        );
        assert_eq!(report.verdict, Verdict::Pass);
        report.record_engine_panic("fft-engine replay failure: bad frame");
        assert_eq!(report.verdict, Verdict::Fail);
        assert_eq!(
            report.notes.as_deref(),
            Some("engine thread panicked: fft-engine replay failure: bad frame")
        );
        // The measured frames survive the panic — they are the evidence of the run.
        assert!(report.result.is_some());
    }

    #[test]
    fn rfc3339_matches_known_instants() {
        assert_eq!(rfc3339_utc(0), "1970-01-01T00:00:00Z");
        assert_eq!(rfc3339_utc(951_868_800), "2000-03-01T00:00:00Z");
        assert_eq!(rfc3339_utc(1_709_251_199), "2024-02-29T23:59:59Z");
        assert_eq!(rfc3339_utc(1_786_356_660), "2026-08-10T10:11:00Z");
    }

    #[test]
    fn command_line_uses_the_binary_file_name() {
        let argv = ["target/release/fft", "--gate", "60", "--gate-out", "r.json"]
            .map(String::from)
            .to_vec();
        assert_eq!(command_line(argv), "fft --gate 60 --gate-out r.json");
        assert_eq!(command_line(Vec::new()), "");
    }

    #[test]
    fn millis_rounds_to_microseconds() {
        assert_eq!(millis(Duration::from_nanos(16_654_321)), 16.654);
        assert_eq!(millis(Duration::ZERO), 0.0);
        assert_eq!(round3(239.999_6), 240.0);
    }
}
