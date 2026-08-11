//! Headless M5 scrub-burst harness: seek service under 60 Hz drag load.
//!
//! Spawns the real [`EngineService`] on a checkpointed session log, issues a
//! realistic scrub burst (default 120 Seeks over 2 s wall ≈ 60/s) with monotonic
//! generations sweeping timestamps across the session, and polls the snapshot
//! slot. Asserts:
//! - every *answered* `seek_generation` is strictly monotonic,
//! - the final answered generation is the last issued (latest-wins lands),
//! - the engine never wedges (progress within timeout).
//!
//! Frame timing is the GUI gate; this proves the SEEK SERVICE under drag load.
//! Latest-wins means most issued seeks are legitimately skipped — report the
//! answered/issued ratio.
//!
//! Quiet-box (full):
//! ```text
//! cargo run --release -p fft-ui --bin m5-scrub-burst -- \
//!   --replay /tmp/esu6-wed-v3-ckpt.fftlog \
//!   --out perf-runner/results/<date>-m5-scrub-burst.json
//! ```
//!
//! Smoke:
//! ```text
//! cargo run --release -p fft-ui --bin m5-scrub-burst -- \
//!   --replay /tmp/esu6-wed-v3-ckpt.fftlog \
//!   --seeks 24 --burst-ms 400 --label SMOKE \
//!   --out /tmp/m5-scrub-burst-smoke.json
//! ```

use std::path::{Path, PathBuf};
use std::process::exit;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use fft_engine::{EngineCmd, EngineConfig, EngineService, Source};
use fft_log::{KIND_EVENTS, LogReader};
use fft_ui::gate_report::GitInfo;
use serde::Serialize;

/// Default: 120 seeks / 2 s = 60 Hz scrub drag.
const DEFAULT_SEEKS: u32 = 120;
const DEFAULT_BURST_MS: u64 = 2_000;
/// After last seek issued, wait this long for the final answer before FAIL.
const SETTLE_TIMEOUT: Duration = Duration::from_secs(30);
/// No answered-gen progress for this long ⇒ wedge FAIL.
const WEDGE_TIMEOUT: Duration = Duration::from_secs(10);
/// First UI seek gen after shell's optional --replay-at (gen 1).
const FIRST_GEN: u64 = 2;

#[derive(Debug, Clone, Serialize)]
struct Evidence {
    gate: &'static str,
    date: String,
    git_sha: String,
    git_dirty: Option<bool>,
    log: String,
    label: Option<String>,
    seeks_issued: u32,
    seeks_answered: u32,
    answered_issued_ratio: f64,
    burst_ms: u64,
    first_ts: u64,
    last_ts: u64,
    first_issued_gen: u64,
    last_issued_gen: u64,
    last_answered_gen: u64,
    final_answered_is_last_issued: bool,
    answered_gens_monotonic: bool,
    wedge: bool,
    wall_ms: f64,
    settle_ms: f64,
    seeks_executed_engine: u64,
    notes: Option<String>,
    verdict: &'static str,
}

fn usage(msg: &str) -> ! {
    eprintln!(
        "m5-scrub-burst: {msg}\n\
         usage: m5-scrub-burst --replay <ckpt.fftlog> --out <evidence.json> \
         [--seeks N] [--burst-ms MS] [--label TEXT]\n\
         requires a checkpointed fftlog (Seek panics otherwise)"
    );
    exit(2)
}

struct Args {
    replay: PathBuf,
    out: PathBuf,
    seeks: u32,
    burst_ms: u64,
    label: Option<String>,
}

fn parse_args() -> Args {
    let mut args = std::env::args().skip(1);
    let mut replay = None;
    let mut out = None;
    let mut seeks = DEFAULT_SEEKS;
    let mut burst_ms = DEFAULT_BURST_MS;
    let mut label = None;
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
            "--seeks" => {
                seeks = args
                    .next()
                    .unwrap_or_else(|| usage("missing --seeks value"))
                    .parse()
                    .unwrap_or_else(|_| usage("--seeks must be a positive integer"));
                if seeks == 0 {
                    usage("--seeks must be > 0");
                }
            }
            "--burst-ms" => {
                burst_ms = args
                    .next()
                    .unwrap_or_else(|| usage("missing --burst-ms value"))
                    .parse()
                    .unwrap_or_else(|_| usage("--burst-ms must be a positive integer"));
                if burst_ms == 0 {
                    usage("--burst-ms must be > 0");
                }
            }
            "--label" => {
                label = Some(
                    args.next()
                        .unwrap_or_else(|| usage("missing --label value")),
                );
            }
            "-h" | "--help" => usage("help"),
            other => usage(&format!("unknown argument {other}")),
        }
    }
    let replay = replay.unwrap_or_else(|| usage("missing --replay"));
    let out = out.unwrap_or_else(|| usage("missing --out"));
    if !replay.is_file() {
        usage(&format!("replay log not found: {}", replay.display()));
    }
    Args {
        replay,
        out,
        seeks,
        burst_ms,
        label,
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

fn event_time_bounds(path: &Path) -> (u64, u64, usize) {
    let (reader, report) = LogReader::open(path).unwrap_or_else(|e| {
        panic!("m5-scrub-burst: open {}: {e}", path.display());
    });
    for w in &report.warnings {
        eprintln!("m5-scrub-burst: open warning: {w}");
    }
    let mut first_ts = None;
    let mut last_ts = 0u64;
    let mut ckpts = 0usize;
    for i in 0..reader.frame_count() {
        let fh = reader
            .frame_header(i)
            .unwrap_or_else(|e| panic!("m5-scrub-burst: frame_header({i}): {e}"));
        if fh.kind == KIND_EVENTS {
            if first_ts.is_none() {
                first_ts = Some(fh.first_ts);
            }
            last_ts = fh.last_ts;
        } else if fh.kind == fft_log::KIND_CHECKPOINT {
            ckpts += 1;
        }
    }
    let first_ts = first_ts
        .unwrap_or_else(|| panic!("m5-scrub-burst: no EVENTS frames in {}", path.display()));
    if last_ts < first_ts {
        panic!("m5-scrub-burst: last_ts < first_ts");
    }
    if ckpts == 0 {
        panic!(
            "m5-scrub-burst: zero checkpoints in {} — run fft-checkpoint first",
            path.display()
        );
    }
    (first_ts, last_ts, ckpts)
}

/// Monotonic timestamp targets sweeping first→last across `n` samples.
fn sweep_targets(first: u64, last: u64, n: u32) -> Vec<u64> {
    if n == 1 {
        return vec![first];
    }
    let span = last.saturating_sub(first);
    (0..n)
        .map(|i| {
            let num = u128::from(span) * u128::from(i);
            let den = u128::from(n - 1);
            first + (num / den) as u64
        })
        .collect()
}

fn write_evidence(path: &Path, evidence: &Evidence) {
    let json = serde_json::to_string_pretty(evidence)
        .unwrap_or_else(|err| panic!("m5-scrub-burst: serialize: {err}"));
    std::fs::write(path, format!("{json}\n"))
        .unwrap_or_else(|err| panic!("m5-scrub-burst: write {}: {err}", path.display()));
    eprintln!("m5-scrub-burst: evidence written to {}", path.display());
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
            "m5-scrub-burst: cannot open evidence {}: {err}",
            args.out.display()
        )
    });

    let git = GitInfo::capture();
    let date = rfc3339_now();
    let (first_ts, last_ts, ckpts) = event_time_bounds(&args.replay);
    eprintln!(
        "m5-scrub-burst: log={} first_ts={first_ts} last_ts={last_ts} checkpoints={ckpts} \
         seeks={} burst_ms={}",
        args.replay.display(),
        args.seeks,
        args.burst_ms
    );

    let targets = sweep_targets(first_ts, last_ts, args.seeks);
    let first_issued_gen = FIRST_GEN;
    let last_issued_gen = FIRST_GEN + u64::from(args.seeks) - 1;

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
    .unwrap_or_else(|err| panic!("m5-scrub-burst: spawn engine: {err}"));

    let snapshots = handle.snapshots();
    handle
        .send(EngineCmd::SetSource(Source::Replay {
            path: args.replay.clone(),
        }))
        .unwrap_or_else(|err| panic!("m5-scrub-burst: SetSource: {err}"));
    // Pause at open — scrub-only load; no Play.
    handle
        .send(EngineCmd::Pause)
        .unwrap_or_else(|err| panic!("m5-scrub-burst: Pause: {err}"));

    let started = Instant::now();
    let burst = Duration::from_millis(args.burst_ms);
    let interval = burst / args.seeks.max(1);

    let mut answered_gens: Vec<u64> = Vec::new();
    let mut last_answered = 0u64;
    let mut monotonic = true;
    let mut wedge = false;
    let mut last_progress = Instant::now();
    let mut issued = 0u32;
    let mut next_issue = Instant::now();

    // Issue phase + concurrent poll.
    while issued < args.seeks {
        let now = Instant::now();
        if now >= next_issue {
            let i = issued;
            let ts = targets[i as usize];
            let generation = first_issued_gen + u64::from(i);
            handle
                .send(EngineCmd::Seek { ts, generation })
                .unwrap_or_else(|err| panic!("m5-scrub-burst: Seek gen={generation}: {err}"));
            issued += 1;
            next_issue = started + interval * issued;
            // Keep pace even if we fell behind (realistic drag: one per tick).
            if next_issue < Instant::now() {
                next_issue = Instant::now();
            }
        }

        let _ = wake_flag.swap(false, Ordering::AcqRel);
        let snap = snapshots.load();
        // Observed seek_generation must strictly increase (latest-wins may skip gens).
        if snap.seek_generation > last_answered {
            answered_gens.push(snap.seek_generation);
            last_answered = snap.seek_generation;
            last_progress = Instant::now();
        } else if last_answered == 0 && issued > 0 && last_progress.elapsed() > WEDGE_TIMEOUT {
            wedge = true;
            eprintln!("m5-scrub-burst: WEDGE — no seek answered within {WEDGE_TIMEOUT:?}");
            break;
        }

        thread::sleep(Duration::from_micros(100));
    }

    // Settle: wait for final generation (latest-wins must land last issued).
    let settle_start = Instant::now();
    while !wedge && last_answered < last_issued_gen {
        if settle_start.elapsed() > SETTLE_TIMEOUT {
            eprintln!(
                "m5-scrub-burst: settle timeout after {:.1}s; last_answered={last_answered} \
                 want {last_issued_gen}",
                settle_start.elapsed().as_secs_f64()
            );
            wedge = true;
            break;
        }
        if last_progress.elapsed() > WEDGE_TIMEOUT {
            eprintln!(
                "m5-scrub-burst: WEDGE during settle — no progress for {WEDGE_TIMEOUT:?} \
                 (answered={last_answered})"
            );
            wedge = true;
            break;
        }
        let _ = wake_flag.swap(false, Ordering::AcqRel);
        let snap = snapshots.load();
        if snap.seek_generation > last_answered {
            answered_gens.push(snap.seek_generation);
            last_answered = snap.seek_generation;
            last_progress = Instant::now();
        }
        thread::sleep(Duration::from_micros(100));
    }
    let settle_ms = settle_start.elapsed().as_secs_f64() * 1e3;
    let wall = started.elapsed();

    // Observed answered generations must be strictly increasing.
    for w in answered_gens.windows(2) {
        if w[1] <= w[0] {
            monotonic = false;
        }
    }

    let seeks_answered = answered_gens.len() as u32;
    let ratio = if issued == 0 {
        0.0
    } else {
        f64::from(seeks_answered) / f64::from(issued)
    };
    let final_ok = last_answered == last_issued_gen;

    let engine_exit = match handle.shutdown() {
        Ok(e) => e,
        Err(payload) => {
            let msg = panic_message(&*payload);
            let evidence = Evidence {
                gate: "M5-SCRUB-BURST",
                date,
                git_sha: git.sha,
                git_dirty: git.dirty,
                log: args.replay.display().to_string(),
                label: args.label,
                seeks_issued: issued,
                seeks_answered,
                answered_issued_ratio: ratio,
                burst_ms: args.burst_ms,
                first_ts,
                last_ts,
                first_issued_gen,
                last_issued_gen,
                last_answered_gen: last_answered,
                final_answered_is_last_issued: final_ok,
                answered_gens_monotonic: monotonic,
                wedge: true,
                wall_ms: wall.as_secs_f64() * 1e3,
                settle_ms,
                seeks_executed_engine: 0,
                notes: Some(format!("engine panic: {msg}")),
                verdict: "FAIL",
            };
            write_evidence(&args.out, &evidence);
            eprintln!("m5-scrub-burst: ENGINE PANIC: {msg}");
            exit(1);
        }
    };

    let mut notes = Vec::new();
    if let Some(l) = &args.label {
        notes.push(l.clone());
    }
    notes.push(format!(
        "latest-wins: answered {seeks_answered}/{issued} (ratio {ratio:.4}); \
         engine seeks_executed={}",
        engine_exit.seeks_executed
    ));
    if wedge {
        notes.push("wedge/timeout".into());
    }

    let verdict = if !wedge && final_ok && monotonic {
        "PASS"
    } else {
        "FAIL"
    };

    let evidence = Evidence {
        gate: "M5-SCRUB-BURST",
        date,
        git_sha: git.sha,
        git_dirty: git.dirty,
        log: args.replay.display().to_string(),
        label: args.label,
        seeks_issued: issued,
        seeks_answered,
        answered_issued_ratio: ratio,
        burst_ms: args.burst_ms,
        first_ts,
        last_ts,
        first_issued_gen,
        last_issued_gen,
        last_answered_gen: last_answered,
        final_answered_is_last_issued: final_ok,
        answered_gens_monotonic: monotonic,
        wedge,
        wall_ms: wall.as_secs_f64() * 1e3,
        settle_ms,
        seeks_executed_engine: engine_exit.seeks_executed,
        notes: Some(notes.join("; ")),
        verdict,
    };
    write_evidence(&args.out, &evidence);

    eprintln!(
        "m5-scrub-burst: issued={issued} answered={seeks_answered} ratio={ratio:.4} \
         last_answered={last_answered}/{last_issued_gen} mono={monotonic} wedge={wedge} \
         seeks_executed={} wall_ms={:.1} verdict={verdict}",
        engine_exit.seeks_executed, evidence.wall_ms,
    );

    if verdict != "PASS" {
        exit(1);
    }
}
