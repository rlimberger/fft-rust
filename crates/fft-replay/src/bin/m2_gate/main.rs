//! M2 seek / bit-identity / throughput gate harness.
//!
//! Measures checkpoint-restore seeks against a checkpointed fftlog, verifies a
//! subset of seeks bit-identical to forward replay (six serialized sections),
//! and times one full forward pass for realtime multiple.
//!
//! Requires a checkpointed log (`fft-checkpoint`). Zero checkpoints → panic
//! with remediation (same frozen contract as the engine).
//!
//! Quiet-box (full gate):
//! ```text
//! cargo run --release -p fft-replay --bin m2-gate -- \
//!   --log /tmp/esu6-wed-v3-ckpt.fftlog \
//!   --seeks 1000 --verify 25 --seed 0x4d325345454b \
//!   --out perf-runner/results/<date>-m2-seek-gate.json
//! ```
//!
//! Smoke (this machine; not a gate claim):
//! ```text
//! cargo run --release -p fft-replay --bin m2-gate -- \
//!   --log /tmp/esu6-wed-v3-ckpt.fftlog \
//!   --seeks 25 --verify 3 --label SMOKE \
//!   --out /tmp/m2-gate-smoke.json
//! ```

mod report;
mod run;

use report::{Budgets, DistMs, Report, git_info, rfc3339_now};
use run::{
    P95_BUDGET_MS, REALTIME_BUDGET, decide_verdict, inspect_log, run_seeks, run_throughput,
    run_verify, sample_targets,
};
use std::env;
use std::fs::File;
use std::io::Write;
use std::path::PathBuf;
use std::process::exit;

/// Default seek count (M2 gate letter).
const DEFAULT_SEEKS: usize = 1000;
/// Default bit-identity subset (full forward replay per target is expensive).
const DEFAULT_VERIFY: usize = 25;
/// Deterministic default seed ("M2SEEK" as hex-ish constant).
const DEFAULT_SEED: u64 = 0x4d32_5345_454b;

const METHODOLOGY: &str = "\
COLD: for each target ts sampled uniformly in [first_ts, last_ts] via xorshift64*(seed), \
open one long-lived ReplaySource on the checkpointed log; allocate fresh Book+MultiProfile; \
wall-time ReplaySource::seek(target, book, profile, || false) — checkpoint restore + tail \
through target_ts (events with ts ≤ target). \
WARM: immediately re-seek the same target on the same source into a second fresh Book+\
MultiProfile (mmap/checkpoint pages hot; no process restart). \
Bit-identity: for the first --verify targets, independent seek vs forward-from-open to the \
same target_ts; compare serialize_book/flow/refresh + MultiProfile::serialize (PROFILE/CVD/\
SESSION) byte vectors; any mismatch is FAIL with the first differing section named. \
Throughput: one full apply_next pass; realtime_multiple = (last_ts-first_ts)/wall. \
Harness requires checkpoint_count > 0 (mirrors engine loud reject).";

fn usage(msg: &str) -> ! {
    eprintln!(
        "m2-gate: {msg}\n\
         usage: m2-gate --log <ckpt.fftlog> [--seeks N] [--verify M] [--seed U64] \
         [--out path.json] [--label TEXT]\n\
         requires a checkpointed fftlog (run fft-checkpoint first); \
         seek on a checkpoint-less log is rejected loudly"
    );
    exit(2)
}

struct Args {
    log: PathBuf,
    seeks: usize,
    verify: usize,
    seed: u64,
    out: Option<PathBuf>,
    label: Option<String>,
}

fn parse_args() -> Args {
    let mut log = None;
    let mut seeks = DEFAULT_SEEKS;
    let mut verify = DEFAULT_VERIFY;
    let mut seed = DEFAULT_SEED;
    let mut out = None;
    let mut label = None;
    let mut args = env::args().skip(1);
    while let Some(a) = args.next() {
        match a.as_str() {
            "--log" => {
                log = Some(PathBuf::from(
                    args.next().unwrap_or_else(|| usage("missing --log path")),
                ));
            }
            "--seeks" => {
                seeks = args
                    .next()
                    .unwrap_or_else(|| usage("missing --seeks value"))
                    .parse()
                    .unwrap_or_else(|_| usage("--seeks must be a positive integer"));
                if seeks == 0 {
                    usage("--seeks must be ≥ 1");
                }
            }
            "--verify" => {
                verify = args
                    .next()
                    .unwrap_or_else(|| usage("missing --verify value"))
                    .parse()
                    .unwrap_or_else(|_| usage("--verify must be a non-negative integer"));
            }
            "--seed" => {
                let raw = args.next().unwrap_or_else(|| usage("missing --seed value"));
                seed =
                    parse_u64(&raw).unwrap_or_else(|| usage("--seed must be u64 (dec or 0xhex)"));
            }
            "--out" => {
                out = Some(PathBuf::from(
                    args.next().unwrap_or_else(|| usage("missing --out path")),
                ));
            }
            "--label" => {
                label = Some(
                    args.next()
                        .unwrap_or_else(|| usage("missing --label value")),
                );
            }
            "-h" | "--help" => usage("help"),
            other => usage(&format!("unknown argument: {other}")),
        }
    }
    let log = log.unwrap_or_else(|| usage("missing required --log"));
    Args {
        log,
        seeks,
        verify,
        seed,
        out,
        label,
    }
}

fn parse_u64(s: &str) -> Option<u64> {
    if let Some(hex) = s.strip_prefix("0x").or_else(|| s.strip_prefix("0X")) {
        u64::from_str_radix(hex, 16).ok()
    } else {
        s.parse().ok()
    }
}

fn main() {
    let args = parse_args();
    if !args.log.exists() {
        panic!(
            "m2-gate: log not found: {}\n\
             Regenerate (HANDOFF volatile fixtures):\n\
               cargo run --release -p fft-ingest -- write /tmp/esu6-wed-v3.fftlog \\\n\
                 data/GLBX-20260803-4WJS899FNL/*.mbo.dbn.zst --trade-date 2026-07-29 \\\n\
                 --tick 250000000 --uom-qty 50000000000 --display-factor 1\n\
               cargo run --release -p fft-engine --bin fft-checkpoint -- \\\n\
                 /tmp/esu6-wed-v3.fftlog /tmp/esu6-wed-v3-ckpt.fftlog",
            args.log.display()
        );
    }

    let (git_sha, git_dirty) = git_info();
    let date = rfc3339_now();
    let binary = env::args().collect::<Vec<_>>().join(" ");

    let (log_info, ckpt) = inspect_log(&args.log);
    if ckpt == 0 {
        panic!(
            "m2-gate: log has zero CHECKPOINT frames — Seek is forbidden on a checkpoint-less log.\n\
             Run `fft-checkpoint {} <checkpointed.fftlog>` and pass the -ckpt copy.",
            args.log.display()
        );
    }
    eprintln!(
        "m2-gate: log={} events={} checkpoints={} frames={} ts=[{}, {}] span={:.1}s",
        log_info.path,
        log_info.event_count,
        log_info.checkpoint_count,
        log_info.frame_count,
        log_info.first_ts,
        log_info.last_ts,
        log_info.event_time_span_s,
    );

    let targets = sample_targets(args.seed, args.seeks, log_info.first_ts, log_info.last_ts);
    eprintln!(
        "m2-gate: sampling {} seeks seed=0x{:x} verify={}",
        args.seeks, args.seed, args.verify
    );

    let (cold_t, warm_t, outcomes) = run_seeks(&args.log, &targets);
    let cold = DistMs::from_durations(cold_t);
    let warm = DistMs::from_durations(warm_t);
    eprintln!(
        "m2-gate: cold p50={:.3} p95={:.3} p99={:.3} max={:.3} ms",
        cold.p50_ms, cold.p95_ms, cold.p99_ms, cold.max_ms
    );
    eprintln!(
        "m2-gate: warm p50={:.3} p95={:.3} p99={:.3} max={:.3} ms",
        warm.p50_ms, warm.p95_ms, warm.p99_ms, warm.max_ms
    );

    let verify = run_verify(&args.log, &outcomes, args.verify);
    eprintln!(
        "m2-gate: verify passed={}/{} failed={}",
        verify.passed, verify.count, verify.failed
    );

    let throughput = run_throughput(&args.log, log_info.event_time_span_s);
    eprintln!(
        "m2-gate: throughput events={} wall={:.3}s eps={:.0} realtime={:.1}×",
        throughput.events,
        throughput.wall_s,
        throughput.events_per_sec,
        throughput.realtime_multiple
    );

    let (verdict, notes) =
        decide_verdict(args.label.as_deref(), &cold, &warm, &verify, &throughput);

    let gate_name = match args.label.as_deref() {
        Some(l) if l.eq_ignore_ascii_case("SMOKE") => {
            "M2 seek harness — SMOKE (not a gate claim)".to_string()
        }
        Some(l) => format!("M2 seek gate — {l}"),
        None => "M2 seek gate — p95≤250ms cold/warm, bit-identity, ≥60× realtime".to_string(),
    };

    let report = Report {
        gate: gate_name,
        date,
        binary,
        git_sha,
        git_dirty,
        label: args.label.clone(),
        log: log_info,
        seed: args.seed,
        seeks: args.seeks,
        methodology: METHODOLOGY.to_string(),
        cold,
        warm,
        verify,
        throughput,
        budgets: Budgets {
            seek_p95_ms: P95_BUDGET_MS,
            realtime_multiple: REALTIME_BUDGET,
        },
        verdict: verdict.clone(),
        notes,
    };

    let json = serde_json::to_string_pretty(&report).expect("serialize report");
    if let Some(out) = &args.out {
        if let Some(parent) = out.parent()
            && !parent.as_os_str().is_empty()
        {
            std::fs::create_dir_all(parent).unwrap_or_else(|e| {
                panic!("m2-gate: create_dir_all {}: {e}", parent.display());
            });
        }
        let mut f = File::create(out).unwrap_or_else(|e| {
            panic!("m2-gate: create {}: {e}", out.display());
        });
        f.write_all(json.as_bytes()).expect("write report");
        f.write_all(b"\n").ok();
        eprintln!("m2-gate: wrote {}", out.display());
    } else {
        println!("{json}");
    }

    eprintln!("m2-gate: verdict={verdict}");
    if verdict == "FAIL" {
        exit(1);
    }
}
