//! Pane chrome (MP/DOM panes + splitter) for the two-pane shell.
//!
//! Split from `shell.rs` so the shell module stays under ~500 lines.

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;

use fft_engine::RenderSnapshot;
use gpui::{
    AnyElement, App, Bounds, Element, ElementId, GlobalElementId, InspectorElementId, LayoutId,
    MouseButton, Pixels, ScrollDelta, Window, div, prelude::*, px, relative,
};

use crate::dom_input::{DomInput, PaneDrag};
use crate::dom_ladder::DomLadder;

use crate::layout::{header_h, row_h};
use crate::mp_element::MarketProfile;
use crate::mp_layout::{mp_row_h, scroll_notches, session_layout, zoom_at_cursor};
use crate::mp_view::current_session;
use crate::pane_state::{Pane, PaneState, SPLITTER_WIDTH, pan_center};
use crate::theme::Palette;

pub(crate) fn mp_pane(
    profile: MarketProfile,
    ratio: f32,
    snapshot: Arc<RenderSnapshot>,
    panes: Rc<RefCell<PaneState>>,
    input: Rc<RefCell<DomInput>>,
    scale: f32,
) -> AnyElement {
    let sessions = snapshot.profile.sessions.len();
    let hover = Rc::clone(&panes);
    let drag_start = Rc::clone(&input);
    let drag_split = Rc::clone(&panes);
    let drag_move = Rc::clone(&input);
    let drag_panes = Rc::clone(&panes);
    let drag_snapshot = Arc::clone(&snapshot);
    let drag_end = Rc::clone(&input);
    let drag_end_out = Rc::clone(&input);
    let wheel_panes = Rc::clone(&panes);
    let wheel_snapshot = Arc::clone(&snapshot);
    let row_height = mp_row_h(scale);
    let profile = GeometryCorrectMp::new(profile, sessions, Rc::clone(&panes), scale);
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
        // Left-button only: axis-dominant pan (vertical price / horizontal strips).
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
                    if current_session(&drag_snapshot.profile).is_some()
                        && let Some(center) =
                            panes.navigation_center(&drag_snapshot.profile, &drag_snapshot.dom)
                    {
                        // Free canvas: center is not clamped to available price range.
                        panes.center = Some(pan_center(
                            center,
                            drag_snapshot.dom.tick_size,
                            panes.mp_scale,
                            delta,
                        ));
                        drop(panes);
                        window.refresh();
                    }
                }
                PaneDrag::Horizontal(dx) => {
                    let mut panes = drag_panes.borrow_mut();
                    let sessions = drag_snapshot.profile.sessions.len().max(1);
                    let viewport_w = f32::from(window.viewport_size().width);
                    let pane_w = panes.effective_mp_width(viewport_w);
                    let layout = session_layout(
                        0.0,
                        pane_w,
                        sessions,
                        panes.mp_pan_px,
                        panes.mp_zoom,
                        scale,
                    );
                    // Dragging content right (positive dx) reveals older/left.
                    if panes.navigate_mp_pan(-dx, layout.content_width, layout.strip_viewport.w) {
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
            // Plain wheel and Ctrl+wheel: horizontal zoom anchored at cursor x.
            // Other modifiers (Shift/Alt/…) are ignored. Wheel never pans the MP.
            if event.modifiers.modified() && !event.modifiers.control {
                return;
            }
            let sessions = wheel_snapshot.profile.sessions.len().max(1);
            let viewport_w = f32::from(window.viewport_size().width);
            let mut panes = wheel_panes.borrow_mut();
            let pane_w = panes.effective_mp_width(viewport_w);
            let notches = scroll_notches(scroll_delta_y(event.delta));
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
                let after = session_layout(0.0, pane_w, sessions, pan, zoom, scale);
                if panes.navigate_mp_zoom(zoom, pan, after.content_width, after.strip_viewport.w) {
                    drop(panes);
                    window.refresh();
                }
            }
            cx.stop_propagation();
        })
        .child(profile)
        .into_any_element()
}

struct GeometryCorrectMp {
    /// Kept across GPUI remeasure passes — `request_layout` may run more than once.
    profile: MarketProfile,
    sessions: usize,
    panes: Rc<RefCell<PaneState>>,
    scale: f32,
}

impl GeometryCorrectMp {
    fn new(
        profile: MarketProfile,
        sessions: usize,
        panes: Rc<RefCell<PaneState>>,
        scale: f32,
    ) -> Self {
        Self {
            profile,
            sessions,
            panes,
            scale,
        }
    }

    fn build_child(&self, panes: &PaneState) -> AnyElement {
        self.profile
            .clone()
            .with_pan_zoom(panes.mp_pan_px, panes.mp_zoom)
            .into_any_element()
    }
}

struct GeometryCorrectMpState {
    child: AnyElement,
}

impl Element for GeometryCorrectMp {
    type RequestLayoutState = GeometryCorrectMpState;
    type PrepaintState = ();

    fn id(&self) -> Option<ElementId> {
        None
    }

    fn source_location(&self) -> Option<&'static std::panic::Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, Self::RequestLayoutState) {
        let viewport_w = f32::from(window.viewport_size().width);
        let mut panes = self.panes.borrow_mut();
        let pane_w = panes.effective_mp_width(viewport_w);
        if self.sessions > 0 {
            let layout = session_layout(
                0.0,
                pane_w,
                self.sessions,
                panes.mp_pan_px,
                panes.mp_zoom,
                self.scale,
            );
            panes.reconcile_mp_pan(layout.content_width, layout.strip_viewport.w);
        }
        let mut child = self.build_child(&panes);
        drop(panes);
        let child_layout = child.request_layout(window, cx);
        (child_layout, GeometryCorrectMpState { child })
    }

    fn prepaint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        _bounds: Bounds<Pixels>,
        state: &mut Self::RequestLayoutState,
        window: &mut Window,
        cx: &mut App,
    ) -> Self::PrepaintState {
        state.child.prepaint(window, cx);
    }

    fn paint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        _bounds: Bounds<Pixels>,
        state: &mut Self::RequestLayoutState,
        _prepaint: &mut Self::PrepaintState,
        window: &mut Window,
        cx: &mut App,
    ) {
        state.child.paint(window, cx);
    }
}

impl IntoElement for GeometryCorrectMp {
    type Element = Self;

    fn into_element(self) -> Self::Element {
        self
    }
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
    let Some(center) = panes.navigation_center(&snapshot.profile, &snapshot.dom) else {
        return false;
    };
    // Free canvas: wheel pan is not clamped to available price range.
    let next = pan_center(center, snapshot.dom.tick_size, panes.dom_scale, delta);
    if panes.center == Some(next) {
        return false;
    }
    panes.center = Some(next);
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
    scroll_delta_y(delta) / row_height
}

/// Extract the Y component of a GPUI scroll delta (lines or pixels).
fn scroll_delta_y(delta: ScrollDelta) -> f32 {
    match delta {
        ScrollDelta::Lines(delta) => delta.y,
        ScrollDelta::Pixels(delta) => f32::from(delta.y),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::glyph_cache::GlyphCache;
    use crate::theme::Palette;

    #[test]
    fn geometry_correct_mp_keeps_profile_across_layouts() {
        let profile = MarketProfile::new(
            Arc::new(RenderSnapshot::default()),
            None,
            1,
            Rc::new(RefCell::new(GlyphCache::default())),
            Rc::new(Palette::mocha()),
            1.0,
        );
        let panes = Rc::new(RefCell::new(PaneState::default()));
        let correct = GeometryCorrectMp::new(profile, 0, Rc::clone(&panes), 1.0);
        // Re-layout must not consume the profile (GPUI may request_layout twice).
        let _child1 = correct.build_child(&panes.borrow());
        let _child2 = correct.build_child(&panes.borrow());
    }
}
