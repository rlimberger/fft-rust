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
use crate::os_theme::{ThemeSlot, resolve_font_family, spawn_theme_watcher};
use crate::pane_state::PaneState;
use crate::shell_panes;
use crate::theme::Palette;

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
    theme_generation: u64,
    /// Resolved once before the window opens; live family switching is out of scope.
    font_family: String,
    focus: FocusHandle,
    focus_once: bool,
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
        let theme_generation = snap.generation;
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
            theme_generation,
            font_family,
            focus: cx.focus_handle().tab_stop(true),
            focus_once: true,
        }
    }

    /// Per-frame u64 load + compare; no entity.update / notify.
    fn pickup_theme_if_changed(&mut self) {
        let next = self.theme_slot.generation();
        if next == self.theme_generation {
            return;
        }
        let snap = self.theme_slot.load();
        self.theme_generation = snap.generation;
        self.palette = Rc::new(snap.palette);
        self.scale = snap.scale;
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
        self.pickup_theme_if_changed();
        self.adopt_replay();
        if let Some(slot) = &self.snapshots {
            self.frame_snapshot = slot.load();
            self.wake_dirty.store(false, Ordering::Release);
        }
        if self.harness.borrow_mut().on_frame(Instant::now()) {
            window.request_animation_frame();
        } else {
            cx.defer(|cx| cx.quit());
        }
        self.start_replay_after_first_paint(window);
        if self.snapshots.is_none() {
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

        let viewport_width = f32::from(window.viewport_size().width);
        self.panes.borrow_mut().splitter.consume(viewport_width);
        self.panes
            .borrow_mut()
            .clamp_center_to_dom(&self.frame_snapshot.dom);
        self.glyph_cache.borrow_mut().begin_frame();
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
        let split_move = Rc::clone(&self.panes);
        let split_end = Rc::clone(&self.panes);
        let split_end_out = Rc::clone(&self.panes);
        let font_family = self.font_family.clone();

        div()
            .id("fft-two-pane-shell")
            .size_full()
            // Family fixed at startup; live family switching is out of scope.
            .font_family(font_family)
            .flex()
            .flex_row()
            .track_focus(&self.focus)
            .on_key_down(move |event, window, cx| {
                if event.keystroke.modifiers.modified() {
                    return;
                }
                let (handled, changed) = {
                    let mut panes = key_panes.borrow_mut();
                    match event.keystroke.key.as_str() {
                        "1" => (true, panes.set_hovered_scale(1)),
                        "2" => (true, panes.set_hovered_scale(2)),
                        "4" => (true, panes.set_hovered_scale(4)),
                        "t" => (true, panes.sync_scale_from_hovered()),
                        "c" => (true, panes.recenter()),
                        _ => (false, false),
                    }
                };
                if changed {
                    window.refresh();
                }
                if handled {
                    cx.stop_propagation();
                }
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
            .children([mp_pane, splitter, dom_pane])
            .into_any_element()
    }
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
    // SetSource resets latest_seek to 0. No fft-ui scrub Seek path exists yet — when
    // one lands, its counter must start at ≥ 2 so it cannot collide with this Seek.
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
