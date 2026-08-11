//! DOM ladder paint helpers (split from `dom_ladder` to stay under ~500 lines).

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use gpui::{
    App, BorderStyle, Bounds, DispatchPhase, MouseMoveEvent, Pixels, Window, fill, outline, point,
    px, size,
};

use crate::dom_badges::{IcebergSide, iceberg_badge_bounds, iceberg_badge_visible};
use crate::dom_input::hover_row_from_y;
use crate::dom_ladder::Prepaint;
use crate::dom_view::DomViewRow;
use crate::layout::{
    ColRect, column_rects, depth_block_width, header_h, is_inside_market, row_h, row_top_y,
};
use crate::theme::Palette;

fn note_shaped_line_paint_failure(surface: &'static str) {
    static WARNED: AtomicBool = AtomicBool::new(false);
    static COUNT: AtomicU64 = AtomicU64::new(0);
    let n = COUNT.fetch_add(1, Ordering::Relaxed) + 1;
    if WARNED
        .compare_exchange(false, true, Ordering::Relaxed, Ordering::Relaxed)
        .is_ok()
    {
        eprintln!(
            "fft: WARNING {surface} shaped line paint failed (count={n}); skipping run, frame continues"
        );
    }
}

fn max_side_sizes(rows: &[DomViewRow]) -> (u64, u64, u64) {
    let mut max_bid = 0u64;
    let mut max_ask = 0u64;
    let mut max_vol = 0u64;
    for row in rows {
        max_bid = max_bid.max(row.bid_size);
        max_ask = max_ask.max(row.ask_size);
        max_vol = max_vol.max(row.session_volume);
    }
    (max_bid, max_ask, max_vol)
}

fn paint_iceberg_badge(
    window: &mut Window,
    side: IcebergSide,
    col: ColRect,
    row_y: f32,
    scale: f32,
    color: gpui::Hsla,
) {
    let (x, y, w, h) = iceberg_badge_bounds(side, col, row_y, row_h(scale), scale);
    window.paint_quad(fill(
        Bounds::new(point(px(x), px(y)), size(px(w), px(h))),
        color,
    ));
}

pub(crate) fn paint(
    prepaint: &mut Prepaint,
    palette: &Palette,
    scale: f32,
    bounds: Bounds<Pixels>,
    window: &mut Window,
    cx: &mut App,
) {
    let rh = row_h(scale);
    let hh = header_h(scale);
    window.paint_quad(fill(bounds, palette.base));

    let header = Bounds::new(bounds.origin, size(bounds.size.width, px(hh)));
    window.paint_quad(fill(header, palette.mantle));

    let dom = &prepaint.dom;
    let origin_x = f32::from(bounds.origin.x);
    let origin_y = f32::from(bounds.origin.y);
    let width = f32::from(bounds.size.width);
    let height = f32::from(bounds.size.height);
    let cols = column_rects(origin_x, width);
    let slice = &dom.rows[prepaint.row_range.clone()];
    let (max_bid, max_ask, max_vol) = max_side_sizes(slice);
    let best_bid = dom.best_bid.map(|price| price.0);
    let best_ask = dom.best_ask.map(|price| price.0);
    let visible_count = slice.len();
    let painted_hover = prepaint.hovered_from_top;

    for (from_top, row) in slice.iter().rev().enumerate() {
        let y = row_top_y(origin_y, from_top, scale);
        let row_bounds = Bounds::new(
            point(bounds.origin.x, px(y)),
            size(bounds.size.width, px(rh)),
        );
        if row.source_present && is_inside_market(row.price.0, best_bid, best_ask) {
            window.paint_quad(fill(row_bounds, palette.inside_band));
        }

        let vol_w = depth_block_width(row.session_volume, max_vol, cols[1].w - 8.0);
        if vol_w > 0.0 {
            window.paint_quad(fill(
                Bounds::new(
                    point(px(cols[1].x + 4.0), px(y + 3.0 * scale)),
                    size(px(vol_w), px(rh - 6.0 * scale)),
                ),
                palette.pv_bar,
            ));
        }

        let bid_w = depth_block_width(row.bid_size, max_bid, cols[2].w - 4.0);
        if bid_w > 0.0 {
            window.paint_quad(fill(
                Bounds::new(
                    point(px(cols[2].x + cols[2].w - bid_w - 2.0), px(y + 1.0 * scale)),
                    size(px(bid_w), px(rh - 2.0 * scale)),
                ),
                palette.bid_depth,
            ));
        }

        let ask_w = depth_block_width(row.ask_size, max_ask, cols[5].w - 4.0);
        if ask_w > 0.0 {
            window.paint_quad(fill(
                Bounds::new(
                    point(px(cols[5].x + 2.0), px(y + 1.0 * scale)),
                    size(px(ask_w), px(rh - 2.0 * scale)),
                ),
                palette.ask_depth,
            ));
        }

        if iceberg_badge_visible(row.refresh_bid_count) {
            paint_iceberg_badge(window, IcebergSide::Bid, cols[2], y, scale, palette.iceberg);
        }
        if iceberg_badge_visible(row.refresh_ask_count) {
            paint_iceberg_badge(window, IcebergSide::Ask, cols[5], y, scale, palette.iceberg);
        }

        window.paint_quad(fill(
            Bounds::new(
                point(bounds.origin.x, px(y + rh - 1.0)),
                size(bounds.size.width, px(1.0)),
            ),
            palette.divider,
        ));
    }

    for col in &cols[1..] {
        window.paint_quad(fill(
            Bounds::new(
                point(px(col.x), bounds.origin.y),
                size(px(1.0), bounds.size.height),
            ),
            palette.divider,
        ));
    }

    if let Some(from_top) = painted_hover {
        let y = row_top_y(origin_y, from_top, scale);
        let row_bounds = Bounds::new(
            point(bounds.origin.x, px(y)),
            size(bounds.size.width, px(rh)),
        );
        window.paint_quad(outline(row_bounds, palette.overlay, BorderStyle::default()));
        if let Some(box_bounds) = prepaint.hover_box {
            window.paint_quad(fill(box_bounds, palette.surface));
        }
    }

    let line_height = px(rh - 2.0 * scale);
    for prepared in prepaint.texts.drain(..) {
        if prepared
            .line
            .paint(
                prepared.origin,
                line_height,
                prepared.align,
                Some(prepared.align_width),
                window,
                cx,
            )
            .is_err()
        {
            note_shaped_line_paint_failure("DOM");
        }
    }
    let hover_line_height = px(rh - 4.0 * scale);
    for prepared in prepaint.hover_texts.drain(..) {
        if prepared
            .line
            .paint(
                prepared.origin,
                hover_line_height,
                prepared.align,
                Some(prepared.align_width),
                window,
                cx,
            )
            .is_err()
        {
            note_shaped_line_paint_failure("DOM hover");
        }
    }

    // GPUI does not auto-repaint on mouse-move; refresh only when the
    // hovered body-row index changes (including enter/leave). No entity.update.
    window.on_mouse_event(move |event: &MouseMoveEvent, phase, window, _cx| {
        if phase != DispatchPhase::Bubble {
            return;
        }
        let x = f32::from(event.position.x);
        let y = f32::from(event.position.y);
        let over = x >= origin_x && x < origin_x + width && y >= origin_y && y < origin_y + height;
        let next = if over {
            hover_row_from_y(y, origin_y, scale, visible_count)
        } else {
            None
        };
        if next != painted_hover {
            window.refresh();
        }
    });
}
