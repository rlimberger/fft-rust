//! `fft` binary: GPUI window + frame-time harness, optionally driven by an fft-engine
//! replay into linked WindoTrader profile + Daytradr DOM custom elements.
//!
//! ```text
//! fft [--gate <seconds>] [--trace <path>] [--replay <fftlog>] [--gate-out <path>]
//! ```
//!
//! Without `--replay`, the M0 blank/dark window + frame harness is unchanged. With
//! `--replay`, the dedicated engine thread is spawned, `SetSource(Replay)` + `Play` are
//! sent, and each animation frame loads exactly one `Arc<RenderSnapshot>` from the
//! latest-value slot (zero `entity.update` calls on the snapshot path); after the run the
//! final snapshot's coverage counters are printed and, with `--gate`, a nonzero dropped
//! count fails the process (`docs/ENGINE.md` §3).
//!
//! `--gate-out` writes the run's self-identifying JSON evidence (git SHA + dirty, replay
//! path, frame-time distribution, coverage) — on `FAIL` as well as `PASS`.
//!
//! Redraw uses GPUI's `request_animation_frame` pattern. Keep the gate window
//! keyboard-focused: GPUI caps unfocused animation-driven redraw to ~30 fps.

use std::cell::RefCell;
use std::path::PathBuf;
use std::process::ExitCode;
use std::rc::Rc;
use std::time::Duration;

use fft_engine::EngineHandle;
use fft_ui::gate_report::{CoverageReport, GateOut, GateReport, GitInfo, RunMeta};
use fft_ui::harness::Harness;
use fft_ui::shell::Shell;
use gpui::{App, AppContext, Bounds, WindowBounds, WindowOptions, px, size};

struct Args {
    gate: Option<Duration>,
    trace: Option<PathBuf>,
    replay: Option<PathBuf>,
    gate_out: Option<PathBuf>,
}

fn parse_args() -> Args {
    let mut args = std::env::args().skip(1);
    let mut gate = None;
    let mut trace = None;
    let mut replay = None;
    let mut gate_out = None;
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
            "--gate-out" => {
                let path = args
                    .next()
                    .unwrap_or_else(|| usage("--gate-out requires <path>"));
                gate_out = Some(PathBuf::from(path));
            }
            other => usage(&format!("unknown argument: {other}")),
        }
    }
    Args {
        gate,
        trace,
        replay,
        gate_out,
    }
}

fn usage(msg: &str) -> ! {
    eprintln!(
        "fft: {msg}\nusage: fft [--gate <seconds>] [--trace <path>] [--replay <fftlog>] \
         [--gate-out <path>]"
    );
    std::process::exit(2);
}

/// Self-identifying description of what this run measured.
fn gate_description(args: &Args) -> String {
    let window = match args.gate {
        Some(gate) => format!("fft frame gate — {:.3} s", gate.as_secs_f64()),
        None => "fft frame harness (ungated)".to_string(),
    };
    match &args.replay {
        Some(path) => format!("{window}, replay {}", path.display()),
        None => format!("{window}, blank window"),
    }
}

fn main() -> ExitCode {
    let args = parse_args();
    // Provenance and evidence-file writability are established before the window opens: a
    // 60 s measured run must never be spent to discover the result cannot be recorded.
    let meta = RunMeta {
        gate: gate_description(&args),
        binary: fft_ui::gate_report::command_line(std::env::args()),
        git: GitInfo::capture(),
        replay: args.replay.clone(),
        trace: args.trace.clone(),
    };
    let gate_out = args.gate_out.clone().map(GateOut::create);

    let harness = Rc::new(RefCell::new(Harness::new(args.gate, args.trace)));
    let app_harness = harness.clone();
    let engine_slot: Rc<RefCell<Option<EngineHandle>>> = Rc::new(RefCell::new(None));
    let engine_for_app = engine_slot.clone();
    let replaying = args.replay.is_some();
    let replay = args.replay;

    gpui_platform::application().run(move |cx: &mut App| {
        cx.on_window_closed(|cx, _| cx.quit()).detach();
        let bounds = Bounds::centered(None, size(px(1024.), px(768.)), cx);
        cx.open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                ..Default::default()
            },
            move |_, cx| cx.new(|cx| Shell::new(app_harness.clone(), replay, engine_for_app, cx)),
        )
        .expect("fft: failed to open window");
        cx.activate(true);
    });

    // Keep the slot alive across shutdown: the engine may publish once more while draining.
    let snapshots = engine_slot.borrow().as_ref().map(EngineHandle::snapshots);
    if let Some(handle) = engine_slot.borrow_mut().take() {
        handle
            .shutdown()
            .unwrap_or_else(|err| panic!("fft: engine thread panicked: {err:?}"));
    }

    let coverage = snapshots.map(|slot| {
        let counters = slot.load().coverage;
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
    let report = GateReport::new(
        &meta,
        fft_ui::gate_report::now_rfc3339_utc(),
        result,
        coverage,
    );
    if let Some(out) = gate_out {
        out.write(&report);
    }
    if fft_ui::gate_report::gate_failed(harness.borrow().gating(), report.verdict) {
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
    }
}
