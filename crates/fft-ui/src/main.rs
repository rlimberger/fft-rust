//! `fft` binary: GPUI window + frame-time harness, optionally driven by an fft-engine
//! replay into a Daytradr DOM ladder (`DomLadder` custom Element).
//!
//! ```text
//! fft [--gate <seconds>] [--trace <path>] [--replay <fftlog>]
//! ```
//!
//! Without `--replay`, the M0 blank/dark window + frame harness is unchanged. With
//! `--replay`, the dedicated engine thread is spawned, `SetSource(Replay)` + `Play` are
//! sent, and each animation frame loads exactly one `Arc<RenderSnapshot>` from the
//! latest-value slot (zero `entity.update` calls on the snapshot path).
//!
//! Redraw uses GPUI's `request_animation_frame` pattern. Keep the gate window
//! keyboard-focused: GPUI caps unfocused animation-driven redraw to ~30 fps.

use std::cell::RefCell;
use std::fs::File;
use std::io::{BufWriter, Write};
use std::path::PathBuf;
use std::process::ExitCode;
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use fft_engine::{
    EngineCmd, EngineConfig, EngineHandle, EngineService, RenderSnapshot, SnapshotSlot, Source,
};
use fft_ui::dom_ladder::DomLadder;
use fft_ui::frame_stats::FrameStats;
use gpui::{
    App, Bounds, Context, Window, WindowBounds, WindowOptions, div, prelude::*, px, rgb, size,
};

/// Warmup is a time budget, never an event count (doctrine rule 5); the sample floor only
/// guards the median against a compositor that briefly withholds frame callbacks.
const WARMUP: Duration = Duration::from_millis(500);
const MIN_WARMUP_SAMPLES: usize = 8;

struct Shell {
    harness: Rc<RefCell<Harness>>,
    snapshots: Option<SnapshotSlot>,
    /// Coalesced payloadless wake from the engine thread (RAF already redraws; this is the
    /// dirty bit the doctrine describes — sampled, never a per-publication update).
    wake_dirty: Arc<AtomicBool>,
    /// Exactly one coherent snapshot for this frame (loaded at frame start).
    frame_snapshot: Arc<RenderSnapshot>,
}

impl Render for Shell {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
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

        if self.snapshots.is_some() {
            DomLadder::new(Arc::clone(&self.frame_snapshot)).into_any_element()
        } else {
            div().size_full().bg(rgb(0x101010)).into_any_element()
        }
    }
}

enum Phase {
    Warmup {
        started: Instant,
        gaps: Vec<Duration>,
    },
    Measuring {
        stats: FrameStats,
        gate_ends: Option<Instant>,
    },
}

struct Harness {
    gate: Option<Duration>,
    trace: Option<BufWriter<File>>,
    last_frame: Option<Instant>,
    phase: Option<Phase>,
}

impl Harness {
    fn new(gate: Option<Duration>, trace: Option<BufWriter<File>>) -> Self {
        Self {
            gate,
            trace,
            last_frame: None,
            phase: None,
        }
    }

    /// Returns `false` once the gate window has elapsed and the redraw loop should stop.
    fn on_frame(&mut self, now: Instant) -> bool {
        let gap = self.last_frame.replace(now).map(|last| now - last);
        let phase = self.phase.get_or_insert(Phase::Warmup {
            started: now,
            gaps: Vec::new(),
        });
        let Some(gap) = gap else { return true };
        match phase {
            Phase::Warmup { started, gaps } => {
                gaps.push(gap);
                if now - *started >= WARMUP && gaps.len() >= MIN_WARMUP_SAMPLES {
                    gaps.sort_unstable();
                    let refresh_interval = gaps[gaps.len() / 2];
                    let deadline = refresh_interval * 3 / 2;
                    eprintln!(
                        "fft: refresh interval {:.3} ms ({:.1} Hz), deadline {:.3} ms",
                        refresh_interval.as_secs_f64() * 1e3,
                        1.0 / refresh_interval.as_secs_f64(),
                        deadline.as_secs_f64() * 1e3,
                    );
                    self.phase = Some(Phase::Measuring {
                        stats: FrameStats::new(deadline),
                        gate_ends: self.gate.map(|g| now + g),
                    });
                }
                true
            }
            Phase::Measuring { stats, gate_ends } => {
                stats.record(gap);
                if let Some(trace) = &mut self.trace {
                    writeln!(trace, "{}", gap.as_nanos())
                        .unwrap_or_else(|err| panic!("fft: trace write failed: {err}"));
                }
                gate_ends.is_none_or(|ends| now < ends)
            }
        }
    }

    /// Prints the summary; returns the process exit code (nonzero iff gating and any miss).
    fn finish(&mut self) -> ExitCode {
        if let Some(trace) = &mut self.trace {
            trace
                .flush()
                .unwrap_or_else(|err| panic!("fft: trace flush failed: {err}"));
        }
        match &self.phase {
            Some(Phase::Measuring { stats, .. }) if stats.frames() > 0 => {
                let summary = stats.summary();
                println!("{summary}");
                if self.gate.is_some() && summary.missed > 0 {
                    ExitCode::FAILURE
                } else {
                    ExitCode::SUCCESS
                }
            }
            _ => {
                eprintln!("fft: no frames measured (warmup never completed)");
                if self.gate.is_some() {
                    ExitCode::FAILURE
                } else {
                    ExitCode::SUCCESS
                }
            }
        }
    }
}

struct Args {
    gate: Option<Duration>,
    trace: Option<BufWriter<File>>,
    replay: Option<PathBuf>,
}

fn parse_args() -> Args {
    let mut args = std::env::args().skip(1);
    let mut gate = None;
    let mut trace = None;
    let mut replay = None;
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
                let file = File::create(&path).unwrap_or_else(|err| {
                    eprintln!("fft: cannot open trace file {path}: {err}");
                    std::process::exit(2);
                });
                trace = Some(BufWriter::new(file));
            }
            "--replay" => {
                let path = args
                    .next()
                    .unwrap_or_else(|| usage("--replay requires <fftlog>"));
                replay = Some(validate_replay_path(PathBuf::from(path)));
            }
            other => usage(&format!("unknown argument: {other}")),
        }
    }
    Args {
        gate,
        trace,
        replay,
    }
}

fn validate_replay_path(path: PathBuf) -> PathBuf {
    let meta = std::fs::metadata(&path).unwrap_or_else(|err| {
        eprintln!(
            "fft: replay path missing or unreadable: {}: {err}",
            path.display()
        );
        std::process::exit(2);
    });
    if !meta.is_file() {
        eprintln!("fft: replay path is not a file: {}", path.display());
        std::process::exit(2);
    }
    // Prove readability before the engine thread opens it.
    File::open(&path).unwrap_or_else(|err| {
        eprintln!("fft: cannot open replay file {}: {err}", path.display());
        std::process::exit(2);
    });
    path
}

fn usage(msg: &str) -> ! {
    eprintln!("fft: {msg}\nusage: fft [--gate <seconds>] [--trace <path>] [--replay <fftlog>]");
    std::process::exit(2);
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
    let harness = Rc::new(RefCell::new(Harness::new(args.gate, args.trace)));
    let app_harness = harness.clone();
    let engine_slot: Rc<RefCell<Option<EngineHandle>>> = Rc::new(RefCell::new(None));
    let engine_for_app = engine_slot.clone();
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
                // Window first, then engine — shell paints before replay I/O on the
                // dedicated thread (M3 "instant shell").
                let (snapshots, wake_dirty) = if let Some(path) = replay {
                    let (handle, snapshots, wake_dirty) = spawn_replay_engine(path);
                    *engine_for_app.borrow_mut() = Some(handle);
                    (Some(snapshots), wake_dirty)
                } else {
                    (None, Arc::new(AtomicBool::new(false)))
                };
                cx.new(|_| Shell {
                    harness: app_harness.clone(),
                    snapshots,
                    wake_dirty,
                    frame_snapshot: Arc::new(RenderSnapshot::default()),
                })
            },
        )
        .expect("fft: failed to open window");
        cx.activate(true);
    });

    if let Some(handle) = engine_slot.borrow_mut().take() {
        handle
            .shutdown()
            .unwrap_or_else(|err| panic!("fft: engine thread panicked: {err:?}"));
    }
    harness.borrow_mut().finish()
}
