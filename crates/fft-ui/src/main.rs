//! `fft` binary: GPUI window + frame-time harness, optionally driven by an fft-engine
//! replay into a Daytradr DOM ladder (`DomLadder` custom Element).
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
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use fft_engine::{
    EngineCmd, EngineConfig, EngineHandle, EngineService, RenderSnapshot, SnapshotSlot, Source,
};
use fft_ui::dom_input::DomInput;
use fft_ui::dom_ladder::DomLadder;
use fft_ui::dom_view::DomView;
use fft_ui::gate_report::{CoverageReport, GateOut, GateReport, GitInfo, RunMeta};
use fft_ui::glyph_cache::GlyphCache;
use fft_ui::harness::Harness;
use fft_ui::layout::{HEADER_H, ROW_H};
use gpui::{
    App, Bounds, Context, FocusHandle, MouseButton, ScrollDelta, Window, WindowBounds,
    WindowOptions, div, prelude::*, px, rgb, size,
};

struct ReplayResources {
    snapshots: SnapshotSlot,
    wake_dirty: Arc<AtomicBool>,
}

struct Shell {
    harness: Rc<RefCell<Harness>>,
    snapshots: Option<SnapshotSlot>,
    replay_ready: Rc<RefCell<Option<ReplayResources>>>,
    pending_replay: Option<PathBuf>,
    engine_slot: Rc<RefCell<Option<EngineHandle>>>,
    /// Coalesced payloadless wake from the engine thread (RAF already redraws; this is the
    /// dirty bit the doctrine describes — sampled, never a per-publication update).
    wake_dirty: Arc<AtomicBool>,
    /// Exactly one coherent snapshot for this frame (loaded at frame start).
    frame_snapshot: Arc<RenderSnapshot>,
    dom_view: Rc<RefCell<DomView>>,
    dom_input: Rc<RefCell<DomInput>>,
    glyph_cache: Rc<RefCell<GlyphCache>>,
    dom_focus: FocusHandle,
    focus_dom_once: bool,
}

impl Render for Shell {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if self.snapshots.is_none()
            && let Some(ready) = self.replay_ready.borrow_mut().take()
        {
            self.snapshots = Some(ready.snapshots);
            self.wake_dirty = ready.wake_dirty;
        }

        // Sample latest-value slot once per frame. Mutating `self` here is not
        // `entity.update` — notify/RAF drives re-render without an effect flush.
        if let Some(slot) = &self.snapshots {
            self.frame_snapshot = slot.load();
            self.wake_dirty.store(false, Ordering::Release);
        }

        if self.harness.borrow_mut().on_frame(Instant::now()) {
            window.request_animation_frame();
        } else {
            cx.defer(|cx| cx.quit());
        }

        if let Some(path) = self.pending_replay.take() {
            let replay_ready = Rc::clone(&self.replay_ready);
            let engine_slot = Rc::clone(&self.engine_slot);
            // Registered during the first render, so SetSource I/O cannot race the
            // shell's first paint. GPUI runs this callback on the following frame.
            window.on_next_frame(move |window, _| {
                let (handle, snapshots, wake_dirty) = spawn_replay_engine(path);
                *engine_slot.borrow_mut() = Some(handle);
                *replay_ready.borrow_mut() = Some(ReplayResources {
                    snapshots,
                    wake_dirty,
                });
                window.refresh();
            });
        }

        if self.snapshots.is_none() {
            return div().size_full().bg(rgb(0x101010)).into_any_element();
        }

        if self.focus_dom_once {
            self.dom_focus.focus(window, cx);
            self.focus_dom_once = false;
        }

        self.glyph_cache.borrow_mut().begin_frame();
        let view_for_frame = *self.dom_view.borrow();
        let ladder = DomLadder::new(
            Arc::clone(&self.frame_snapshot),
            view_for_frame,
            Rc::clone(&self.glyph_cache),
        );

        let key_view = Rc::clone(&self.dom_view);
        let drag_start = Rc::clone(&self.dom_input);
        let drag_move = Rc::clone(&self.dom_input);
        let drag_view = Rc::clone(&self.dom_view);
        let drag_snapshot = Arc::clone(&self.frame_snapshot);
        let drag_end = Rc::clone(&self.dom_input);
        let drag_end_out = Rc::clone(&self.dom_input);
        let wheel_input = Rc::clone(&self.dom_input);
        let wheel_view = Rc::clone(&self.dom_view);
        let wheel_snapshot = Arc::clone(&self.frame_snapshot);

        div()
            .id("dom-ladder-input")
            .size_full()
            .track_focus(&self.dom_focus)
            .on_key_down(move |event, window, cx| {
                if event.keystroke.modifiers.modified() {
                    return;
                }
                let mut view = key_view.borrow_mut();
                let (handled, changed) = match event.keystroke.key.as_str() {
                    "1" => (true, view.set_tick_scale(1)),
                    "2" => (true, view.set_tick_scale(2)),
                    "4" => (true, view.set_tick_scale(4)),
                    "c" => (true, view.recenter()),
                    _ => (false, false),
                };
                drop(view);
                if changed {
                    window.refresh();
                }
                if handled {
                    cx.stop_propagation();
                }
            })
            .on_mouse_down(MouseButton::Left, move |event, _, _| {
                // The wrapper is the full-window root, so its body starts at HEADER_H.
                let mut input = drag_start.borrow_mut();
                input.end_drag();
                if f32::from(event.position.y) >= HEADER_H {
                    input.begin_drag(f32::from(event.position.y));
                }
            })
            .on_mouse_move(move |event, window, _| {
                if !event.dragging() {
                    drag_move.borrow_mut().end_drag();
                    return;
                }
                let delta = drag_move
                    .borrow_mut()
                    .drag_to(f32::from(event.position.y), ROW_H);
                if delta == 0 {
                    return;
                }
                let changed = {
                    let mut view = drag_view.borrow_mut();
                    let dom = view.aggregate(&drag_snapshot.dom);
                    view.pan_rows(&dom, delta)
                };
                if changed {
                    window.refresh();
                }
            })
            .on_mouse_up(MouseButton::Left, move |_, _, _| {
                drag_end.borrow_mut().end_drag();
            })
            .on_mouse_up_out(MouseButton::Left, move |_, _, _| {
                drag_end_out.borrow_mut().end_drag();
            })
            .on_scroll_wheel(move |event, window, cx| {
                if event.modifiers.modified() {
                    return;
                }
                let rows = match event.delta {
                    ScrollDelta::Lines(delta) => delta.y,
                    ScrollDelta::Pixels(delta) => f32::from(delta.y) / ROW_H,
                };
                if rows == 0.0 {
                    return;
                }
                let delta = wheel_input.borrow_mut().wheel(rows);
                if delta != 0 {
                    let changed = {
                        let mut view = wheel_view.borrow_mut();
                        let dom = view.aggregate(&wheel_snapshot.dom);
                        view.pan_rows(&dom, delta)
                    };
                    if changed {
                        window.refresh();
                    }
                }
                cx.stop_propagation();
            })
            .child(ladder)
            .into_any_element()
    }
}

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

fn spawn_replay_engine(path: PathBuf) -> (EngineHandle, SnapshotSlot, Arc<AtomicBool>) {
    let wake_dirty = Arc::new(AtomicBool::new(false));
    let wake = Arc::clone(&wake_dirty);
    let handle = EngineService::spawn(
        EngineConfig {
            visible_tick_span: 256,
        },
        Box::new(move || {
            wake.store(true, Ordering::Release);
        }),
    )
    .unwrap_or_else(|err| panic!("fft: failed to spawn engine thread: {err}"));
    handle
        .send(EngineCmd::SetSource(Source::Replay { path }))
        .unwrap_or_else(|err| panic!("fft: SetSource failed: {err}"));
    handle
        .send(EngineCmd::Play)
        .unwrap_or_else(|err| panic!("fft: Play failed: {err}"));
    let snapshots = handle.snapshots();
    (handle, snapshots, wake_dirty)
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
            move |_, cx| {
                cx.new(|cx| Shell {
                    harness: app_harness.clone(),
                    snapshots: None,
                    replay_ready: Rc::new(RefCell::new(None)),
                    pending_replay: replay,
                    engine_slot: engine_for_app,
                    wake_dirty: Arc::new(AtomicBool::new(false)),
                    frame_snapshot: Arc::new(RenderSnapshot::default()),
                    dom_view: Rc::new(RefCell::new(DomView::default())),
                    dom_input: Rc::new(RefCell::new(DomInput::default())),
                    glyph_cache: Rc::new(RefCell::new(GlyphCache::default())),
                    dom_focus: cx.focus_handle().tab_stop(true),
                    focus_dom_once: true,
                })
            },
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
