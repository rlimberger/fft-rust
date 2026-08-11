//! Headless M4 pane-agreement harness.
//!
//! Spawns the real [`EngineService`], drives replay at max legal speed, and runs
//! [`check_pane_agreement`] on every *observed* coherent snapshot. Latest-value
//! publication means generations may be missed under max speed — that is acceptable;
//! coverage is reported honestly against `EngineExit.publications`.
//!
//! ```text
//! m4-agreement --replay <path.fftlog> --out <evidence.json> [--smoke]
//! ```

use std::path::{Path, PathBuf};
use std::process::exit;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use fft_engine::{DomRenderState, EngineCmd, EngineConfig, EngineExit, EngineService, Source};
use fft_ui::gate_report::GitInfo;
use fft_ui::mp_view::{VolumeMismatch, check_pane_agreement};
use serde::Serialize;

/// Protocol allows any finite `speed > 0` (`EngineCmd::SetSpeed` assert).
const MAX_SPEED: f64 = 1e9;

/// Stall after EOF: applied_seq + generation stable this long ⇒ playback done.
const STABLE_DONE: Duration = Duration::from_millis(50);

/// Absolute wall-clock cap for a full-session run (fail loudly past this).
const FULL_TIMEOUT: Duration = Duration::from_secs(120);

/// Absolute wall-clock cap for `--smoke` (subset / early stop).
const SMOKE_TIMEOUT: Duration = Duration::from_secs(30);

#[derive(Debug, Clone, Serialize)]
struct FailureDetail {
    generation: u64,
    applied_seq: u64,
    applied_ts: u64,
    price: i64,
    profile_volume: u64,
    dom_volume: u64,
    kind: &'static str,
}

#[derive(Debug, Clone, Serialize)]
struct Evidence {
    gate: &'static str,
    date: String,
    git_sha: String,
    git_dirty: Option<bool>,
    log: String,
    speed: f64,
    smoke: bool,
    snapshots_checked: u64,
    publications_total: u64,
    generations_observed: u64,
    generations_missed_estimate: u64,
    coverage_fraction: f64,
    agreement_failures: u64,
    structural_failures: u64,
    prices_compared_total: u64,
    first_failure: Option<FailureDetail>,
    exit_events_applied: u64,
    exit_events_read: u64,
    exit_gap_records: u64,
    wall_ms: f64,
    notes: Option<String>,
    verdict: &'static str,
}

fn usage(msg: &str) -> ! {
    eprintln!(
        "m4-agreement: {msg}\n\
         usage: m4-agreement --replay <path.fftlog> --out <evidence.json> [--smoke]"
    );
    exit(2)
}

fn parse_args() -> (PathBuf, PathBuf, bool) {
    let mut args = std::env::args().skip(1);
    let mut replay = None;
    let mut out = None;
    let mut smoke = false;
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--replay" => {
                replay = Some(PathBuf::from(
                    args.next()
                        .unwrap_or_else(|| usage("missing value for --replay")),
                ));
            }
            "--out" => {
                out = Some(PathBuf::from(
                    args.next()
                        .unwrap_or_else(|| usage("missing value for --out")),
                ));
            }
            "--smoke" => smoke = true,
            "-h" | "--help" => usage("help"),
            other => usage(&format!("unknown argument {other}")),
        }
    }
    let replay = replay.unwrap_or_else(|| usage("missing --replay"));
    let out = out.unwrap_or_else(|| usage("missing --out"));
    if !replay.is_file() {
        usage(&format!("replay log not found: {}", replay.display()));
    }
    (replay, out, smoke)
}

fn rfc3339_now() -> String {
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system clock before UNIX_EPOCH")
        .as_secs();
    let days = secs / 86_400;
    let tod = secs % 86_400;
    let (y, m, d) = civil_from_days(days as i64);
    let hh = tod / 3600;
    let mm = (tod % 3600) / 60;
    let ss = tod % 60;
    format!("{y:04}-{m:02}-{d:02}T{hh:02}:{mm:02}:{ss:02}Z")
}

/// Days since Unix epoch → (year, month, day). Howard Hinnant civil_from_days.
fn civil_from_days(days: i64) -> (i32, u32, u32) {
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
    let y = (yoe as i64) + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    (y as i32, m as u32, d as u32)
}

/// DOM rows ascending + contiguous by `tick_size`.
///
/// Engine `build_snapshot` emits a contiguous visible span; `dom_view::aggregate_rows`
/// asserts ascending (`source.price.0 >= prior` at dom_view.rs:171).
fn check_dom_structure(dom: &DomRenderState) -> Result<(), String> {
    if dom.rows.is_empty() {
        return Ok(());
    }
    if dom.tick_size.0 <= 0 {
        return Err(format!(
            "DOM tick_size must be positive, got {}",
            dom.tick_size.0
        ));
    }
    let tick = dom.tick_size.0;
    for pair in dom.rows.windows(2) {
        let a = pair[0].price.0;
        let b = pair[1].price.0;
        if b <= a {
            return Err(format!("DOM rows not strictly ascending: {a} then {b}"));
        }
        let gap = b - a;
        if gap != tick {
            return Err(format!(
                "DOM rows not contiguous by tick_size={tick}: {a} then {b} (gap {gap})"
            ));
        }
    }
    Ok(())
}

fn mismatch_detail(
    generation: u64,
    applied_seq: u64,
    applied_ts: u64,
    m: &VolumeMismatch,
) -> FailureDetail {
    FailureDetail {
        generation,
        applied_seq,
        applied_ts,
        price: m.price.0,
        profile_volume: m.profile_volume,
        dom_volume: m.dom_volume,
        kind: "volume_mismatch",
    }
}

fn write_evidence(path: &Path, evidence: &Evidence) {
    let json = serde_json::to_string_pretty(evidence)
        .unwrap_or_else(|err| panic!("m4-agreement: serialize evidence: {err}"));
    std::fs::write(path, format!("{json}\n"))
        .unwrap_or_else(|err| panic!("m4-agreement: write {}: {err}", path.display()));
    eprintln!("m4-agreement: evidence written to {}", path.display());
}

fn panic_message(payload: &(dyn std::any::Any + Send)) -> String {
    payload
        .downcast_ref::<&str>()
        .map(|msg| (*msg).to_string())
        .or_else(|| payload.downcast_ref::<String>().cloned())
        .unwrap_or_else(|| "engine panic payload was neither &str nor String".into())
}

fn main() {
    let (replay, out_path, smoke) = parse_args();
    // Open/truncate evidence early so a bad --out fails before the run.
    std::fs::File::create(&out_path).unwrap_or_else(|err| {
        panic!(
            "m4-agreement: cannot open evidence file {}: {err}",
            out_path.display()
        )
    });

    let git = GitInfo::capture();
    let date = rfc3339_now();
    let timeout = if smoke { SMOKE_TIMEOUT } else { FULL_TIMEOUT };

    let wake_flag = Arc::new(AtomicBool::new(false));
    let wake = Arc::clone(&wake_flag);
    let handle = EngineService::spawn(
        EngineConfig {
            // Match the shell's visible span so agreement sees the same DOM window.
            visible_tick_span: 256,
        },
        Box::new(move || {
            wake.store(true, Ordering::Release);
        }),
    )
    .unwrap_or_else(|err| panic!("m4-agreement: spawn engine: {err}"));

    let snapshots = handle.snapshots();

    handle
        .send(EngineCmd::SetSource(Source::Replay {
            path: replay.clone(),
        }))
        .unwrap_or_else(|err| panic!("m4-agreement: SetSource: {err}"));
    handle
        .send(EngineCmd::SetSpeed(MAX_SPEED))
        .unwrap_or_else(|err| panic!("m4-agreement: SetSpeed: {err}"));
    handle
        .send(EngineCmd::Play)
        .unwrap_or_else(|err| panic!("m4-agreement: Play: {err}"));

    let started = Instant::now();
    let mut last_seen_gen = 0u64;
    let mut snapshots_checked = 0u64;
    let mut generations_observed = 0u64;
    let mut agreement_failures = 0u64;
    let mut structural_failures = 0u64;
    let mut prices_compared_total = 0u64;
    let mut first_failure: Option<FailureDetail> = None;
    let mut first_structural_msg: Option<String> = None;
    let mut stable_since: Option<Instant> = None;
    let mut last_stable_key = (0u64, 0u64); // (generation, applied_seq)
    let mut last_coverage_applied = 0u64;

    loop {
        if started.elapsed() > timeout {
            eprintln!(
                "m4-agreement: timeout after {:.1}s (smoke={smoke}); shutting down",
                started.elapsed().as_secs_f64()
            );
            break;
        }

        // Wake is advisory; latest-value slot still requires load every loop.
        let _ = wake_flag.swap(false, Ordering::AcqRel);
        let snap = snapshots.load();

        if snap.generation > last_seen_gen {
            generations_observed += 1;
            last_seen_gen = snap.generation;
            last_coverage_applied = snap.coverage.events_applied;
            snapshots_checked += 1;

            match check_dom_structure(&snap.dom) {
                Ok(()) => {}
                Err(msg) => {
                    structural_failures += 1;
                    if first_failure.is_none() {
                        first_failure = Some(FailureDetail {
                            generation: snap.generation,
                            applied_seq: snap.applied_seq,
                            applied_ts: snap.applied_ts,
                            price: 0,
                            profile_volume: 0,
                            dom_volume: 0,
                            kind: "structural",
                        });
                        first_structural_msg = Some(msg);
                    }
                }
            }

            match check_pane_agreement(&snap.profile, &snap.dom) {
                Ok(compared) => {
                    prices_compared_total += compared as u64;
                }
                Err(mismatch) => {
                    agreement_failures += 1;
                    if first_failure.is_none() {
                        first_failure = Some(mismatch_detail(
                            snap.generation,
                            snap.applied_seq,
                            snap.applied_ts,
                            &mismatch,
                        ));
                    }
                }
            }

            stable_since = None;
            last_stable_key = (snap.generation, snap.applied_seq);
        } else {
            // Playback ends when peek returns None → playing=false; publications stop.
            let key = (snap.generation, snap.applied_seq);
            if key != (0, 0) && key == last_stable_key && snap.generation > 0 {
                match stable_since {
                    Some(t) if t.elapsed() >= STABLE_DONE => {
                        if last_coverage_applied > 0 {
                            break;
                        }
                    }
                    Some(_) => {}
                    None => stable_since = Some(Instant::now()),
                }
            } else {
                last_stable_key = key;
                stable_since = Some(Instant::now());
            }
        }

        thread::sleep(Duration::from_micros(50));
    }

    let wall = started.elapsed();
    let engine_exit: EngineExit = match handle.shutdown() {
        Ok(e) => e,
        Err(payload) => {
            let msg = panic_message(&*payload);
            let evidence = Evidence {
                gate: "M4-AGREEMENT-HARNESS",
                date,
                git_sha: git.sha,
                git_dirty: git.dirty,
                log: replay.display().to_string(),
                speed: MAX_SPEED,
                smoke,
                snapshots_checked,
                publications_total: 0,
                generations_observed,
                generations_missed_estimate: 0,
                coverage_fraction: 0.0,
                agreement_failures,
                structural_failures,
                prices_compared_total,
                first_failure,
                exit_events_applied: last_coverage_applied,
                exit_events_read: 0,
                exit_gap_records: 0,
                wall_ms: wall.as_secs_f64() * 1e3,
                notes: Some(format!("engine thread panicked: {msg}")),
                verdict: "FAIL",
            };
            write_evidence(&out_path, &evidence);
            eprintln!("m4-agreement: ENGINE PANIC: {msg}");
            exit(1);
        }
    };

    let publications_total = engine_exit.publications;
    let missed = publications_total.saturating_sub(generations_observed);
    let coverage_fraction = if publications_total == 0 {
        0.0
    } else {
        generations_observed as f64 / publications_total as f64
    };

    let timed_out = wall > timeout;
    let mut notes = Vec::new();
    if smoke {
        notes.push("SMOKE".to_string());
    }
    if timed_out {
        notes.push(format!(
            "stopped on wall timeout {:.1}s (playback may be incomplete)",
            timeout.as_secs_f64()
        ));
    }
    if let Some(msg) = first_structural_msg {
        notes.push(format!("first structural failure: {msg}"));
    }
    notes.push(format!(
        "latest-value slot: observed {generations_observed}/{publications_total} publications \
         (missed ≈ {missed}); agreement checked on observed coherent snapshots only"
    ));

    let verdict = if agreement_failures == 0 && structural_failures == 0 {
        "PASS"
    } else {
        "FAIL"
    };

    let evidence = Evidence {
        gate: "M4-AGREEMENT-HARNESS",
        date,
        git_sha: git.sha,
        git_dirty: git.dirty,
        log: replay.display().to_string(),
        speed: MAX_SPEED,
        smoke,
        snapshots_checked,
        publications_total,
        generations_observed,
        generations_missed_estimate: missed,
        coverage_fraction,
        agreement_failures,
        structural_failures,
        prices_compared_total,
        first_failure,
        exit_events_applied: engine_exit.coverage.events_applied,
        exit_events_read: engine_exit.coverage.events_read,
        exit_gap_records: engine_exit.coverage.gap_records,
        wall_ms: wall.as_secs_f64() * 1e3,
        notes: Some(notes.join("; ")),
        verdict,
    };

    write_evidence(&out_path, &evidence);

    eprintln!(
        "m4-agreement: checked={} publications_total={} coverage={:.4} agreement_failures={} \
         structural_failures={} events_applied={} wall_ms={:.1} verdict={verdict}",
        evidence.snapshots_checked,
        evidence.publications_total,
        evidence.coverage_fraction,
        evidence.agreement_failures,
        evidence.structural_failures,
        evidence.exit_events_applied,
        evidence.wall_ms,
    );

    if verdict != "PASS" {
        exit(1);
    }
}
