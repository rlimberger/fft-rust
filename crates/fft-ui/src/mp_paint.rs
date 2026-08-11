//! Market Profile paint helpers (split from `mp_element` to stay under ~500 lines).

use gpui::{Bounds, Pixels, Window, fill, point, px, size};

use crate::mp_element::{Markers, MpPrepaint};
use crate::mp_layout::{MpStrips, mp_row_h, price_line_y, row_y, volume_width};
use crate::mp_view::ETH_PERIOD_COUNT;
use crate::theme::Palette;
use fft_core::Price;

pub(crate) fn paint_rows(
    bounds: Bounds<Pixels>,
    cols: MpStrips,
    prepaint: &MpPrepaint,
    palette: &Palette,
    scale: f32,
    window: &mut Window,
) {
    let origin_y = f32::from(bounds.origin.y);
    let rh = mp_row_h(scale);
    let strip_left = prepaint.layout.strip_viewport.x;
    let strip_right = prepaint.layout.strip_viewport.x + prepaint.layout.strip_viewport.w;
    let current_left = prepaint
        .layout
        .blocks
        .last()
        .map(|b| b.x.max(strip_left))
        .unwrap_or(strip_left);
    for (from_top, row) in prepaint.profile.rows.iter().rev().enumerate() {
        let y = row_y(origin_y, from_top, scale);
        let bucket_high = row
            .price
            .0
            .checked_add(prepaint.profile.scaled_tick.0 - 1)
            .expect("MP bucket high overflows i64");
        if prepaint
            .markers
            .val
            .zip(prepaint.markers.vah)
            .is_some_and(|(low, high)| bucket_high >= low.0 && row.price.0 <= high.0)
        {
            let left = current_left;
            let right = cols.axis.x.min(strip_right);
            let w = (right - left).max(0.0);
            if w > 0.0 {
                window.paint_quad(fill(
                    Bounds::new(point(px(left), px(y)), size(px(w), px(rh))),
                    palette.va_bg,
                ));
            }
        }
        let pv_w = volume_width(row.period_volume, prepaint.max_pv, cols.pv.w - 4.0);
        if pv_w > 0.0 {
            let x = cols.pv.x + 2.0;
            if x < strip_right && x + pv_w > strip_left {
                window.paint_quad(fill(
                    Bounds::new(
                        point(px(x), px(y + 3.0 * scale)),
                        size(px(pv_w), px(rh - 6.0 * scale)),
                    ),
                    palette.pv_bar,
                ));
            }
        }
        // SV = session volume-at-price TOTAL only (René 2026-08-11). No aggressor split.
        let total_w = volume_width(row.session_volume, prepaint.max_sv, cols.sv.w - 4.0);
        if total_w > 0.0 {
            let x = cols.sv.x + 2.0;
            if x < strip_right && x + total_w > strip_left {
                window.paint_quad(fill(
                    Bounds::new(
                        point(px(x), px(y + 4.0 * scale)),
                        size(px(total_w), px(rh - 8.0 * scale)),
                    ),
                    palette.sv_total,
                ));
            }
        }
    }
}

pub(crate) fn paint_period_cursor(
    bounds: Bounds<Pixels>,
    body_h: f32,
    cols: MpStrips,
    markers: Markers,
    palette: &Palette,
    window: &mut Window,
) {
    let period = usize::try_from(markers.current_period).expect("MP period fits usize");
    if period < ETH_PERIOD_COUNT && cols.ep.w > 0.0 {
        let step = cols.ep.w / ETH_PERIOD_COUNT as f32;
        window.paint_quad(fill(
            Bounds::new(
                point(px(cols.ep.x + period as f32 * step), bounds.origin.y),
                size(px(step.max(1.0)), px(body_h)),
            ),
            palette.period_cursor,
        ));
    }
    if markers.period_gap && cols.pv.w > 0.0 {
        window.paint_quad(fill(
            Bounds::new(
                point(px(cols.pv.x), bounds.origin.y),
                size(px(cols.pv.w), px(body_h)),
            ),
            palette.period_gap,
        ));
    }
}

pub(crate) fn paint_semantic_lines(
    bounds: Bounds<Pixels>,
    body_h: f32,
    prepaint: &MpPrepaint,
    palette: &Palette,
    scale: f32,
    window: &mut Window,
) {
    let Some(top) = prepaint.profile.rows.last().map(|row| row.price) else {
        return;
    };
    let origin_y = f32::from(bounds.origin.y);
    let mut line = |price: Option<Price>, color: gpui::Hsla, thickness: f32| {
        let Some(y) = price.and_then(|price| {
            let bucket = Price(
                price
                    .0
                    .div_euclid(prepaint.profile.scaled_tick.0)
                    .checked_mul(prepaint.profile.scaled_tick.0)
                    .expect("MP marker bucket overflows i64"),
            );
            price_line_y(
                bucket.0,
                top.0,
                prepaint.profile.scaled_tick.0,
                origin_y,
                scale,
            )
        }) else {
            return;
        };
        if y >= origin_y && y < origin_y + body_h {
            window.paint_quad(fill(
                Bounds::new(
                    point(bounds.origin.x, px(y - thickness / 2.0)),
                    size(bounds.size.width, px(thickness)),
                ),
                color,
            ));
        }
    };
    // Open first (lowest priority); every other marker overdraws it.
    line(prepaint.markers.open, palette.session_open, 1.0);
    line(prepaint.markers.vah, palette.vah_val, 1.0);
    line(prepaint.markers.val, palette.vah_val, 1.0);
    line(prepaint.markers.ib_high, palette.ib, 1.0);
    line(prepaint.markers.ib_low, palette.ib, 1.0);
    line(prepaint.markers.vpoc, palette.vpoc, 1.5);
    line(prepaint.markers.current_price, palette.current_price, 1.0);
}

pub(crate) fn paint_dividers(
    bounds: Bounds<Pixels>,
    body_h: f32,
    cols: MpStrips,
    palette: &Palette,
    window: &mut Window,
) {
    for x in [cols.ep.x, cols.pv.x, cols.sv.x, cols.axis.x] {
        window.paint_quad(fill(
            Bounds::new(
                point(px(x), bounds.origin.y),
                size(px(1.0), bounds.size.height),
            ),
            palette.divider,
        ));
    }
    window.paint_quad(fill(
        Bounds::new(
            point(bounds.origin.x, px(f32::from(bounds.origin.y) + body_h)),
            size(bounds.size.width, px(1.0)),
        ),
        palette.divider,
    ));
}
