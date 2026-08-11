//! Pane chrome (MP/DOM panes + splitter) for the two-pane shell.
//!
//! Split from `shell.rs` so the shell module stays under ~500 lines.

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;

use fft_engine::RenderSnapshot;
use gpui::{AnyElement, MouseButton, ScrollDelta, div, prelude::*, px, relative};

use crate::dom_input::{DomInput, PaneDrag};
use crate::dom_ladder::DomLadder;
use crate::dom_view::DomView;
use crate::layout::{header_h, row_h};
use crate::mp_element::MarketProfile;
use crate::mp_layout::{clamp_pan, mp_row_h, session_layout, zoom_at_cursor};
use crate::mp_view::{current_session, pan_center};
use crate::pane_state::{Pane, PaneState, SPLITTER_WIDTH};
use crate::theme::Palette;

pub(crate) fn mp_pane(
    mut profile: MarketProfile,
    ratio: f32,
    snapshot: Arc<RenderSnapshot>,
    panes: Rc<RefCell<PaneState>>,
    input: Rc<RefCell<DomInput>>,
    scale: f32,
) -> AnyElement {
    // Inject pan/zoom from PaneState without changing shell.rs's MarketProfile::new call.
    {
        let state = panes.borrow();
        profile = profile.with_pan_zoom(state.mp_pan_px, state.mp_zoom);
    }
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
    let wheel_snapshot = Arc::clone(&snapshot);
    let row_height = mp_row_h(scale);
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
                    .begin_drag(f32::from(event.position.x), f32::from(event.position.y));
            }
        })
        .on_mouse_move(move |event, window, _| {
            if !event.dragging() || drag_panes.borrow().splitter.is_dragging() {
                drag_move.borrow_mut().end_drag();
                return;
            }
            let drag = drag_move.borrow_mut().drag_to(
                f32::from(event.position.x),
                f32::from(event.position.y),
                row_height,
            );
            match drag {
                PaneDrag::None => {}
                PaneDrag::Vertical(delta) => {
                    let mut panes = drag_panes.borrow_mut();
                    if let Some(session) = current_session(&drag_snapshot.profile) {
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
                }
                PaneDrag::Horizontal(dx) => {
                    let mut panes = drag_panes.borrow_mut();
                    let sessions = drag_snapshot.profile.sessions.len().max(1);
                    let viewport_w = f32::from(window.viewport_size().width);
                    let pane_w = ((viewport_w - SPLITTER_WIDTH) * panes.splitter.ratio()).max(1.0);
                    let layout = session_layout(
                        0.0,
                        pane_w,
                        sessions,
                        panes.mp_pan_px,
                        panes.mp_zoom,
                        scale,
                    );
                    // Dragging content right (positive dx) decreases pan (reveals older left).
                    let next = panes.mp_pan_px - dx;
                    let clamped = clamp_pan(next, layout.content_width, layout.strip_viewport.w);
                    if (clamped - panes.mp_pan_px).abs() > f32::EPSILON {
                        panes.mp_pan_px = clamped;
                        drop(panes);
                        window.refresh();
                    }
                }
            }
        })
        .on_mouse_up(MouseButton::Left, move |_, _, _| {
            drag_end.borrow_mut().end_drag();
        })
        .on_mouse_up_out(MouseButton::Left, move |_, _, _| {
            drag_end_out.borrow_mut().end_drag();
        })
        .on_scroll_wheel(move |event, window, cx| {
            // Ctrl+wheel: horizontal zoom anchored at cursor x.
            if event.modifiers.control {
                let sessions = wheel_snapshot.profile.sessions.len().max(1);
                let viewport_w = f32::from(window.viewport_size().width);
                let mut panes = wheel_panes.borrow_mut();
                let pane_w = ((viewport_w - SPLITTER_WIDTH) * panes.splitter.ratio()).max(1.0);
                let notches = scroll_notches(event.delta);
                if notches != 0.0 {
                    let cursor_x = f32::from(event.position.x);
                    let (zoom, pan) = zoom_at_cursor(
                        0.0,
                        pane_w,
                        sessions,
                        panes.mp_pan_px,
                        panes.mp_zoom,
                        scale,
                        cursor_x,
                        notches,
                    );
                    if (zoom - panes.mp_zoom).abs() > f32::EPSILON
                        || (pan - panes.mp_pan_px).abs() > f32::EPSILON
                    {
                        panes.mp_zoom = zoom;
                        panes.mp_pan_px = pan;
                        drop(panes);
                        window.refresh();
                    }
                }
                cx.stop_propagation();
                return;
            }
            if event.modifiers.modified() {
                return;
            }
            let rows = scroll_rows(event.delta, row_height);
            let delta = wheel_input.borrow_mut().wheel(rows);
            if delta != 0 {
                let mut panes = wheel_panes.borrow_mut();
                if let Some(session) = current_session(&wheel_snapshot.profile) {
                    panes.center = pan_center(
                        session,
                        wheel_snapshot.dom.tick_size,
                        panes.mp_scale,
                        panes.effective_center(&wheel_snapshot.dom),
                        delta,
                    );
                    panes.clamp_center_to_dom(&wheel_snapshot.dom);
                    drop(panes);
                    window.refresh();
                }
            }
            cx.stop_propagation();
        })
        .child(profile)
        .into_any_element()
}

pub(crate) fn dom_pane(
    ladder: DomLadder,
    ratio: f32,
    snapshot: Arc<RenderSnapshot>,
    panes: Rc<RefCell<PaneState>>,
    input: Rc<RefCell<DomInput>>,
    scale: f32,
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
    let rh = row_h(scale);
    let hh = header_h(scale);
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
            if !drag_split.borrow().splitter.is_dragging() && f32::from(event.position.y) >= hh {
                input.begin_drag(f32::from(event.position.x), f32::from(event.position.y));
            }
        })
        .on_mouse_move(move |event, window, _| {
            if !event.dragging() || drag_panes.borrow().splitter.is_dragging() {
                drag_move.borrow_mut().end_drag();
                return;
            }
            let drag = drag_move.borrow_mut().drag_to(
                f32::from(event.position.x),
                f32::from(event.position.y),
                rh,
            );
            match drag {
                PaneDrag::Vertical(delta) => {
                    if pan_dom(&drag_panes, &drag_snapshot, delta) {
                        window.refresh();
                    }
                }
                PaneDrag::None | PaneDrag::Horizontal(_) => {}
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
            let delta = wheel_input.borrow_mut().wheel(scroll_rows(event.delta, rh));
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

pub(crate) fn splitter(panes: Rc<RefCell<PaneState>>, palette: Rc<Palette>) -> AnyElement {
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

/// Wheel notches for Ctrl+zoom. Positive = zoom in.
fn scroll_notches(delta: ScrollDelta) -> f32 {
    match delta {
        ScrollDelta::Lines(delta) => {
            if delta.y > 0.0 {
                1.0
            } else if delta.y < 0.0 {
                -1.0
            } else {
                0.0
            }
        }
        ScrollDelta::Pixels(delta) => {
            let y = f32::from(delta.y);
            if y > 0.0 {
                1.0
            } else if y < 0.0 {
                -1.0
            } else {
                0.0
            }
        }
    }
}
