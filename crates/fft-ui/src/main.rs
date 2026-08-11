//! `fft` binary: GPUI window + frame-time harness, optionally driven by an fft-engine
//! replay into linked WindoTrader profile + Daytradr DOM custom elements.
//!
//! ```text
//! fft [--gate <seconds>] [--trace <path>] [--replay <fftlog>] [--replay-at <ts>]
//!     [--prior <fftlog>]... [--no-prior-discovery] [--no-auto-ingest] [--dbn-dir <path>]
//!     [--gate-out <path>] [--manifest <path>] [--conditions <text>] [--startup-trace]
//! ```
//!
//! `--startup-trace` emits wall-ms from process entry to first painted frame and to the
//! first non-empty `RenderSnapshot` (generation > 0), then quits. Used for the M5 cold-
//! start boring gate (PRD §4); normal runs are unchanged.
//!
//! Without `--replay`, the M0 blank/dark window + frame harness is unchanged. With
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
//! `--prior <fftlog>` (repeatable) loads earlier trade-date logs as profile-only prior
//! sessions after Play. Order on the CLI is preserved and is the UI contract: **oldest
//! first**. Existing matching priors are also discovered beside the replay log and under
//! the session cache unless `--no-prior-discovery` is supplied. Wrong explicit dates are
//! skipped loudly by the engine (`docs/ENGINE.md` §2). Requires `--replay`. Each explicit
//! path is existence-validated at startup (same rationale as `--manifest`).
//!
//! `--gate-out` writes the run's self-identifying JSON evidence (git SHA + dirty, pinned
//! `gpui` rev, replay path, frame-time distribution, coverage) — on `FAIL` as well as
//! `PASS`. `--manifest` / `--conditions` are runner-supplied provenance; absent ⇒ JSON
//! `null`. `--manifest` is validated before the window opens.
//!
//! Redraw uses GPUI's `request_animation_frame` pattern. Keep the gate window
//! keyboard-focused: GPUI caps unfocused animation-driven redraw to ~30 fps.

use std::cell::RefCell;
use std::path::PathBuf;
use std::process::ExitCode;
use std::rc::Rc;
use std::time::Duration;

use fft_engine::EngineHandle;
use fft_ui::datetime::parse_replay_at;
use fft_ui::gate_report::{CoverageReport, GateOut, GateReport, GitInfo, RunMeta};
use fft_ui::harness::Harness;
use fft_ui::prefs::ShellPrefsHandles;
use fft_ui::prior_discovery::PriorOptions;
use fft_ui::shell::Shell;
use gpui::{App, AppContext, Bounds, WindowBounds, WindowOptions, px, size};

struct Args {
    gate: Option<Duration>,
    trace: Option<PathBuf>,
    replay: Option<PathBuf>,
    /// Seek target in nanoseconds UTC; requires `--replay`.
    replay_at: Option<u64>,
    /// Original `--replay-at` argument text for gate provenance.
    replay_at_arg: Option<String>,
    /// Prior-day fftlogs, oldest-first (CLI order preserved).
    prior: Vec<PathBuf>,
    /// Disable sibling/cache prior discovery and auto-ingest.
    no_prior_discovery: bool,
    /// Keep existing-log discovery but disable DBN auto-ingest.
    no_auto_ingest: bool,
    /// Override automatic `data/GLBX-*` DBN directory resolution.
    dbn_dir: Option<PathBuf>,
    gate_out: Option<PathBuf>,
    /// Perf-runner manifest path — validated at startup, recorded verbatim in evidence.
    manifest: Option<PathBuf>,
    /// Free-form run conditions from the runner — recorded verbatim when supplied.
    conditions: Option<String>,
    /// Emit cold-start first-paint / first-interactive marks (M5 boring gate).
    startup_trace: bool,
}

fn parse_args() -> Args {
    let mut args = std::env::args().skip(1);
    let mut gate = None;
    let mut trace = None;
    let mut replay = None;
    let mut replay_at = None;
    let mut replay_at_arg = None;
    let mut prior = Vec::new();
    let mut no_prior_discovery = false;
    let mut no_auto_ingest = false;
    let mut dbn_dir = None;
    let mut gate_out = None;
    let mut manifest = None;
    let mut conditions = None;
    let mut startup_trace = false;
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--gate" => {
                let secs = args
                    .next()
                    .unwrap_or_else(|| usage("--gate requires <seconds>"));
                let secs: f64 = secs
                    .parse()
                    .unwrap_or_else(|_| usage(&format!("invalid --gate value: {secs}")));
                if !(secs > 0.0 && secs.is_finite()) {
                    usage(&format!("--gate must be a positive number, got {secs}"));
                }
                gate = Some(Duration::from_secs_f64(secs));
            }
            "--trace" => {
                let path = args
                    .next()
                    .unwrap_or_else(|| usage("--trace requires <path>"));
                trace = Some(PathBuf::from(path));
            }
            "--replay" => {
                let path = args
                    .next()
                    .unwrap_or_else(|| usage("--replay requires <fftlog>"));
                replay = Some(PathBuf::from(path));
            }
            "--replay-at" => {
                let value = args
                    .next()
                    .unwrap_or_else(|| usage("--replay-at requires <ts>"));
                let ts = parse_replay_at(&value).unwrap_or_else(|err| usage(&err));
                replay_at = Some(ts);
                replay_at_arg = Some(value);
            }
            "--prior" => {
                let path = args
                    .next()
                    .unwrap_or_else(|| usage("--prior requires <fftlog>"));
                let path = PathBuf::from(path);
                // Existence-validated before the window opens (same rationale as --manifest).
                if !path.is_file() {
                    usage(&format!("--prior file does not exist: {}", path.display()));
                }
                prior.push(path);
            }
            "--no-prior-discovery" => {
                no_prior_discovery = true;
            }
            "--no-auto-ingest" => {
                no_auto_ingest = true;
            }
            "--dbn-dir" => {
                let path = args
                    .next()
                    .unwrap_or_else(|| usage("--dbn-dir requires <path>"));
                let path = PathBuf::from(path);
                if !path.is_dir() {
                    usage(&format!("--dbn-dir is not a directory: {}", path.display()));
                }
                dbn_dir = Some(path);
            }
            "--gate-out" => {
                let path = args
                    .next()
                    .unwrap_or_else(|| usage("--gate-out requires <path>"));
                gate_out = Some(PathBuf::from(path));
            }
            "--manifest" => {
                let path = args
                    .next()
                    .unwrap_or_else(|| usage("--manifest requires <path>"));
                let path = PathBuf::from(path);
                // Same rationale as GateOut::create: bad provenance must fail before a
                // measured run is spent, never after it.
                if !path.is_file() {
                    usage(&format!(
                        "--manifest file does not exist: {}",
                        path.display()
                    ));
                }
                manifest = Some(path);
            }
            "--conditions" => {
                let text = args
                    .next()
                    .unwrap_or_else(|| usage("--conditions requires <text>"));
                conditions = Some(text);
            }
            "--startup-trace" => {
                startup_trace = true;
            }
            other => usage(&format!("unknown argument: {other}")),
        }
    }
    if replay_at.is_some() && replay.is_none() {
        usage("--replay-at requires --replay");
    }
    if !prior.is_empty() && replay.is_none() {
        usage("--prior requires --replay");
    }
    if startup_trace && replay.is_none() {
        usage("--startup-trace requires --replay (interactive mark needs a snapshot)");
    }
    Args {
        gate,
        trace,
        replay,
        replay_at,
        replay_at_arg,
        prior,
        no_prior_discovery,
        no_auto_ingest,
        dbn_dir,
        gate_out,
        manifest,
        conditions,
        startup_trace,
    }
}

fn usage(msg: &str) -> ! {
    eprintln!(
        "fft: {msg}\nusage: fft [--gate <seconds>] [--trace <path>] [--replay <fftlog>] \
         [--replay-at <ts>] [--prior <fftlog>]... [--no-prior-discovery] \
         [--no-auto-ingest] [--dbn-dir <path>] [--gate-out <path>] [--manifest <path>] \
         [--conditions <text>] [--startup-trace]\n\
         --prior: earlier trade-date fftlog (repeatable, oldest first; requires --replay)\n\
         --no-prior-discovery: disable existing-log discovery and DBN auto-ingest\n\
         --no-auto-ingest: discover existing prior logs but do not ingest missing days\n\
         --dbn-dir: override automatic data/GLBX-* DBN source resolution\n\
         --startup-trace: emit first_paint_ms / first_interactive_ms then quit (M5 cold start)"
    );
    std::process::exit(2);
}

/// Self-identifying description of what this run measured.
fn gate_description(args: &Args) -> String {
    let window = match args.gate {
        Some(gate) => format!("fft frame gate — {:.3} s", gate.as_secs_f64()),
        None => "fft frame harness (ungated)".to_string(),
    };
    let base = match (&args.replay, &args.replay_at_arg) {
        (Some(path), Some(at)) => format!("{window}, replay {} @ {at}", path.display()),
        (Some(path), None) => format!("{window}, replay {}", path.display()),
        (None, _) => format!("{window}, blank window"),
    };
    let n = args.prior.len();
    if n > 0 {
        format!("{base} +{n} priors")
    } else {
        base
    }
}

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
    // Provenance and evidence-file writability are established before the window opens: a
    // 60 s measured run must never be spent to discover the result cannot be recorded.
    let meta = RunMeta {
        gate: gate_description(&args),
        binary: fft_ui::gate_report::command_line(std::env::args()),
        git: GitInfo::capture(),
        replay: args.replay.clone(),
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
    let replaying = args.replay.is_some();
    let replay = args.replay;
    let replay_at = args.replay_at;
    let prior = args.prior;
    let prior_options = PriorOptions {
        discover: !args.no_prior_discovery,
        auto_ingest: !args.no_prior_discovery && !args.no_auto_ingest,
        dbn_dir: args.dbn_dir,
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
                        replay,
                        replay_at,
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
        None if replaying => {
            eprintln!("fft: WARNING replay requested but the engine never started — no coverage");
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
