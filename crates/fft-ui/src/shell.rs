//! Coherent shell and zero-`entity.update` frame/input path.
use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Instant;

use fft_engine::{EngineHandle, RenderSnapshot, SnapshotSlot};
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
use crate::os_theme::{ThemeSlot, resolve_font_family, spawn_theme_watcher};
use crate::pane_state::PaneState;
use crate::prefs::{Prefs, ShellPrefsHandles};
use crate::prior_discovery::PriorOptions;
use crate::shell_input;
use crate::shell_panes;
use crate::shell_replay::{spawn_replay_engine, spawn_sim_live_engine};
use crate::theme::Palette;
use crate::theme_warmup::{
    PendingTheme, ThemeWarmAction, collect_visible_glyph_jobs, drive_theme_warmup,
    note_theme_slot_advance, shape_pending_batch,
};
use crate::transport::TransportState;
use crate::transport_paint::{ScrubTrackGeom, TransportStripArgs, transport_strip};

/// Re-export for the `fft` binary CLI / shell constructor.
pub use crate::shell_replay::StartupSource;

struct ReplayResources {
    snapshots: SnapshotSlot,
    wake_dirty: Arc<AtomicBool>,
}

pub struct Shell {
    harness: Rc<RefCell<Harness>>,
    snapshots: Option<SnapshotSlot>,
    replay_ready: Rc<RefCell<Option<ReplayResources>>>,
    startup: Option<StartupSource>,
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
        startup: StartupSource,
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
        let mut transport = TransportState::from_prefs(&prefs);
        // Sim-live / scrub-latency gate: arm transport keys/strip at spawn.
        if matches!(startup, StartupSource::SimLive { .. }) || crate::scrub_latency::enabled() {
            transport.mode_on = true;
        }
        let startup = match startup {
            StartupSource::None => None,
            other => Some(other),
        };
        Self {
            harness,
            snapshots: None,
            replay_ready: Rc::new(RefCell::new(None)),
            startup,
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
            transport: Rc::new(RefCell::new(transport)),
            scrub_track: Rc::new(RefCell::new(ScrubTrackGeom::default())),
        }
    }

    pub fn prefs_handles(&self) -> ShellPrefsHandles {
        ShellPrefsHandles::new(Rc::clone(&self.panes), Rc::clone(&self.transport))
    }

    fn drain_scrub_seek(&self) {
        let cmd = self.transport.borrow_mut().take_coalesced_seek();
        if let Some(cmd) = cmd {
            shell_input::dispatch_transport(&self.engine_slot.borrow(), std::slice::from_ref(&cmd));
        }
    }

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

    fn warm_and_maybe_adopt_theme(&mut self, window: &mut Window) {
        if self.pending_theme.is_none() {
            return;
        }
        let viewport_h = f32::from(window.viewport_size().height);
        let (center, mp_scale, dom_scale) = {
            let panes = self.panes.borrow();
            (
                panes.navigation_center(&self.frame_snapshot.profile, &self.frame_snapshot.dom),
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
        let Some(startup) = self.startup.take() else {
            return;
        };
        let priors = std::mem::take(&mut self.prior_sessions);
        let prior_options = self.prior_options.clone();
        let replay_ready = Rc::clone(&self.replay_ready);
        let engine_slot = Rc::clone(&self.engine_slot);
        let speed = self.transport.borrow().speed();
        // Window paints before engine spawn. Sim-live head snap runs on a worker
        // thread (not here) so the UI never blocks on log I/O.
        window.on_next_frame(move |window, _| {
            let (handle, snapshots, wake_dirty) = match startup {
                StartupSource::None => unreachable!("None stripped before pending startup"),
                StartupSource::Replay { path, replay_at } => {
                    spawn_replay_engine(path, replay_at, &priors, prior_options, speed)
                }
                StartupSource::SimLive {
                    path,
                    head_ts,
                    live_out,
                } => spawn_sim_live_engine(path, head_ts, live_out, speed),
            };
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
        self.detect_theme_slot();
        self.adopt_replay();
        if let Some(slot) = &self.snapshots {
            self.frame_snapshot = slot.load();
            self.wake_dirty.store(false, Ordering::Release);
        }
        if self.frame_snapshot.generation > 0 {
            crate::startup_trace::note_first_interactive();
            crate::scrub_latency::note_rendered(self.frame_snapshot.seek_generation);
        }
        self.glyph_cache.borrow_mut().begin_frame();
        let frame_now = Instant::now();
        let fps = self.frame_cadence.record(frame_now);
        let keep_going = self.harness.borrow_mut().on_frame(frame_now);
        if crate::startup_trace::complete() || crate::scrub_latency::should_quit() || !keep_going {
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
                live_phase: self.frame_snapshot.live_phase,
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
        debug_assert!(
            check_pane_agreement(&self.frame_snapshot.profile, &self.frame_snapshot.dom).is_ok(),
            "MP/DOM volume mismatch"
        );

        self.drain_scrub_seek();
        if crate::scrub_latency::enabled()
            && self.frame_snapshot.generation > 0
            && self.engine_slot.borrow().is_some()
        {
            let (first_ts, last_ts) = shell_input::scrub_range_from_snapshot(&self.frame_snapshot);
            let transport = Rc::clone(&self.transport);
            if crate::scrub_latency::drive_script_if_needed(first_ts, last_ts, move |ts| {
                transport.borrow_mut().script_scrub_release(ts);
            }) {
                cx.defer(|cx| cx.quit());
            }
        }

        let viewport_width = f32::from(window.viewport_size().width);
        if self.panes.borrow().dom_visible() {
            self.panes.borrow_mut().splitter.consume(viewport_width);
        }
        // Free canvas: user pan is not re-clamped to available price range each frame.
        let center = self
            .panes
            .borrow()
            .navigation_center(&self.frame_snapshot.profile, &self.frame_snapshot.dom);
        let (mp_scale, dom_scale, ratio, dom_visible) = {
            let panes = self.panes.borrow();
            (
                panes.mp_scale,
                panes.dom_scale,
                panes.splitter.ratio(),
                panes.dom_visible(),
            )
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
        let mp_pane = shell_panes::mp_pane(
            mp,
            if dom_visible { ratio } else { 1.0 },
            Arc::clone(&self.frame_snapshot),
            Rc::clone(&self.panes),
            Rc::clone(&self.mp_input),
            ui_scale,
        );
        let mut pane_elements = vec![mp_pane];
        if dom_visible {
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
            pane_elements.push(shell_panes::splitter(
                Rc::clone(&self.panes),
                Rc::clone(&self.palette),
            ));
            pane_elements.push(shell_panes::dom_pane(
                dom,
                1.0 - ratio,
                Arc::clone(&self.frame_snapshot),
                Rc::clone(&self.panes),
                Rc::clone(&self.dom_input),
                ui_scale,
            ));
        }
        let key_panes = Rc::clone(&self.panes);
        let key_dom_input = Rc::clone(&self.dom_input);
        let key_transport = Rc::clone(&self.transport);
        let key_engine = Rc::clone(&self.engine_slot);
        let key_applied_ts = self.frame_snapshot.applied_ts;
        let key_live_phase = self.frame_snapshot.live_phase;
        let (scrub_first, scrub_last) =
            shell_input::scrub_range_from_snapshot(&self.frame_snapshot);
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
            live_phase: key_live_phase,
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
            .children(pane_elements);

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
                let Some(refresh) = shell_input::handle_key(
                    event.keystroke.key.as_str(),
                    &shell_input::KeyCtx {
                        panes: &key_panes,
                        dom_input: &key_dom_input,
                        transport: &key_transport,
                        engine: &key_engine,
                        applied_ts: key_applied_ts,
                        first_ts: key_first,
                        last_ts: key_last,
                        live_phase: key_live_phase,
                    },
                ) else {
                    return;
                };
                if refresh {
                    window.refresh();
                }
                cx.stop_propagation();
            })
            .on_mouse_move(move |event, window, _| {
                shell_input::on_splitter_mouse_move(event, &split_move, window);
            })
            .on_mouse_up(MouseButton::Left, move |_, _, _| {
                shell_input::end_splitter_drag(&split_end);
            })
            .on_mouse_up_out(MouseButton::Left, move |_, _, _| {
                shell_input::end_splitter_drag(&split_end_out);
            })
            .child(header)
            .child(panes_row)
            .children(strip)
            .into_any_element()
    }
}
