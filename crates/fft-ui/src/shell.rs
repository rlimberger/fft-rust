//! Coherent two-pane shell and zero-`entity.update` frame/input path.

use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Instant;

use fft_engine::{EngineCmd, EngineHandle, RenderSnapshot, SnapshotSlot};
use gpui::{Context, FocusHandle, MouseButton, Render, Window, div, prelude::*};

use crate::dom_input::DomInput;
use crate::dom_ladder::DomLadder;
use crate::dom_view::DomView;
use crate::glyph_cache::GlyphCache;
use crate::harness::Harness;
use crate::header::{FrameCadence, HeaderArgs, contract_context, header_strip};
use crate::mp_element::MarketProfile;
#[cfg(debug_assertions)]
use crate::mp_view::check_pane_agreement;
use crate::mp_view::display_session;
use crate::os_theme::{ThemeSlot, resolve_font_family, spawn_theme_watcher};
use crate::pane_state::PaneState;
use crate::prefs::{Prefs, ShellPrefsHandles};
use crate::prior_discovery::PriorOptions;
use crate::shell_panes;
use crate::shell_replay::spawn_replay_engine;
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
    /// Seek target (ns UTC) after SetSource, before Play.
    replay_at: Option<u64>,
    /// Prior-day logs, oldest-first after Play (`--prior`, ENGINE.md §2).
    prior_sessions: Vec<PathBuf>,
    prior_options: PriorOptions,
    engine_slot: Rc<RefCell<Option<EngineHandle>>>,
    wake_dirty: Arc<AtomicBool>,
    frame_snapshot: Arc<RenderSnapshot>,
    panes: Rc<RefCell<PaneState>>,
    dom_input: Rc<RefCell<DomInput>>,
    mp_input: Rc<RefCell<DomInput>>,
    glyph_cache: Rc<RefCell<GlyphCache>>,
    palette: Rc<Palette>,
    /// OS theme scale (`base_size / 12`).
    scale: f32,
    theme_slot: Arc<ThemeSlot>,
    slot_seen_generation: u64,
    pending_theme: Option<PendingTheme>,
    font_family: String,
    focus: FocusHandle,
    focus_once: bool,
    frame_cadence: FrameCadence,
    transport: Rc<RefCell<TransportState>>,
    scrub_track: Rc<RefCell<ScrubTrackGeom>>,
}

impl Shell {
    pub fn new(
        harness: Rc<RefCell<Harness>>,
        pending_replay: Option<PathBuf>,
        replay_at: Option<u64>,
        prior_sessions: Vec<PathBuf>,
        prior_options: PriorOptions,
        engine_slot: Rc<RefCell<Option<EngineHandle>>>,
        cx: &mut Context<Self>,
    ) -> Self {
        let font_family = resolve_font_family();
        let theme_slot = spawn_theme_watcher();
        let snap = theme_slot.load();
        let slot_seen_generation = snap.generation;
        let palette = Rc::new(snap.palette);
        let scale = snap.scale;
        let prefs = Prefs::load();
        let panes = Rc::new(RefCell::new(PaneState::from_prefs(&prefs)));
        let transport = Rc::new(RefCell::new(TransportState::from_prefs(&prefs)));
        Self {
            harness,
            snapshots: None,
            replay_ready: Rc::new(RefCell::new(None)),
            pending_replay,
            replay_at,
            prior_sessions,
            prior_options,
            engine_slot,
            wake_dirty: Arc::new(AtomicBool::new(false)),
            frame_snapshot: Arc::new(RenderSnapshot::default()),
            panes,
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
            frame_cadence: FrameCadence::default(),
            transport,
            scrub_track: Rc::new(RefCell::new(ScrubTrackGeom::default())),
        }
    }

    /// Quit-time prefs handles (main holds these across `app.run`).
    pub fn prefs_handles(&self) -> ShellPrefsHandles {
        ShellPrefsHandles::new(Rc::clone(&self.panes), Rc::clone(&self.transport))
    }

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

    fn drain_scrub_seek(&self) {
        let cmd = self.transport.borrow_mut().take_coalesced_seek();
        if let Some(cmd) = cmd {
            Self::dispatch_transport(&self.engine_slot.borrow(), std::slice::from_ref(&cmd));
        }
    }

    /// Phase 1 — detect only (palette/scale stay on the pending path until warm).
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

    /// Phase 2 — warm at the OLD theme, then adopt.
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
        let priors = std::mem::take(&mut self.prior_sessions);
        let prior_options = self.prior_options.clone();
        let replay_ready = Rc::clone(&self.replay_ready);
        let engine_slot = Rc::clone(&self.engine_slot);
        let speed = self.transport.borrow().speed();
        window.on_next_frame(move |window, _| {
            let (handle, snapshots, wake_dirty) =
                spawn_replay_engine(path, replay_at, &priors, prior_options, speed);
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
        if self.frame_snapshot.generation > 0 {
            crate::startup_trace::note_first_interactive();
        }
        self.glyph_cache.borrow_mut().begin_frame();
        let frame_now = Instant::now();
        let fps = self.frame_cadence.record(frame_now);
        let keep_going = self.harness.borrow_mut().on_frame(frame_now);
        if crate::startup_trace::complete() || !keep_going {
            cx.defer(|cx| cx.quit());
        } else {
            window.request_animation_frame();
        }
        self.start_replay_after_first_paint(window);
        if self.snapshots.is_none() {
            self.warm_and_maybe_adopt_theme(window);
            let header = header_strip(HeaderArgs {
                palette: Rc::clone(&self.palette),
                scale: self.scale,
                contract: contract_context(&self.frame_snapshot),
                applied_ts: self.frame_snapshot.applied_ts,
                fps,
            });
            return div()
                .id("fft-empty-shell")
                .size_full()
                .font_family(self.font_family.clone())
                .flex()
                .flex_col()
                .child(header)
                .child(
                    div()
                        .flex_1()
                        .w_full()
                        .min_h_0()
                        .bg(self.palette.blank_window),
                )
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
        let header = header_strip(HeaderArgs {
            palette: Rc::clone(&self.palette),
            scale: ui_scale,
            contract: contract_context(&self.frame_snapshot),
            applied_ts: self.frame_snapshot.applied_ts,
            fps,
        });
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
            .font_family(font_family)
            .flex()
            .flex_col()
            .track_focus(&self.focus)
            .on_key_down(move |event, window, cx| {
                if event.keystroke.modifiers.modified() {
                    return;
                }
                let key = event.keystroke.key.as_str();
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
            .child(header)
            .child(panes_row)
            .children(strip)
            .into_any_element()
    }
}

/// Scrub range: session open…+24h from trade_date (snapshot has no log extent).
fn scrub_range_from_snapshot(snap: &RenderSnapshot) -> (u64, u64) {
    if let Some(session) = display_session(&snap.profile)
        && session.trade_date > 0
    {
        return session_range_ns(session.trade_date);
    }
    (0, 1)
}
