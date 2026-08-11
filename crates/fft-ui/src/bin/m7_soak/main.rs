//! Headless M7 long-run stability soak: real engine product paths, arbitrary wall time.
//!
//! One CYCLE = spawn EngineService → SetSource(checkpointed current) → LoadPriorSession
//! oldest-first → Play at --speed with continuous snapshot polling → scrub burst
//! (m5-scrub-burst pattern) → SetSpeed ladder → Pause/Play toggles → run to EOF or
//! --cycle-secs → SetSource again (same log; priors retained per ENGINE.md §2 r4) →
//! observe retention via Seek publish → shutdown. Engine panic mid-cycle is a recorded
//! finding; the rig continues with a fresh engine. Metrics: JSON lines to --out
//! (append+flush, crash-safe). One summary line with explicit PASS/FAIL on normal exit.
//!
//! Bin lives in fft-ui (with m5-scrub-burst / m5-rss-week): headless gate harnesses that
//! drive EngineService without GPUI. fft-engine stays the product library + fft-checkpoint.
//!
//! Canonical full-week shape (ENGINE.md §2 date rule): **Fri current + Mon–Thu priors**.
//! Later-or-equal trade dates are counted skips, never accepted priors.
//!
//! Quiet-box 24 h (Fri current + Mon–Thu):
//! ```text
//! cargo run --release -p fft-ui --bin m7-soak -- \
//!   --replay /tmp/esu6-fri-ckpt.fftlog \
//!   --prior /tmp/esu6-2026-07-27.fftlog --prior /tmp/esu6-2026-07-28.fftlog \
//!   --prior /tmp/esu6-2026-07-29.fftlog --prior /tmp/esu6-2026-07-30.fftlog \
//!   --speed 64 --out perf-runner/results/<date>-m7-soak.jsonl --max-hours 24
//! ```
//!
//! Week-long wall (unbounded cycles, 168 h): same Fri+Mon–Thu shape; not a single
//! week-long engine — each cycle restarts. Single-engine week-long is a remaining non-goal.
//! ```text
//! cargo run --release -p fft-ui --bin m7-soak -- \
//!   --replay /tmp/esu6-fri-ckpt.fftlog \
//!   --prior /tmp/esu6-2026-07-27.fftlog --prior /tmp/esu6-2026-07-28.fftlog \
//!   --prior /tmp/esu6-2026-07-29.fftlog --prior /tmp/esu6-2026-07-30.fftlog \
//!   --speed 64 --out perf-runner/results/<date>-m7-soak-week.jsonl --max-hours 168
//! ```
//!
//! Smoke (short cycles; use only earlier-date priors so honesty can pass):
//! ```text
//! cargo run --release -p fft-ui --bin m7-soak -- \
//!   --replay /tmp/esu6-wed-v3-ckpt.fftlog \
//!   --prior /tmp/esu6-2026-07-27.fftlog --prior /tmp/esu6-2026-07-28.fftlog \
//!   --speed 64 --cycle-secs 120 --max-cycles 3 --out /tmp/m7-soak-smoke.jsonl
//! ```
//!
//! EOF-driven single cycle (`--cycle-secs 0`): ends at source EOF; heartbeats every 30 s.
//! ```text
//! cargo run --release -p fft-ui --bin m7-soak -- \
//!   --replay /tmp/esu6-wed-v3-ckpt.fftlog \
//!   --prior ~/.cache/fft/sessions/ESU6-2026-07-27.fftlog \
//!   --prior ~/.cache/fft/sessions/ESU6-2026-07-28.fftlog \
//!   --speed 64 --cycle-secs 0 --max-cycles 1 --out /tmp/m7-soak-eof-smoke.jsonl
//! ```
//!
//! Signal note: no new deps; std has no portable SIGINT handler. Ctrl-C/SIGTERM may
//! skip the summary line — per-cycle JSONL is append+flush so completed cycles survive.
//! Mid-cycle `kind=heartbeat` lines are also append+flush; after-soak-gates.sh only
//! requires exactly one terminal `kind=summary`.

mod args;
mod cycle;
mod heartbeat;
mod honesty;
mod metrics;
mod util;

use std::process::exit;
use std::time::Instant;

use fft_ui::gate_report::GitInfo;

use args::parse_args;
use cycle::run_cycle;
use metrics::{SummaryLine, append_json_line, leak_suspect};
use util::{RSS_BUDGET_BYTES, eof_cycle_deadline_secs, event_time_bounds};

fn main() {
    let args = parse_args();
    // Truncate / create so this run's JSONL is clean.
    std::fs::File::create(&args.out).unwrap_or_else(|e| {
        panic!("m7-soak: cannot create {}: {e}", args.out.display());
    });

    let git = GitInfo::capture();
    let (first_ts, last_ts) = event_time_bounds(&args.replay);
    eprintln!(
        "m7-soak: replay={} priors={} speed={} cycle_secs={} max_cycles={} max_hours={} \
         scrub_seeks={} leak_window={} leak_pct={}% first_ts={first_ts} last_ts={last_ts}",
        args.replay.display(),
        args.priors.len(),
        args.speed,
        args.cycle_secs,
        args.max_cycles,
        args.max_hours,
        args.scrub_seeks,
        args.leak_window,
        args.leak_pct,
    );
    if args.cycle_secs == 0 {
        let eof_secs = eof_cycle_deadline_secs(first_ts, last_ts, args.speed);
        eprintln!(
            "m7-soak: cycle_secs=0 ⇒ EOF-driven; safety deadline={eof_secs}s ((session_span_ns/speed)×2 + 120); heartbeats every 30s"
        );
    }
    eprintln!(
        "m7-soak: full-week shape = Fri current + Mon–Thu priors (ENGINE.md §2); \
         later-date --prior entries are expected skips only"
    );
    eprintln!(
        "m7-soak: signal limitation — no SIGINT/SIGTERM handler (no new deps); \
         rely on per-cycle JSONL append+flush. Kill may skip the summary line."
    );

    let soak_start = Instant::now();
    let wall_deadline = if args.max_hours > 0.0 {
        Some(soak_start + std::time::Duration::from_secs_f64(args.max_hours * 3600.0))
    } else {
        None
    };

    let mut cycle = 0u64;
    let mut failures = 0u64;
    let mut leak_suspects = 0u64;
    let mut rss_ceiling_fails = 0u64;
    // In-cycle peak RSS series (not post-shutdown residual) for leak detection.
    let mut peak_rss_series: Vec<u64> = Vec::new();
    let mut baseline_rss: Option<u64> = None;
    let mut peak_vm_rss_kb = 0u64;
    let mut peak_vm_hwm_kb = 0u64;
    loop {
        if args.max_cycles > 0 && cycle >= args.max_cycles {
            break;
        }
        if wall_deadline.is_some_and(|d| Instant::now() >= d) {
            break;
        }

        cycle += 1;
        eprintln!("m7-soak: === cycle {cycle} start ===");
        let result = run_cycle(cycle, &args, first_ts, last_ts);
        peak_vm_rss_kb = peak_vm_rss_kb.max(result.peak_rss_kb);
        peak_vm_hwm_kb = peak_vm_hwm_kb.max(result.peak_hwm_kb);

        if !result.line.ok {
            failures += 1;
        }
        if result.peak_rss_kb * 1024 > RSS_BUDGET_BYTES || !result.line.rss_ceiling_ok {
            rss_ceiling_fails += 1;
            eprintln!(
                "m7-soak: FAIL RSS CEILING cycle={cycle} peak_rss={} kB ({:.2} MiB)",
                result.peak_rss_kb,
                result.peak_rss_kb as f64 / 1024.0
            );
        }

        peak_rss_series.push(result.peak_rss_kb);
        if cycle == 3 {
            baseline_rss = Some(result.peak_rss_kb);
            eprintln!(
                "m7-soak: leak baseline (cycle 3 in-cycle peak) VmRSS={} kB",
                result.peak_rss_kb
            );
        }
        if let Some(base) = baseline_rss
            && leak_suspect(&peak_rss_series, base, args.leak_window, args.leak_pct)
        {
            leak_suspects += 1;
            let window = &peak_rss_series[peak_rss_series.len() - args.leak_window..];
            eprintln!(
                "m7-soak: LEAK SUSPECT cycle={cycle} baseline_kb={base} \
                 peak_window_kb={window:?} leak_pct={}",
                args.leak_pct
            );
        }

        append_json_line(&args.out, &result.line);
        eprintln!(
            "m7-soak: cycle={cycle} ok={} wall={:.1}s applied={} pubs={} seeks_exec={} \
             prior_ok={}/{} prior_skip={}/{} sessions={} peak_rss={} kB hwm={} kB \
             ready={} scrub={} retain={} panic={:?}",
            result.line.ok,
            result.line.wall_secs,
            result.line.events_applied,
            result.line.publications,
            result.line.seeks_executed,
            result.line.priors_completed,
            result.line.expected_priors_accepted,
            result.line.prior_skips,
            result.line.expected_prior_skips,
            result.line.sessions,
            result.line.peak_rss_kb,
            result.line.vm_hwm_kb,
            result.line.current_ready,
            result.line.seeks_final_answered,
            result.line.retention_ok,
            result.line.panic,
        );
    }

    let verdict = if failures == 0 && leak_suspects == 0 && rss_ceiling_fails == 0 {
        "PASS"
    } else {
        "FAIL"
    };
    let summary = SummaryLine {
        kind: "summary",
        verdict,
        cycles: cycle,
        failures,
        leak_suspects,
        rss_ceiling_fails,
        wall_secs: soak_start.elapsed().as_secs_f64(),
        peak_vm_rss_kb,
        peak_vm_hwm_kb,
        baseline_rss_kb: baseline_rss,
        leak_window: args.leak_window,
        leak_pct: args.leak_pct,
        git_sha: git.sha,
        git_dirty: git.dirty,
        label: args.label,
        notes: Some(format!(
            "peak_rss_series_kb={peak_rss_series:?}; signal=none (JSONL append-safe); \
             leak_uses=in_cycle_peak"
        )),
    };
    append_json_line(&args.out, &summary);
    eprintln!(
        "m7-soak: SUMMARY verdict={verdict} cycles={cycle} failures={failures} \
         leak_suspects={leak_suspects} rss_ceiling_fails={rss_ceiling_fails} \
         wall={:.1}s peak_rss={} kB peak_hwm={} kB out={}",
        summary.wall_secs,
        peak_vm_rss_kb,
        peak_vm_hwm_kb,
        args.out.display()
    );

    if verdict != "PASS" {
        exit(1);
    }
}
