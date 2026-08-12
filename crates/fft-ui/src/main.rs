//! `fft` binary: GPUI window + frame-time harness, optionally driven by an fft-engine
//! replay or sim-live source into linked WindoTrader profile + Daytradr DOM custom elements.
//!
//! ```text
//! fft [--gate <seconds>] [--trace <path>] [--replay <fftlog>] [--replay-at <ts>]
//!     [--sim-live <fftlog>] [--head <ts>] [--live-out <path>]
//!     [--prior <fftlog>]... [--no-prior-discovery] [--no-auto-ingest] [--dbn-dir <path>]
//!     [--gate-out <path>] [--manifest <path>] [--conditions <text>] [--startup-trace]
//!     [--scrub-latency-gate <N>] [--scrub-latency-out <path>] [--scrub-latency-seed <u64>]
//! ```
//!
//! `--startup-trace` emits wall-ms from process entry to first painted frame and to the
//! first non-empty `RenderSnapshot` (generation > 0), then quits. Used for the M5 cold-
//! start boring gate (PRD §4); normal runs are unchanged.
//!
//! `--scrub-latency-gate <N>` scripts N scrub-releases after the first interactive snapshot
//! and measures release→rendered p95 (PRD §4 claim 1 letter). Requires `--replay` and
//! `--scrub-latency-out`; exclusive with `--startup-trace` and `--gate`.
//!
//! Without a feed source, the M0 blank/dark window + frame harness is unchanged. With
//! `--replay`, the dedicated engine thread is spawned, `SetSource(Replay)` + `Play` are
//! sent, and each animation frame loads exactly one `Arc<RenderSnapshot>` from the
//! latest-value slot (zero `entity.update` calls on the snapshot path); after the run the
//! engine's final coverage counters are printed and, with `--gate`, a nonzero dropped
//! count fails the process (`docs/ENGINE.md` §3). An engine-thread panic is recorded in the
//! evidence and fails the process — it never pre-empts the write.
//!
//! `--replay-at <ts>` seeks to an event timestamp before Play (PRD §6 sim-live anchor).
//! Accepted forms: all-digits nanoseconds UTC, or `YYYY-MM-DDTHH:MM:SSZ` (UTC, second
//! resolution). Requires `--replay`.
//!
//! `--sim-live <fftlog>` joins at session open and wall-pins at `--head` (ENGINE.md §5).
//! Requires `--head` and `--live-out`; mutually exclusive with `--replay` / `--replay-at`.
//! Prior discovery stays replay-only. The wall-clock head snaps to the last in-log event
//! ts ≤ head before `SetSource` (exact event timestamp required by the engine).
//!
//! `--prior <fftlog>` (repeatable) loads earlier trade-date logs as profile-only prior
//! sessions after Play. Order on the CLI is preserved and is the UI contract: **oldest
//! first**. Existing matching priors are also discovered beside the replay log and under
//! the session cache unless `--no-prior-discovery` is supplied. Wrong explicit dates are
//! skipped loudly by the engine (`docs/ENGINE.md` §2). Requires `--replay`. Each explicit
//! path is existence-validated at startup (same rationale as `--manifest`).
//!
//! `--gate-out` writes the run's self-identifying JSON evidence (git SHA + dirty, pinned
//! `gpui` rev, replay/sim-live source path, frame-time distribution, coverage) — on `FAIL`
//! as well as `PASS`. `--manifest` / `--conditions` are runner-supplied provenance; absent
//! ⇒ JSON `null`. `--manifest` is validated before the window opens.
//!
//! Redraw uses GPUI's `request_animation_frame` pattern. Keep the gate window
//! keyboard-focused: GPUI caps unfocused animation-driven redraw to ~30 fps.

use std::cell::RefCell;
use std::process::ExitCode;
use std::rc::Rc;

use fft_engine::EngineHandle;
use fft_ui::gate_report::{CoverageReport, GateOut, GateReport, GitInfo, RunMeta};
use fft_ui::harness::Harness;
use fft_ui::prefs::ShellPrefsHandles;
use fft_ui::prior_discovery::PriorOptions;
use fft_ui::shell::Shell;
use gpui::{App, AppContext, Bounds, WindowBounds, WindowOptions, px, size};

mod fft_cli;
use fft_cli::{gate_description, parse_args};

fn main() -> ExitCode {
    // M5 cold-start origin: wall clock before any arg parse / GPUI / engine work.
    fft_ui::startup_trace::mark_process_start();
    // A market display must never render at GPUI's unfocused ~30fps throttle rate —
    // the tape runs whether or not this window holds keyboard focus (opt-out patched
    // into the pinned gpui rev — see the workspace Cargo.toml).
    // SAFETY: before any thread exists; GPUI reads the variable once, later.
    unsafe { std::env::set_var("GPUI_DISABLE_INACTIVE_THROTTLE", "1") };
    let args = parse_args();
    if args.startup_trace {
        fft_ui::startup_trace::enable();
    }
    if let (Some(n), Some(out)) = (args.scrub_latency_gate, args.scrub_latency_out.clone()) {
        let log = args
            .startup
            .meta_path()
            .expect("scrub-latency-gate requires --replay");
        fft_ui::scrub_latency::enable(
            n,
            out,
            args.scrub_latency_seed,
            fft_ui::scrub_latency::BUDGET_P95_MS,
            log,
        );
    }
    // Missing tzdb fails loudly here, before the window opens; paint-time clock
    // failures after this soft-fail to "--:--:--" (doctrine §7 split).
    fft_ui::transport::ensure_tzdb_available();
    // Provenance and evidence-file writability are established before the window opens: a
    // 60 s measured run must never be spent to discover the result cannot be recorded.
    let meta = RunMeta {
        gate: gate_description(&args),
        binary: fft_ui::gate_report::command_line(std::env::args()),
        git: GitInfo::capture(),
        // Sim-live records the source log path in the existing `replay` field.
        replay: args.startup.meta_path(),
        trace: args.trace.clone(),
        manifest: args
            .manifest
            .as_ref()
            .map(|path| path.display().to_string()),
        conditions: args.conditions.clone(),
    };
    let gate_out = args.gate_out.clone().map(GateOut::create);

    let harness = Rc::new(RefCell::new(Harness::new(args.gate, args.trace)));
    let app_harness = harness.clone();
    let engine_slot: Rc<RefCell<Option<EngineHandle>>> = Rc::new(RefCell::new(None));
    let engine_for_app = engine_slot.clone();
    // Quit-hook: Shell installs handles at construction; main writes prefs after app.run.
    let prefs_slot: Rc<RefCell<Option<ShellPrefsHandles>>> = Rc::new(RefCell::new(None));
    let prefs_for_app = prefs_slot.clone();
    let engine_expected = args.startup.starts_engine();
    let startup = args.startup;
    // Claim-1 gate measures release→rendered on the current session only; priors add
    // non-seek work on the engine thread and must not contaminate the p95 letter.
    let scrub_gate = args.scrub_latency_gate.is_some();
    let prior = if scrub_gate { Vec::new() } else { args.prior };
    let prior_options = PriorOptions {
        discover: !scrub_gate && !args.no_prior_discovery,
        auto_ingest: !scrub_gate && !args.no_prior_discovery && !args.no_auto_ingest,
        dbn_dir: if scrub_gate { None } else { args.dbn_dir },
    };

    gpui_platform::application().run(move |cx: &mut App| {
        cx.on_window_closed(|cx, _| cx.quit()).detach();
        let bounds = Bounds::centered(None, size(px(1024.), px(768.)), cx);
        cx.open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                ..Default::default()
            },
            move |_, cx| {
                cx.new(|cx| {
                    let shell = Shell::new(
                        app_harness.clone(),
                        startup,
                        prior,
                        prior_options,
                        engine_for_app,
                        cx,
                    );
                    *prefs_for_app.borrow_mut() = Some(shell.prefs_handles());
                    shell
                })
            },
        )
        .expect("fft: failed to open window");
        cx.activate(true);
    });

    // Persist UI prefs after the window is gone (never crash on write failure).
    if let Some(handles) = prefs_slot.borrow().as_ref() {
        handles.snapshot().save();
    }

    // Keep the slot alive across shutdown: the engine may publish once more while draining.
    let snapshots = engine_slot.borrow().as_ref().map(EngineHandle::snapshots);
    // Held, not unwrapped: an engine panic must not cost a 60 s measured run its evidence
    // file, so the result is decided only after the report is on disk.
    let engine_exit = engine_slot.borrow_mut().take().map(EngineHandle::shutdown);

    let counters = engine_exit
        .as_ref()
        .and_then(|exit| exit.as_ref().ok())
        .map(|exit| exit.coverage)
        // The engine died: the last publication is all that survives of what it applied.
        .or_else(|| snapshots.map(|slot| slot.load().coverage));
    let coverage = counters.map(|counters| {
        CoverageReport::new(
            counters.events_read,
            counters.events_applied,
            counters.gap_records,
        )
    });
    match &coverage {
        Some(coverage) => println!("fft: {coverage}"),
        None if engine_expected => {
            eprintln!("fft: WARNING feed requested but the engine never started — no coverage");
        }
        None => {}
    }

    let result = harness.borrow_mut().finish();
    let mut report = GateReport::new(
        &meta,
        fft_ui::gate_report::now_rfc3339_utc(),
        result,
        coverage,
    );
    let engine_panic = engine_exit.and_then(Result::err);
    if let Some(err) = &engine_panic {
        report.record_engine_panic(&panic_message(&**err));
    }
    if let Some(out) = gate_out {
        out.write(&report);
    }
    if let Some(err) = engine_panic {
        eprintln!("fft: ENGINE THREAD PANICKED: {err:?}");
        return ExitCode::FAILURE;
    }
    if fft_ui::scrub_latency::enabled() {
        if !fft_ui::scrub_latency::should_quit() {
            fft_ui::scrub_latency::fail_and_quit(
                "exited before scrub-latency gate completed (window closed or engine died)",
            );
        }
        return if fft_ui::scrub_latency::exit_failure() == Some(true) {
            ExitCode::FAILURE
        } else {
            ExitCode::SUCCESS
        };
    }
    if fft_ui::gate_report::gate_failed(harness.borrow().gating(), report.verdict) {
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
    }
}

/// Panic payload as text. `panic!` produces `&str` or `String`; anything else is opaque.
fn panic_message(payload: &(dyn std::any::Any + Send)) -> String {
    payload
        .downcast_ref::<&str>()
        .map(|msg| (*msg).to_string())
        .or_else(|| payload.downcast_ref::<String>().cloned())
        .unwrap_or_else(|| "non-string panic payload".to_string())
}
