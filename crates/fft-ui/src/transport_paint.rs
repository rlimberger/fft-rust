//! Div-based transport strip chrome (handful of elements — not per-cell).

use std::cell::RefCell;
use std::rc::Rc;

use gpui::{AnyElement, MouseButton, div, prelude::*, px};

use crate::theme::Palette;
use crate::transport::{
    TransportState, format_ct_clock, format_speed, play_glyph, scrub_x_from_ts, transport_h,
};

/// Horizontal padding around strip contents.
const PAD_X: f32 = 8.0;
/// Control cluster width (glyph + speed + clock).
const CONTROLS_W: f32 = 148.0;
/// Scrub track height.
const TRACK_H: f32 = 6.0;
/// Position marker width.
const MARKER_W: f32 = 3.0;

/// Geometry of the scrub track in window space (updated each paint for drag mapping).
#[derive(Debug, Clone, Copy)]
pub struct ScrubTrackGeom {
    pub x: f32,
    pub w: f32,
}

impl Default for ScrubTrackGeom {
    fn default() -> Self {
        Self { x: 0.0, w: 1.0 }
    }
}

/// Inputs for one transport-strip paint.
pub struct TransportStripArgs {
    pub transport: Rc<RefCell<TransportState>>,
    pub track_geom: Rc<RefCell<ScrubTrackGeom>>,
    pub palette: Rc<Palette>,
    pub scale: f32,
    pub applied_ts: u64,
    pub first_ts: u64,
    pub last_ts: u64,
    pub viewport_width: f32,
}

/// Build the bottom transport strip. Caller only mounts when `mode_on`.
pub fn transport_strip(args: TransportStripArgs) -> AnyElement {
    let TransportStripArgs {
        transport,
        track_geom,
        palette,
        scale,
        applied_ts,
        first_ts,
        last_ts,
        viewport_width,
    } = args;

    let h = transport_h(scale);
    let (playing, speed, status, marker_ts) = {
        let t = transport.borrow();
        let marker = t.pending_scrub_ts().unwrap_or(applied_ts);
        (t.playing, t.speed(), t.status_hint, marker)
    };

    let track_x = PAD_X * scale + CONTROLS_W * scale;
    let track_w = (viewport_width - track_x - PAD_X * scale).max(1.0);
    *track_geom.borrow_mut() = ScrubTrackGeom {
        x: track_x,
        w: track_w,
    };

    let clock = format_ct_clock(applied_ts);
    let speed_label = format_speed(speed);
    let glyph = play_glyph(playing);
    let status_text = status.unwrap_or("");

    let marker_x = scrub_x_from_ts(marker_ts, track_x, track_w, first_ts, last_ts) - track_x;
    let marker_x = marker_x.clamp(0.0, (track_w - MARKER_W * scale).max(0.0));

    let drag_transport = Rc::clone(&transport);
    let drag_geom = Rc::clone(&track_geom);
    let move_transport = Rc::clone(&transport);
    let move_geom = Rc::clone(&track_geom);
    let end_transport = Rc::clone(&transport);
    let end_out_transport = Rc::clone(&transport);

    let text_size = px(11.0 * scale);

    div()
        .id("transport-strip")
        .w_full()
        .h(px(h))
        .flex()
        .flex_row()
        .items_center()
        .flex_none()
        .bg(palette.footer_bg)
        .border_t_1()
        .border_color(palette.divider)
        .px(px(PAD_X * scale))
        .gap_2()
        .child(
            div()
                .id("transport-controls")
                .flex()
                .flex_row()
                .items_center()
                .gap_2()
                .flex_none()
                .w(px(CONTROLS_W * scale))
                .child(
                    div()
                        .id("transport-play-glyph")
                        .text_size(text_size)
                        .text_color(palette.text)
                        .child(glyph.to_string()),
                )
                .child(
                    div()
                        .id("transport-speed")
                        .text_size(text_size)
                        .text_color(palette.subtext)
                        .child(speed_label),
                )
                .child(
                    div()
                        .id("transport-clock")
                        .text_size(text_size)
                        .text_color(palette.text)
                        .child(clock),
                ),
        )
        .child(
            div()
                .id("transport-scrub-track")
                .flex_1()
                .h(px(TRACK_H * scale + 8.0 * scale))
                .flex()
                .items_center()
                .cursor_pointer()
                .on_mouse_down(MouseButton::Left, move |event, window, cx| {
                    let geom = *drag_geom.borrow();
                    drag_transport.borrow_mut().begin_scrub(
                        f32::from(event.position.x),
                        geom.x,
                        geom.w,
                        first_ts,
                        last_ts,
                    );
                    window.refresh();
                    cx.stop_propagation();
                })
                .on_mouse_move(move |event, window, _| {
                    if !event.dragging() {
                        return;
                    }
                    let geom = *move_geom.borrow();
                    let mut t = move_transport.borrow_mut();
                    if t.is_scrubbing() {
                        t.queue_scrub(
                            f32::from(event.position.x),
                            geom.x,
                            geom.w,
                            first_ts,
                            last_ts,
                        );
                        drop(t);
                        window.refresh();
                    }
                })
                .on_mouse_up(MouseButton::Left, move |_, window, _| {
                    end_transport.borrow_mut().end_scrub();
                    window.refresh();
                })
                .on_mouse_up_out(MouseButton::Left, move |_, window, _| {
                    end_out_transport.borrow_mut().end_scrub();
                    window.refresh();
                })
                .child(
                    div()
                        .id("transport-scrub-rail")
                        .w_full()
                        .h(px(TRACK_H * scale))
                        .rounded_sm()
                        .bg(palette.surface)
                        .relative()
                        .child(
                            div()
                                .id("transport-scrub-marker")
                                .absolute()
                                .left(px(marker_x))
                                .top(px(0.0))
                                .w(px(MARKER_W * scale))
                                .h_full()
                                .bg(palette.text),
                        ),
                ),
        )
        .when(!status_text.is_empty(), |this| {
            this.child(
                div()
                    .id("transport-status")
                    .flex_none()
                    .text_size(text_size)
                    .text_color(palette.subtext)
                    .child(status_text.to_string()),
            )
        })
        .into_any_element()
}
