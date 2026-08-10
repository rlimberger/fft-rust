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
use gpui::{
    AnyElement, Context, FocusHandle, MouseButton, Render, ScrollDelta, Window, div, prelude::*,
    px, relative,
};

use crate::dom_input::DomInput;
use crate::dom_ladder::DomLadder;
use crate::dom_view::DomView;
use crate::glyph_cache::GlyphCache;
use crate::harness::Harness;
use crate::layout::{HEADER_H, ROW_H};
use crate::mp_element::MarketProfile;
use crate::mp_layout::MP_ROW_H;
#[cfg(debug_assertions)]
use crate::mp_view::check_pane_agreement;
use crate::mp_view::{display_session, pan_center};
use crate::pane_state::{Pane, PaneState, SPLITTER_WIDTH};
use crate::theme::Palette;

/// Installed family name (`fc-list`); not the bare "JetBrains Mono".
const FONT_FAMILY: &str = "JetBrainsMono Nerd Font";

struct ReplayResources {
    snapshots: SnapshotSlot,
    wake_dirty: Arc<AtomicBool>,
}

pub struct Shell {
    harness: Rc<RefCell<Harness>>,
    snapshots: Option<SnapshotSlot>,
    replay_ready: Rc<RefCell<Option<ReplayResources>>>,
    pending_replay: Option<PathBuf>,
    engine_slot: Rc<RefCell<Option<EngineHandle>>>,
    wake_dirty: Arc<AtomicBool>,
    frame_snapshot: Arc<RenderSnapshot>,
    panes: Rc<RefCell<PaneState>>,
    dom_input: Rc<RefCell<DomInput>>,
    mp_input: Rc<RefCell<DomInput>>,
    glyph_cache: Rc<RefCell<GlyphCache>>,
    palette: Rc<Palette>,
    focus: FocusHandle,
    focus_once: bool,
}

impl Shell {
    pub fn new(
        harness: Rc<RefCell<Harness>>,
        pending_replay: Option<PathBuf>,
        engine_slot: Rc<RefCell<Option<EngineHandle>>>,
        cx: &mut Context<Self>,
    ) -> Self {
        Self {
            harness,
            snapshots: None,
            replay_ready: Rc::new(RefCell::new(None)),
            pending_replay,
            engine_slot,
            wake_dirty: Arc::new(AtomicBool::new(false)),
            frame_snapshot: Arc::new(RenderSnapshot::default()),
            panes: Rc::new(RefCell::new(PaneState::default())),
            dom_input: Rc::new(RefCell::new(DomInput::default())),
            mp_input: Rc::new(RefCell::new(DomInput::default())),
            glyph_cache: Rc::new(RefCell::new(GlyphCache::default())),
            palette: Rc::new(Palette::from_env()),
            focus: cx.focus_handle().tab_stop(true),
            focus_once: true,
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
        let replay_ready = Rc::clone(&self.replay_ready);
        let engine_slot = Rc::clone(&self.engine_slot);
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
}

impl Render for Shell {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
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
        let mp = MarketProfile::new(
            Arc::clone(&self.frame_snapshot),
            center,
            mp_scale,
            Rc::clone(&self.glyph_cache),
            Rc::clone(&self.palette),
        );
        let dom = DomLadder::new(
            Arc::clone(&self.frame_snapshot),
            DomView {
                anchor: center,
                tick_scale: dom_scale,
            },
            Rc::clone(&self.glyph_cache),
            Rc::clone(&self.palette),
        );

        let mp_pane = mp_pane(
            mp,
            ratio,
            Arc::clone(&self.frame_snapshot),
            Rc::clone(&self.panes),
            Rc::clone(&self.mp_input),
        );
        let dom_pane = dom_pane(
            dom,
            1.0 - ratio,
            Arc::clone(&self.frame_snapshot),
            Rc::clone(&self.panes),
            Rc::clone(&self.dom_input),
        );
        let splitter = splitter(Rc::clone(&self.panes), Rc::clone(&self.palette));
        let key_panes = Rc::clone(&self.panes);
        let split_move = Rc::clone(&self.panes);
        let split_end = Rc::clone(&self.panes);
        let split_end_out = Rc::clone(&self.panes);

        div()
            .id("fft-two-pane-shell")
            .size_full()
            .font_family(FONT_FAMILY)
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

fn mp_pane(
    profile: MarketProfile,
    ratio: f32,
    snapshot: Arc<RenderSnapshot>,
    panes: Rc<RefCell<PaneState>>,
    input: Rc<RefCell<DomInput>>,
) -> AnyElement {
    let hover = Rc::clone(&panes);
    let drag_start = Rc::clone(&input);
    let drag_split = Rc::clone(&panes);
    let drag_move = Rc::clone(&input);
    let drag_panes = Rc::clone(&panes);
    let drag_snapshot = Arc::clone(&snapshot);
    let drag_end = Rc::clone(&input);
    let drag_end_out = Rc::clone(&input);
    let wheel_input = Rc::clone(&input);
    let wheel_panes = Rc::clone(&panes);
    div()
        .id("market-profile-pane")
        .h_full()
        .min_w_0()
        .overflow_hidden()
        .flex_shrink_1()
        .flex_basis(relative(ratio))
        .on_hover(move |hovered, _, _| {
            hover
                .borrow_mut()
                .set_hovered(Pane::MarketProfile, *hovered);
        })
        .on_mouse_down(MouseButton::Left, move |event, _, _| {
            if !drag_split.borrow().splitter.is_dragging() {
                drag_start
                    .borrow_mut()
                    .begin_drag(f32::from(event.position.y));
            }
        })
        .on_mouse_move(move |event, window, _| {
            if !event.dragging() || drag_panes.borrow().splitter.is_dragging() {
                drag_move.borrow_mut().end_drag();
                return;
            }
            let delta = drag_move
                .borrow_mut()
                .drag_to(f32::from(event.position.y), MP_ROW_H);
            if delta == 0 {
                return;
            }
            let mut panes = drag_panes.borrow_mut();
            if let Some(session) = display_session(&drag_snapshot.profile) {
                panes.center = pan_center(
                    session,
                    drag_snapshot.dom.tick_size,
                    panes.mp_scale,
                    panes.effective_center(&drag_snapshot.dom),
                    delta,
                );
                panes.clamp_center_to_dom(&drag_snapshot.dom);
                drop(panes);
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
            let rows = scroll_rows(event.delta, MP_ROW_H);
            let delta = wheel_input.borrow_mut().wheel(rows);
            if delta != 0 {
                let mut panes = wheel_panes.borrow_mut();
                if let Some(session) = display_session(&snapshot.profile) {
                    panes.center = pan_center(
                        session,
                        snapshot.dom.tick_size,
                        panes.mp_scale,
                        panes.effective_center(&snapshot.dom),
                        delta,
                    );
                    panes.clamp_center_to_dom(&snapshot.dom);
                    drop(panes);
                    window.refresh();
                }
            }
            cx.stop_propagation();
        })
        .child(profile)
        .into_any_element()
}

fn dom_pane(
    ladder: DomLadder,
    ratio: f32,
    snapshot: Arc<RenderSnapshot>,
    panes: Rc<RefCell<PaneState>>,
    input: Rc<RefCell<DomInput>>,
) -> AnyElement {
    let hover = Rc::clone(&panes);
    let drag_start = Rc::clone(&input);
    let drag_split = Rc::clone(&panes);
    let drag_move = Rc::clone(&input);
    let drag_panes = Rc::clone(&panes);
    let drag_snapshot = Arc::clone(&snapshot);
    let drag_end = Rc::clone(&input);
    let drag_end_out = Rc::clone(&input);
    let wheel_input = Rc::clone(&input);
    let wheel_panes = Rc::clone(&panes);
    div()
        .id("dom-ladder-pane")
        .h_full()
        .min_w_0()
        .overflow_hidden()
        .flex_shrink_1()
        .flex_basis(relative(ratio))
        .on_hover(move |hovered, _, _| {
            hover.borrow_mut().set_hovered(Pane::Dom, *hovered);
        })
        .on_mouse_down(MouseButton::Left, move |event, _, _| {
            let mut input = drag_start.borrow_mut();
            input.end_drag();
            if !drag_split.borrow().splitter.is_dragging()
                && f32::from(event.position.y) >= HEADER_H
            {
                input.begin_drag(f32::from(event.position.y));
            }
        })
        .on_mouse_move(move |event, window, _| {
            if !event.dragging() || drag_panes.borrow().splitter.is_dragging() {
                drag_move.borrow_mut().end_drag();
                return;
            }
            let delta = drag_move
                .borrow_mut()
                .drag_to(f32::from(event.position.y), ROW_H);
            if delta == 0 {
                return;
            }
            let changed = pan_dom(&drag_panes, &drag_snapshot, delta);
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
            let delta = wheel_input
                .borrow_mut()
                .wheel(scroll_rows(event.delta, ROW_H));
            if delta != 0 && pan_dom(&wheel_panes, &snapshot, delta) {
                window.refresh();
            }
            cx.stop_propagation();
        })
        .child(ladder)
        .into_any_element()
}

fn pan_dom(panes: &Rc<RefCell<PaneState>>, snapshot: &RenderSnapshot, delta: i64) -> bool {
    let mut panes = panes.borrow_mut();
    let mut view = DomView {
        anchor: panes.effective_center(&snapshot.dom),
        tick_scale: panes.dom_scale,
    };
    let dom = view.aggregate(&snapshot.dom);
    if !view.pan_rows(&dom, delta) {
        return false;
    }
    panes.center = view.anchor;
    true
}

fn splitter(panes: Rc<RefCell<PaneState>>, palette: Rc<Palette>) -> AnyElement {
    div()
        .id("pane-splitter")
        .w(px(SPLITTER_WIDTH))
        .h_full()
        .flex_none()
        .bg(palette.splitter)
        .cursor_col_resize()
        .on_mouse_down(MouseButton::Left, move |event, window, cx| {
            panes
                .borrow_mut()
                .splitter
                .begin(f32::from(event.position.x));
            window.refresh();
            cx.stop_propagation();
        })
        .into_any_element()
}

fn scroll_rows(delta: ScrollDelta, row_height: f32) -> f32 {
    match delta {
        ScrollDelta::Lines(delta) => delta.y,
        ScrollDelta::Pixels(delta) => f32::from(delta.y) / row_height,
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
