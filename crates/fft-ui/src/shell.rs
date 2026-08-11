//! Coherent two-pane shell and zero-`entity.update` frame/input path.

use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Instant;

use fft_engine::{
    EngineCmd, EngineConfig, EngineHandle, EngineService, RenderSnapshot, SnapshotSlot, Source,
};
use gpui::{Context, FocusHandle, MouseButton, Render, Window, div, prelude::*};

use crate::dom_input::DomInput;
use crate::dom_ladder::DomLadder;
use crate::dom_view::DomView;
use crate::glyph_cache::GlyphCache;
use crate::harness::Harness;
use crate::mp_element::MarketProfile;
#[cfg(debug_assertions)]
use crate::mp_view::check_pane_agreement;
use crate::mp_view::display_session;
use crate::os_theme::{ThemeSlot, resolve_font_family, spawn_theme_watcher};
use crate::pane_state::PaneState;
use crate::shell_panes;
use crate::theme::Palette;
use crate::theme_warmup::{
    PendingTheme, ThemeWarmAction, collect_visible_glyph_jobs, drive_theme_warmup,
    note_theme_slot_advance, shape_pending_batch,
};
use crate::transport::{TransportCommand, TransportState, session_range_ns};
use crate::transport_paint::{ScrubTrackGeom, TransportStripArgs, transport_strip};

struct ReplayResources {
    snapshots: SnapshotSlot,
    wake_dirty: Arc<AtomicBool>,
}

pub struct Shell {
    harness: Rc<RefCell<Harness>>,
    snapshots: Option<SnapshotSlot>,
    replay_ready: Rc<RefCell<Option<ReplayResources>>>,
    pending_replay: Option<PathBuf>,
    /// Optional seek target (ns UTC) applied after SetSource, before Play.
    replay_at: Option<u64>,
    engine_slot: Rc<RefCell<Option<EngineHandle>>>,
    wake_dirty: Arc<AtomicBool>,
    frame_snapshot: Arc<RenderSnapshot>,
    panes: Rc<RefCell<PaneState>>,
    dom_input: Rc<RefCell<DomInput>>,
    mp_input: Rc<RefCell<DomInput>>,
    glyph_cache: Rc<RefCell<GlyphCache>>,
    palette: Rc<Palette>,
    /// OS theme scale (`base_size / 12`); applied to row heights and font sizes.
    scale: f32,
    theme_slot: Arc<ThemeSlot>,
    /// Last ThemeSlot generation we have reacted to (detect ≠ adopt).
    slot_seen_generation: u64,
    /// Incoming theme waiting for glyph warm-up before adoption.
    pending_theme: Option<PendingTheme>,
    /// Resolved once before the window opens; live family switching is out of scope.
    font_family: String,
    focus: FocusHandle,
    focus_once: bool,
    /// Replay transport (`r` strip + keys). RefCell so key/mouse handlers avoid entity.update.
    transport: Rc<RefCell<TransportState>>,
    /// Scrub track window-space geometry (updated each strip paint).
    scrub_track: Rc<RefCell<ScrubTrackGeom>>,
}

impl Shell {
    pub fn new(
        harness: Rc<RefCell<Harness>>,
        pending_replay: Option<PathBuf>,
        replay_at: Option<u64>,
        engine_slot: Rc<RefCell<Option<EngineHandle>>>,
        cx: &mut Context<Self>,
    ) -> Self {
        // Family is fixed at startup (fc-match); palette/scale follow the OS theme live.
        let font_family = resolve_font_family();
        let theme_slot = spawn_theme_watcher();
        let snap = theme_slot.load();
        let slot_seen_generation = snap.generation;
        let palette = Rc::new(snap.palette);
        let scale = snap.scale;
        Self {
            harness,
            snapshots: None,
            replay_ready: Rc::new(RefCell::new(None)),
            pending_replay,
            replay_at,
            engine_slot,
            wake_dirty: Arc::new(AtomicBool::new(false)),
            frame_snapshot: Arc::new(RenderSnapshot::default()),
            panes: Rc::new(RefCell::new(PaneState::default())),
            dom_input: Rc::new(RefCell::new(DomInput::default())),
            mp_input: Rc::new(RefCell::new(DomInput::default())),
            glyph_cache: Rc::new(RefCell::new(GlyphCache::default())),
            palette,
            scale,
            theme_slot,
            slot_seen_generation,
            pending_theme: None,
            font_family,
            focus: cx.focus_handle().tab_stop(true),
            focus_once: true,
            transport: Rc::new(RefCell::new(TransportState::default())),
            scrub_track: Rc::new(RefCell::new(ScrubTrackGeom::default())),
        }
    }

    /// Map pure transport commands onto the engine (non-blocking bounded send).
    fn dispatch_transport(engine: &Option<EngineHandle>, commands: &[TransportCommand]) {
        let Some(handle) = engine.as_ref() else {
            return;
        };
        for cmd in commands {
            let engine_cmd = match cmd {
                TransportCommand::Play => EngineCmd::Play,
                TransportCommand::Pause => EngineCmd::Pause,
                TransportCommand::SetSpeed(s) => EngineCmd::SetSpeed(*s),
                TransportCommand::Seek { ts, generation } => EngineCmd::Seek {
                    ts: *ts,
                    generation: *generation,
                },
            };
            handle
                .send(engine_cmd)
                .unwrap_or_else(|err| panic!("fft: transport command failed: {err}"));
        }
    }

    /// Drain at most one scrub Seek per frame (latest-wins).
    fn drain_scrub_seek(&self) {
        let cmd = self.transport.borrow_mut().take_coalesced_seek();
        if let Some(cmd) = cmd {
            Self::dispatch_transport(&self.engine_slot.borrow(), std::slice::from_ref(&cmd));
        }
    }

    /// Phase 1 — detect only. Never adopts; render keeps the previous palette/scale.
    ///
    /// GlyphCache keys include color bits and font size ([`crate::glyph_cache`]), so
    /// palette-only and scale changes both miss cold — both take the pending path.
    fn detect_theme_slot(&mut self) {
        let theme_slot = Arc::clone(&self.theme_slot);
        let advanced = note_theme_slot_advance(
            &mut self.pending_theme,
            self.theme_slot.generation(),
            self.slot_seen_generation,
            || theme_slot.load(),
        );
        if advanced {
            self.slot_seen_generation = self
                .pending_theme
                .as_ref()
                .map(|p| p.snap.generation)
                .unwrap_or(self.slot_seen_generation);
        }
    }

    /// Phase 2 — after this frame's element tree is built at the OLD theme, warm then adopt.
    fn warm_and_maybe_adopt_theme(&mut self, window: &mut Window) {
        if self.pending_theme.is_none() {
            return;
        }
        let viewport_h = f32::from(window.viewport_size().height);
        let (center, mp_scale, dom_scale) = {
            let panes = self.panes.borrow();
            (
                panes.effective_center(&self.frame_snapshot.dom),
                panes.mp_scale,
                panes.dom_scale,
            )
        };
        if let Some(pend) = self.pending_theme.as_mut() {
            // Install once; keep cursor progress across frames. Empty → retry later.
            let queue = collect_visible_glyph_jobs(
                &self.frame_snapshot,
                center,
                mp_scale,
                dom_scale,
                &pend.snap.palette,
                pend.snap.scale,
                viewport_h,
            );
            pend.ensure_queue(queue);
        }
        let mut cache = self.glyph_cache.borrow_mut();
        let action = drive_theme_warmup(&mut self.pending_theme, |pend, budget| {
            shape_pending_batch(pend, &mut cache, window, budget)
        });
        drop(cache);
        match action {
            ThemeWarmAction::Idle => {}
            ThemeWarmAction::KeepPending => {
                window.request_animation_frame();
            }
            ThemeWarmAction::Adopt {
                snap,
                warm_frames_used,
                warmed_entries,
            } => {
                // Adopt the snapshot we warmed — never re-load the slot here.
                // A concurrent publish is picked up next frame as a new pending.
                self.palette = Rc::new(snap.palette);
                self.scale = snap.scale;
                eprintln!(
                    "fft: theme adopted after {warm_frames_used} warm frames, {warmed_entries} glyphs pre-shaped"
                );
            }
        }
    }

    fn adopt_replay(&mut self) {
        if self.snapshots.is_none()
            && let Some(ready) = self.replay_ready.borrow_mut().take()
        {
            self.snapshots = Some(ready.snapshots);
            self.wake_dirty = ready.wake_dirty;
        }
    }

    fn start_replay_after_first_paint(&mut self, window: &mut Window) {
        let Some(path) = self.pending_replay.take() else {
            return;
        };
        let replay_at = self.replay_at.take();
        let replay_ready = Rc::clone(&self.replay_ready);
        let engine_slot = Rc::clone(&self.engine_slot);
        window.on_next_frame(move |window, _| {
            let (handle, snapshots, wake_dirty) = spawn_replay_engine(path, replay_at);
            *engine_slot.borrow_mut() = Some(handle);
            *replay_ready.borrow_mut() = Some(ReplayResources {
                snapshots,
                wake_dirty,
            });
            window.refresh();
        });
    }
}

impl Render for Shell {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // Detect before building the tree; do not adopt yet.
        self.detect_theme_slot();
        self.adopt_replay();
        if let Some(slot) = &self.snapshots {
            self.frame_snapshot = slot.load();
            self.wake_dirty.store(false, Ordering::Release);
        }
        self.glyph_cache.borrow_mut().begin_frame();
        if self.harness.borrow_mut().on_frame(Instant::now()) {
            window.request_animation_frame();
        } else {
            cx.defer(|cx| cx.quit());
        }
        self.start_replay_after_first_paint(window);
        if self.snapshots.is_none() {
            // Warm against whatever snapshot exists (may be empty early on).
            self.warm_and_maybe_adopt_theme(window);
            return div()
                .size_full()
                .bg(self.palette.blank_window)
                .into_any_element();
        }
        if self.focus_once {
            self.focus.focus(window, cx);
            self.focus_once = false;
        }

        #[cfg(debug_assertions)]
        if let Err(mismatch) =
            check_pane_agreement(&self.frame_snapshot.profile, &self.frame_snapshot.dom)
        {
            debug_assert!(
                false,
                "MP/DOM volume mismatch at {:?}: profile={}, DOM={}",
                mismatch.price, mismatch.profile_volume, mismatch.dom_volume
            );
        }

        // One scrub Seek max per frame from the latest drag position.
        self.drain_scrub_seek();

        let viewport_width = f32::from(window.viewport_size().width);
        self.panes.borrow_mut().splitter.consume(viewport_width);
        self.panes
            .borrow_mut()
            .clamp_center_to_dom(&self.frame_snapshot.dom);
        let center = self
            .panes
            .borrow()
            .effective_center(&self.frame_snapshot.dom);
        let (mp_scale, dom_scale, ratio) = {
            let panes = self.panes.borrow();
            (panes.mp_scale, panes.dom_scale, panes.splitter.ratio())
        };
        // OLD theme for this frame while a pending warm-up is in flight.
        let ui_scale = self.scale;
        let mp = MarketProfile::new(
            Arc::clone(&self.frame_snapshot),
            center,
            mp_scale,
            Rc::clone(&self.glyph_cache),
            Rc::clone(&self.palette),
            ui_scale,
        );
        let dom = DomLadder::new(
            Arc::clone(&self.frame_snapshot),
            DomView {
                anchor: center,
                tick_scale: dom_scale,
            },
            Rc::clone(&self.glyph_cache),
            Rc::clone(&self.palette),
            ui_scale,
        );

        let mp_pane = shell_panes::mp_pane(
            mp,
            ratio,
            Arc::clone(&self.frame_snapshot),
            Rc::clone(&self.panes),
            Rc::clone(&self.mp_input),
            ui_scale,
        );
        let dom_pane = shell_panes::dom_pane(
            dom,
            1.0 - ratio,
            Arc::clone(&self.frame_snapshot),
            Rc::clone(&self.panes),
            Rc::clone(&self.dom_input),
            ui_scale,
        );
        let splitter = shell_panes::splitter(Rc::clone(&self.panes), Rc::clone(&self.palette));
        let key_panes = Rc::clone(&self.panes);
        let key_transport = Rc::clone(&self.transport);
        let key_engine = Rc::clone(&self.engine_slot);
        let key_applied_ts = self.frame_snapshot.applied_ts;
        let (scrub_first, scrub_last) = scrub_range_from_snapshot(&self.frame_snapshot);
        let key_first = scrub_first;
        let key_last = scrub_last;
        let split_move = Rc::clone(&self.panes);
        let split_end = Rc::clone(&self.panes);
        let split_end_out = Rc::clone(&self.panes);
        let font_family = self.font_family.clone();
        let transport_on = self.transport.borrow().mode_on;
        let strip = if transport_on {
            Some(transport_strip(TransportStripArgs {
                transport: Rc::clone(&self.transport),
                track_geom: Rc::clone(&self.scrub_track),
                palette: Rc::clone(&self.palette),
                scale: ui_scale,
                applied_ts: self.frame_snapshot.applied_ts,
                first_ts: scrub_first,
                last_ts: scrub_last,
                viewport_width,
            }))
        } else {
            None
        };

        // After the tree is built at the old theme: warm the incoming glyph set, then adopt.
        self.warm_and_maybe_adopt_theme(window);

        let panes_row = div()
            .id("fft-panes-row")
            .flex_1()
            .w_full()
            .min_h_0()
            .flex()
            .flex_row()
            .children([mp_pane, splitter, dom_pane]);

        div()
            .id("fft-two-pane-shell")
            .size_full()
            // Family fixed at startup; live family switching is out of scope.
            .font_family(font_family)
            .flex()
            .flex_col()
            .track_focus(&self.focus)
            .on_key_down(move |event, window, cx| {
                if event.keystroke.modifiers.modified() {
                    return;
                }
                let key = event.keystroke.key.as_str();
                // Pane scale / recenter first (unchanged hover-routing).
                let (pane_handled, pane_refresh) = {
                    let mut panes = key_panes.borrow_mut();
                    match key {
                        "1" => (true, panes.set_hovered_scale(1)),
                        "2" => (true, panes.set_hovered_scale(2)),
                        "4" => (true, panes.set_hovered_scale(4)),
                        "t" => (true, panes.sync_scale_from_hovered()),
                        "c" => (true, panes.recenter()),
                        _ => (false, false),
                    }
                };
                if pane_handled {
                    if pane_refresh {
                        window.refresh();
                    }
                    cx.stop_propagation();
                    return;
                }
                let action = {
                    let mut t = key_transport.borrow_mut();
                    match key {
                        "r" => t.toggle_mode(),
                        "space" => t.toggle_play(),
                        "]" => t.speed_up(),
                        "[" => t.speed_down(),
                        "left" => t.step(key_applied_ts, key_first, key_last, false),
                        "right" => t.step(key_applied_ts, key_first, key_last, true),
                        "l" => t.go_live_placeholder(),
                        _ => return,
                    }
                };
                if let Some(hint) = action.status_hint {
                    eprintln!("fft: {hint}");
                }
                Self::dispatch_transport(&key_engine.borrow(), &action.commands);
                if action.refresh {
                    window.refresh();
                }
                cx.stop_propagation();
            })
            .on_mouse_move(move |event, window, _| {
                let mut panes = split_move.borrow_mut();
                if panes.splitter.is_dragging() && event.dragging() {
                    panes.splitter.queue(f32::from(event.position.x));
                    drop(panes);
                    window.refresh();
                }
            })
            .on_mouse_up(MouseButton::Left, move |_, _, _| {
                split_end.borrow_mut().splitter.end();
            })
            .on_mouse_up_out(MouseButton::Left, move |_, _, _| {
                split_end_out.borrow_mut().splitter.end();
            })
            .child(panes_row)
            .children(strip)
            .into_any_element()
    }
}

/// Scrub range: session open…+24h from profile trade_date (engine has no log extent on snapshot).
fn scrub_range_from_snapshot(snap: &RenderSnapshot) -> (u64, u64) {
    if let Some(session) = display_session(&snap.profile)
        && session.trade_date > 0
    {
        return session_range_ns(session.trade_date);
    }
    // Empty snapshot: degenerate range so pure math still clamps.
    (0, 1)
}

fn spawn_replay_engine(
    path: PathBuf,
    replay_at: Option<u64>,
) -> (EngineHandle, SnapshotSlot, Arc<AtomicBool>) {
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
    // Seek pauses; Play must follow. Generation 1 is the first valid UI seek after
    // SetSource resets latest_seek to 0. Transport scrub/step counter starts at 2
    // (`TransportState` / FIRST_UI_SEEK_GENERATION) so it cannot collide with this Seek.
    if let Some(ts) = replay_at {
        handle
            .send(EngineCmd::Seek { ts, generation: 1 })
            .unwrap_or_else(|err| panic!("fft: Seek failed: {err}"));
    }
    handle
        .send(EngineCmd::Play)
        .unwrap_or_else(|err| panic!("fft: Play failed: {err}"));
    let snapshots = handle.snapshots();
    (handle, snapshots, wake_dirty)
}
