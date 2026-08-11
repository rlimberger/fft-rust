//! Headless M5 RSS probe: engine-side week load vs 2 GB VmHWM (PRD §4).
//!
//! `LoadPriorSession` requires earlier trade dates (ENGINE.md §2): current = Fri,
//! priors = Mon…Thu oldest-first. Quiet-box / smoke: see --help.
//!
use std::path::{Path, PathBuf};
use std::process::exit;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use fft_engine::{EngineCmd, EngineConfig, EngineService, Source};
use fft_ui::gate_report::GitInfo;
use serde::Serialize;

/// PRD §4 boring gate.
const RSS_BUDGET_BYTES: u64 = 2 * 1024 * 1024 * 1024;
/// Wall cap for full week prior builds + current-day apply.
const TIMEOUT: Duration = Duration::from_secs(600);
/// Per-prior absolute cap (full-day sliced apply can take tens of seconds).
const PRIOR_TIMEOUT: Duration = Duration::from_secs(180);
/// Stable EOF / prior completion window.
const STABLE: Duration = Duration::from_millis(200);

#[derive(Debug, Clone, Serialize)]
struct Evidence {
    gate: &'static str,
    date: String,
    git_sha: String,
    git_dirty: Option<bool>,
    current: String,
    priors: Vec<String>,
    label: Option<String>,
    sessions_in_snapshot: usize,
    priors_completed: u64,
    prior_skips: u64,
    events_applied: u64,
    vm_hwm_kb: u64,
    vm_hwm_bytes: u64,
    vm_rss_kb: u64,
    budget_bytes: u64,
    under_budget: bool,
    wall_ms: f64,
    notes: Option<String>,
    verdict: &'static str,
}

fn usage(msg: &str) -> ! {
    eprintln!(
        "m5-rss-week: {msg}\n\
         usage: m5-rss-week --current <fftlog> --prior <fftlog>... --out <evidence.json> \
         [--label TEXT] [--replay-at <ts>]\n\
         --prior: earlier trade-date logs (repeatable, oldest first); \
         four priors + Wed current = full sample week"
    );
    exit(2)
}

struct Args {
    current: PathBuf,
    priors: Vec<PathBuf>,
    out: PathBuf,
    label: Option<String>,
    replay_at: Option<u64>,
}

fn parse_args() -> Args {
    let mut args = std::env::args().skip(1);
    let mut current = None;
    let mut priors = Vec::new();
    let mut out = None;
    let mut label = None;
    let mut replay_at = None;
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--current" => {
                current = Some(PathBuf::from(
                    args.next()
                        .unwrap_or_else(|| usage("missing value for --current")),
                ));
            }
            "--prior" => {
                let p = PathBuf::from(
                    args.next()
                        .unwrap_or_else(|| usage("missing value for --prior")),
                );
                if !p.is_file() {
                    usage(&format!("--prior not a file: {}", p.display()));
                }
                priors.push(p);
            }
            "--out" => {
                out = Some(PathBuf::from(
                    args.next()
                        .unwrap_or_else(|| usage("missing value for --out")),
                ));
            }
            "--label" => {
                label = Some(
                    args.next()
                        .unwrap_or_else(|| usage("missing --label value")),
                );
            }
            "--replay-at" => {
                let v = args
                    .next()
                    .unwrap_or_else(|| usage("missing --replay-at value"));
                replay_at =
                    Some(fft_ui::datetime::parse_replay_at(&v).unwrap_or_else(|e| usage(&e)));
            }
            "-h" | "--help" => usage("help"),
            other => usage(&format!("unknown argument {other}")),
        }
    }
    let current = current.unwrap_or_else(|| usage("missing --current"));
    let out = out.unwrap_or_else(|| usage("missing --out"));
    if !current.is_file() {
        usage(&format!("--current not a file: {}", current.display()));
    }
    if priors.is_empty() {
        usage("need at least one --prior (full week wants four other days)");
    }
    Args {
        current,
        priors,
        out,
        label,
        replay_at,
    }
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

/// Linux `/proc/self/status` VmHWM / VmRSS (kB). Fail loud off-Linux.
fn read_vm_status() -> (u64, u64) {
    let text = std::fs::read_to_string("/proc/self/status").unwrap_or_else(|e| {
        panic!("m5-rss-week: read /proc/self/status: {e} (Linux-only probe)");
    });
    let mut hwm = None;
    let mut rss = None;
    for line in text.lines() {
        if let Some(rest) = line.strip_prefix("VmHWM:") {
            hwm = Some(parse_kb(rest, "VmHWM"));
        } else if let Some(rest) = line.strip_prefix("VmRSS:") {
            rss = Some(parse_kb(rest, "VmRSS"));
        }
    }
    (
        hwm.expect("m5-rss-week: VmHWM missing from /proc/self/status"),
        rss.expect("m5-rss-week: VmRSS missing from /proc/self/status"),
    )
}

fn parse_kb(rest: &str, name: &str) -> u64 {
    // "   12345 kB"
    rest.split_whitespace()
        .next()
        .unwrap_or_else(|| panic!("m5-rss-week: empty {name}"))
        .parse::<u64>()
        .unwrap_or_else(|_| panic!("m5-rss-week: bad {name}: {rest}"))
}

fn write_evidence(path: &Path, evidence: &Evidence) {
    let json = serde_json::to_string_pretty(evidence)
        .unwrap_or_else(|err| panic!("m5-rss-week: serialize: {err}"));
    std::fs::write(path, format!("{json}\n"))
        .unwrap_or_else(|err| panic!("m5-rss-week: write {}: {err}", path.display()));
    eprintln!("m5-rss-week: evidence written to {}", path.display());
}

fn panic_message(payload: &(dyn std::any::Any + Send)) -> String {
    payload
        .downcast_ref::<&str>()
        .map(|msg| (*msg).to_string())
        .or_else(|| payload.downcast_ref::<String>().cloned())
        .unwrap_or_else(|| "non-string panic payload".into())
}

fn main() {
    let args = parse_args();
    std::fs::File::create(&args.out).unwrap_or_else(|err| {
        panic!(
            "m5-rss-week: cannot open evidence {}: {err}",
            args.out.display()
        )
    });

    let git = GitInfo::capture();
    let date = rfc3339_now();
    let expected_sessions = 1 + args.priors.len();

    eprintln!(
        "m5-rss-week: current={} priors={} budget={} MiB",
        args.current.display(),
        args.priors.len(),
        RSS_BUDGET_BYTES / (1024 * 1024)
    );

    let wake_flag = Arc::new(AtomicBool::new(false));
    let wake = Arc::clone(&wake_flag);
    let handle = EngineService::spawn(
        EngineConfig {
            visible_tick_span: 256,
        },
        Box::new(move || {
            wake.store(true, Ordering::Release);
        }),
    )
    .unwrap_or_else(|err| panic!("m5-rss-week: spawn: {err}"));

    let snapshots = handle.snapshots();
    handle
        .send(EngineCmd::SetSource(Source::Replay {
            path: args.current.clone(),
        }))
        .unwrap_or_else(|err| panic!("m5-rss-week: SetSource: {err}"));

    if let Some(ts) = args.replay_at {
        handle
            .send(EngineCmd::Seek { ts, generation: 1 })
            .unwrap_or_else(|err| panic!("m5-rss-week: Seek: {err}"));
    }

    // Max speed: finish current session quickly; priors slice on the side.
    handle
        .send(EngineCmd::SetSpeed(1e9))
        .unwrap_or_else(|err| panic!("m5-rss-week: SetSpeed: {err}"));
    handle
        .send(EngineCmd::Play)
        .unwrap_or_else(|err| panic!("m5-rss-week: Play: {err}"));

    let started = Instant::now();
    let mut peak_hwm_kb = 0u64;
    let mut last_applied;
    let mut last_sessions;

    // Wait for the current session to publish once before counting priors.
    // (sessions 0→1 is always current, never a prior.)
    eprintln!("m5-rss-week: waiting for current session publication…");
    loop {
        if started.elapsed() > TIMEOUT {
            panic!("m5-rss-week: timeout waiting for current session");
        }
        let (hwm, _) = read_vm_status();
        peak_hwm_kb = peak_hwm_kb.max(hwm);
        let _ = wake_flag.swap(false, Ordering::AcqRel);
        let snap = snapshots.load();
        last_applied = snap.coverage.events_applied;
        last_sessions = snap.profile.sessions.len();
        if snap.generation > 0 && last_sessions >= 1 {
            eprintln!(
                "m5-rss-week: current published gen={} sessions={last_sessions} applied={last_applied}",
                snap.generation
            );
            break;
        }
        thread::sleep(Duration::from_millis(5));
    }

    // Pause so prior builds monopolize the engine thread (2 ms slices otherwise
    // compete with max-speed forward apply and take minutes per day).
    handle
        .send(EngineCmd::Pause)
        .unwrap_or_else(|err| panic!("m5-rss-week: Pause: {err}"));

    // ENGINE.md §2: one LoadPriorSession at a time (a new command *replaces*
    // in-progress). Issue oldest-first and wait for each complete publication.
    for (i, prior) in args.priors.iter().enumerate() {
        let sessions_before = snapshots.load().profile.sessions.len();
        let target_sessions = sessions_before + 1;
        handle
            .send(EngineCmd::LoadPriorSession {
                path: prior.clone(),
            })
            .unwrap_or_else(|err| panic!("m5-rss-week: LoadPriorSession: {err}"));
        eprintln!(
            "m5-rss-week: loading prior {}/{} {} (sessions_before={sessions_before})",
            i + 1,
            args.priors.len(),
            prior.display()
        );
        let prior_start = Instant::now();
        loop {
            if started.elapsed() > TIMEOUT || prior_start.elapsed() > PRIOR_TIMEOUT {
                eprintln!(
                    "m5-rss-week: timeout during prior {} after {:.1}s (sessions={last_sessions})",
                    prior.display(),
                    prior_start.elapsed().as_secs_f64()
                );
                break;
            }
            let (hwm, _) = read_vm_status();
            peak_hwm_kb = peak_hwm_kb.max(hwm);
            let _ = wake_flag.swap(false, Ordering::AcqRel);
            let snap = snapshots.load();
            last_applied = snap.coverage.events_applied;
            last_sessions = snap.profile.sessions.len();
            if last_sessions >= target_sessions {
                eprintln!(
                    "m5-rss-week: prior {} done in {:.1}s (sessions={last_sessions})",
                    prior.display(),
                    prior_start.elapsed().as_secs_f64()
                );
                break;
            }
            thread::sleep(Duration::from_millis(10));
        }
        if started.elapsed() > TIMEOUT {
            break;
        }
    }

    // Resume max-speed apply so current-day book reaches steady state, then idle.
    handle
        .send(EngineCmd::Play)
        .unwrap_or_else(|err| panic!("m5-rss-week: Play resume: {err}"));

    let mut stable_since: Option<Instant> = None;
    let mut last_key = (0u64, 0usize, 0u64);
    loop {
        if started.elapsed() > TIMEOUT {
            eprintln!(
                "m5-rss-week: timeout after {:.1}s (sessions={last_sessions}/{})",
                started.elapsed().as_secs_f64(),
                expected_sessions
            );
            break;
        }

        let (hwm, _) = read_vm_status();
        peak_hwm_kb = peak_hwm_kb.max(hwm);

        let _ = wake_flag.swap(false, Ordering::AcqRel);
        let snap = snapshots.load();
        last_applied = snap.coverage.events_applied;
        last_sessions = snap.profile.sessions.len();
        let key = (snap.generation, last_sessions, last_applied);

        if snap.generation > 0 {
            if key == last_key {
                match stable_since {
                    Some(t) if t.elapsed() >= STABLE => break,
                    Some(_) => {}
                    None => stable_since = Some(Instant::now()),
                }
            } else {
                last_key = key;
                stable_since = Some(Instant::now());
            }
        }

        thread::sleep(Duration::from_millis(5));
    }

    // Final sample after steady state.
    let (hwm_final, rss_final) = read_vm_status();
    peak_hwm_kb = peak_hwm_kb.max(hwm_final);
    let wall = started.elapsed();

    let engine_exit = match handle.shutdown() {
        Ok(e) => e,
        Err(payload) => {
            let msg = panic_message(&*payload);
            let hwm_b = peak_hwm_kb * 1024;
            let evidence = Evidence {
                gate: "M5-RSS-WEEK",
                date,
                git_sha: git.sha,
                git_dirty: git.dirty,
                current: args.current.display().to_string(),
                priors: args
                    .priors
                    .iter()
                    .map(|p| p.display().to_string())
                    .collect(),
                label: args.label,
                sessions_in_snapshot: last_sessions,
                priors_completed: 0,
                prior_skips: 0,
                events_applied: last_applied,
                vm_hwm_kb: peak_hwm_kb,
                vm_hwm_bytes: hwm_b,
                vm_rss_kb: rss_final,
                budget_bytes: RSS_BUDGET_BYTES,
                under_budget: hwm_b < RSS_BUDGET_BYTES,
                wall_ms: wall.as_secs_f64() * 1e3,
                notes: Some(format!("engine panic: {msg}")),
                verdict: "FAIL",
            };
            write_evidence(&args.out, &evidence);
            eprintln!("m5-rss-week: ENGINE PANIC: {msg}");
            exit(1);
        }
    };

    // Post-shutdown peak (join may free; HWM is high-water so stays).
    let (hwm_post, _) = read_vm_status();
    peak_hwm_kb = peak_hwm_kb.max(hwm_post);
    let hwm_bytes = peak_hwm_kb * 1024;
    let under = hwm_bytes < RSS_BUDGET_BYTES;

    let mut notes = Vec::new();
    if let Some(l) = &args.label {
        notes.push(l.clone());
    }
    notes.push(format!(
        "sessions={last_sessions} expected={expected_sessions}; \
         priors_completed={} prior_skips={}; VmHWM={peak_hwm_kb} kB ({:.2} MiB) vs budget {:.0} MiB",
        engine_exit.priors_completed,
        engine_exit.prior_skips,
        peak_hwm_kb as f64 / 1024.0,
        RSS_BUDGET_BYTES as f64 / (1024.0 * 1024.0)
    ));
    if last_sessions < expected_sessions {
        notes.push(format!(
            "incomplete: only {last_sessions}/{expected_sessions} sessions in snapshot"
        ));
    }
    if engine_exit.prior_skips > 0 {
        notes.push(format!(
            "prior_skips={} (loud skips — check trade dates / instrument)",
            engine_exit.prior_skips
        ));
    }

    // PASS only when the full week is present and peak RSS is under budget.
    let loaded = last_sessions >= expected_sessions && engine_exit.prior_skips == 0;
    let verdict = if loaded && under { "PASS" } else { "FAIL" };

    let evidence = Evidence {
        gate: "M5-RSS-WEEK",
        date,
        git_sha: git.sha,
        git_dirty: git.dirty,
        current: args.current.display().to_string(),
        priors: args
            .priors
            .iter()
            .map(|p| p.display().to_string())
            .collect(),
        label: args.label,
        sessions_in_snapshot: last_sessions,
        priors_completed: engine_exit.priors_completed,
        prior_skips: engine_exit.prior_skips,
        events_applied: engine_exit.coverage.events_applied,
        vm_hwm_kb: peak_hwm_kb,
        vm_hwm_bytes: hwm_bytes,
        vm_rss_kb: rss_final,
        budget_bytes: RSS_BUDGET_BYTES,
        under_budget: under,
        wall_ms: wall.as_secs_f64() * 1e3,
        notes: Some(notes.join("; ")),
        verdict,
    };
    write_evidence(&args.out, &evidence);

    eprintln!(
        "m5-rss-week: sessions={} priors_completed={} prior_skips={} events_applied={} \
         VmHWM={} kB ({:.2} MiB) budget=2048 MiB under={under} wall_ms={:.1} verdict={verdict}",
        evidence.sessions_in_snapshot,
        evidence.priors_completed,
        evidence.prior_skips,
        evidence.events_applied,
        evidence.vm_hwm_kb,
        evidence.vm_hwm_kb as f64 / 1024.0,
        evidence.wall_ms,
    );

    if verdict != "PASS" {
        exit(1);
    }
}
