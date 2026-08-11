//! M1 data-plane gate evidence harness (IMPLEMENTATION-PLAN.md "M1 — Data plane").
//!
//! Headless: consumes pre-ingested fftlog v2 files (ingest stays on `fft-ingest write`),
//! measures (b) busiest-day book+profile apply time, (c) N-chunk ≡ one-shot differential
//! on real data, (d) bytes/event (and legacy size ratio when legacy logs are readable).
//!
//! Quiet-box runs produce claimable numbers; concurrent-build runs must pass `--smoke`
//! so the evidence blob labels timings SMOKE (builds pollute wall-clock).
//!
//! ```text
//! m1-gate --out <evidence.json> [--smoke] [--seed N] [--legacy-dir DIR] [--diff-trials N] \
//!         <day1.fftlog> [day2.fftlog ...]
//! ```

mod report;
mod run;

use report::{DayStat, DiffTrial, capture_git_sha, render_json, utc_date_string};
use run::{XorShift64, apply_chunked, apply_oneshot, inspect_day, split_sizes};
use std::fs::{self, File};
use std::io::Write;
use std::path::PathBuf;
use std::process::exit;

const DEFAULT_DIFF_TRIALS: usize = 7;
const DEFAULT_SEED: u64 = 0xC0FF_EE42_A1A7_E011;

fn usage(msg: &str) -> ! {
    eprintln!(
        "m1-gate: {msg}\n\
         usage: m1-gate --out <evidence.json> [--smoke] [--seed N] [--legacy-dir DIR] \
         [--diff-trials N] <day.fftlog>..."
    );
    exit(2);
}

fn main() {
    let mut out: Option<PathBuf> = None;
    let mut smoke = false;
    let mut seed = DEFAULT_SEED;
    let mut legacy_dir: Option<PathBuf> = None;
    let mut diff_trials = DEFAULT_DIFF_TRIALS;
    let mut logs: Vec<PathBuf> = Vec::new();

    let mut args = std::env::args().skip(1).peekable();
    while let Some(a) = args.next() {
        match a.as_str() {
            "--out" => {
                out = Some(PathBuf::from(
                    args.next().unwrap_or_else(|| usage("missing --out path")),
                ));
            }
            "--smoke" => smoke = true,
            "--seed" => {
                let s = args.next().unwrap_or_else(|| usage("missing --seed value"));
                seed = parse_u64(&s, "--seed");
            }
            "--legacy-dir" => {
                legacy_dir = Some(PathBuf::from(
                    args.next()
                        .unwrap_or_else(|| usage("missing --legacy-dir path")),
                ));
            }
            "--diff-trials" => {
                let s = args
                    .next()
                    .unwrap_or_else(|| usage("missing --diff-trials value"));
                diff_trials = parse_u64(&s, "--diff-trials") as usize;
                if diff_trials == 0 {
                    usage("--diff-trials must be ≥ 1");
                }
            }
            "-h" | "--help" => usage("help"),
            flag if flag.starts_with('-') => usage(&format!("unknown flag {flag}")),
            path => logs.push(PathBuf::from(path)),
        }
    }

    let out = out.unwrap_or_else(|| usage("missing --out"));
    if logs.is_empty() {
        usage("need at least one .fftlog path");
    }

    let git_sha = capture_git_sha();
    let date_utc = utc_date_string();
    let timing_label = if smoke { "SMOKE" } else { "QUIET_BOX" };

    eprintln!(
        "m1-gate: {} log(s), seed={seed:#x}, diff_trials={diff_trials}, label={timing_label}, git={git_sha}",
        logs.len()
    );

    let mut days: Vec<DayStat> = Vec::with_capacity(logs.len());
    for path in &logs {
        match inspect_day(path, legacy_dir.as_deref()) {
            Ok(d) => {
                eprintln!(
                    "  day {} {} events={} bytes={} B/ev={:.3} legacy={}",
                    d.trade_date_ymd,
                    d.symbol,
                    d.event_count,
                    d.file_bytes,
                    d.bytes_per_event,
                    d.legacy_status
                );
                days.push(d);
            }
            Err(e) => {
                eprintln!("m1-gate: inspect {}: {e}", path.display());
                exit(1);
            }
        }
    }

    let busiest_idx = days
        .iter()
        .enumerate()
        .max_by_key(|(_, d)| d.event_count)
        .map(|(i, _)| i)
        .expect("non-empty days");
    let busiest = &days[busiest_idx];
    eprintln!(
        "m1-gate: busiest day {} ({} events) — forward apply…",
        busiest.trade_date_ymd, busiest.event_count
    );

    let oneshot = match apply_oneshot(&busiest.path) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("m1-gate: oneshot apply failed: {e}");
            exit(1);
        }
    };
    if oneshot.events != busiest.event_count {
        eprintln!(
            "m1-gate: WARN oneshot events {} ≠ inspected {}",
            oneshot.events, busiest.event_count
        );
    }
    eprintln!(
        "m1-gate: oneshot apply {:.6}s for {} events ({timing_label})",
        oneshot.seconds, oneshot.events
    );

    // ≤ 3 s: revised budget (René 2026-08-11, IMPLEMENTATION-PLAN M1 gate) — the
    // original < 2 s predates per-event refresh/flow/profile state.
    let apply_budget_s = 3.0_f64;
    let apply_pass = oneshot.seconds <= apply_budget_s;
    eprintln!(
        "m1-gate: apply gate (<= {apply_budget_s} s): {} ({timing_label})",
        if apply_pass { "PASS" } else { "FAIL" }
    );

    eprintln!("m1-gate: differential {diff_trials} random chunk splits…");
    let mut rng = XorShift64::new(seed);
    let mut trials: Vec<DiffTrial> = Vec::with_capacity(diff_trials);
    let mut all_match = true;
    for t in 0..diff_trials {
        let n_chunks = 2 + (rng.next() as usize % 15); // 2..=16
        let chunk_sizes = split_sizes(oneshot.events as usize, n_chunks, &mut rng);
        let trial = match apply_chunked(&busiest.path, &chunk_sizes, &oneshot) {
            Ok(mut tr) => {
                tr.trial = t;
                tr.n_chunks = chunk_sizes.len();
                tr.chunk_sizes = chunk_sizes;
                tr
            }
            Err(e) => DiffTrial {
                trial: t,
                n_chunks: chunk_sizes.len(),
                chunk_sizes,
                match_oneshot: false,
                seconds: 0.0,
                fail_reason: Some(e),
            },
        };
        if !trial.match_oneshot {
            all_match = false;
        }
        eprintln!(
            "  trial {t}: chunks={} sizes={:?} match={} {:.6}s{}",
            trial.n_chunks,
            trial.chunk_sizes,
            trial.match_oneshot,
            trial.seconds,
            trial
                .fail_reason
                .as_ref()
                .map(|r| format!(" reason={r}"))
                .unwrap_or_default()
        );
        trials.push(trial);
    }
    eprintln!(
        "m1-gate: differential verdict: {}",
        if all_match { "PASS" } else { "FAIL" }
    );

    let mut size_ratio_unverified = false;
    for d in &days {
        if d.legacy_ratio.is_none() {
            size_ratio_unverified = true;
        }
    }
    let size_claim = if size_ratio_unverified {
        "UNVERIFIED"
    } else {
        let max_ratio = days
            .iter()
            .filter_map(|d| d.legacy_ratio)
            .fold(0.0_f64, f64::max);
        if max_ratio <= 0.5 { "PASS" } else { "FAIL" }
    };

    let json = render_json(
        &git_sha,
        &date_utc,
        timing_label,
        smoke,
        seed,
        &days,
        busiest,
        &oneshot,
        apply_pass,
        apply_budget_s,
        &trials,
        all_match,
        size_claim,
        size_ratio_unverified,
    );

    if let Some(parent) = out.parent()
        && !parent.as_os_str().is_empty()
    {
        fs::create_dir_all(parent).unwrap_or_else(|e| {
            eprintln!("m1-gate: create {}: {e}", parent.display());
            exit(1);
        });
    }
    let mut f = File::create(&out).unwrap_or_else(|e| {
        eprintln!("m1-gate: write {}: {e}", out.display());
        exit(1);
    });
    f.write_all(json.as_bytes()).unwrap_or_else(|e| {
        eprintln!("m1-gate: write {}: {e}", out.display());
        exit(1);
    });
    eprintln!("m1-gate: wrote {}", out.display());

    // Non-zero exit only on hard failures (I/O already exited). Timing FAIL under
    // --smoke is still exit 0 so CI smoke can land; quiet-box FAIL exits 1.
    if !smoke && (!apply_pass || !all_match) {
        exit(1);
    }
}

fn parse_u64(s: &str, flag: &str) -> u64 {
    if let Some(hex) = s.strip_prefix("0x").or_else(|| s.strip_prefix("0X")) {
        u64::from_str_radix(hex, 16).unwrap_or_else(|_| usage(&format!("bad {flag} hex: {s}")))
    } else {
        s.parse::<u64>()
            .unwrap_or_else(|_| usage(&format!("bad {flag}: {s}")))
    }
}
