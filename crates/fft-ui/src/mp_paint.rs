//! Market Profile paint helpers (split from `mp_element` to stay under ~500 lines).

use gpui::{Bounds, Pixels, Window, fill, point, px, size};

use crate::mp_element::{Markers, MpPrepaint};
use crate::mp_layout::{MpStrips, Strip, mp_row_h, price_line_y, row_y, volume_width};
use crate::mp_view::ETH_PERIOD_COUNT;
use crate::theme::Palette;
use fft_core::Price;

#[derive(Clone, Copy, Debug, PartialEq)]
struct HorizontalClip {
    x: f32,
    w: f32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum SemanticLineSpan {
    CurrentSession,
    FullPane,
}

fn clip_horizontal(x: f32, w: f32, viewport: Strip) -> Option<HorizontalClip> {
    let left = x.max(viewport.x);
    let right = (x + w).min(viewport.x + viewport.w);
    (right > left).then_some(HorizontalClip {
        x: left,
        w: right - left,
    })
}

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
        if let Some(clipped) =
            clip_horizontal(cols.pv.x + 2.0, pv_w, prepaint.layout.strip_viewport)
        {
            window.paint_quad(fill(
                Bounds::new(
                    point(px(clipped.x), px(y + 3.0 * scale)),
                    size(px(clipped.w), px(rh - 6.0 * scale)),
                ),
                palette.pv_bar,
            ));
        }
        // SV = session volume-at-price TOTAL only (René 2026-08-11). No aggressor split.
        let total_w = volume_width(row.session_volume, prepaint.max_sv, cols.sv.w - 4.0);
        if let Some(clipped) =
            clip_horizontal(cols.sv.x + 2.0, total_w, prepaint.layout.strip_viewport)
        {
            window.paint_quad(fill(
                Bounds::new(
                    point(px(clipped.x), px(y + 4.0 * scale)),
                    size(px(clipped.w), px(rh - 8.0 * scale)),
                ),
                palette.sv_total,
            ));
        }
    }
}

pub(crate) fn paint_period_cursor(
    bounds: Bounds<Pixels>,
    body_h: f32,
    cols: MpStrips,
    viewport: Strip,
    markers: Markers,
    palette: &Palette,
    window: &mut Window,
) {
    let period = usize::try_from(markers.current_period).expect("MP period fits usize");
    if period < ETH_PERIOD_COUNT && cols.ep.w > 0.0 {
        let step = cols.ep.w / ETH_PERIOD_COUNT as f32;
        let x = cols.ep.x + period as f32 * step;
        let ep_right = cols.ep.x + cols.ep.w;
        let cursor_w = step.max(1.0).min((ep_right - x).max(0.0));
        if let Some(clipped) = clip_horizontal(x, cursor_w, viewport) {
            window.paint_quad(fill(
                Bounds::new(
                    point(px(clipped.x), bounds.origin.y),
                    size(px(clipped.w), px(body_h)),
                ),
                palette.period_cursor,
            ));
        }
    }
    if markers.period_gap
        && let Some(clipped) = clip_horizontal(cols.pv.x, cols.pv.w, viewport)
    {
        window.paint_quad(fill(
            Bounds::new(
                point(px(clipped.x), bounds.origin.y),
                size(px(clipped.w), px(body_h)),
            ),
            palette.period_gap,
        ));
    }
}

fn semantic_line_horizontal_span(
    bounds: Bounds<Pixels>,
    layout: &crate::mp_layout::SessionLayout,
    span: SemanticLineSpan,
) -> HorizontalClip {
    match span {
        SemanticLineSpan::CurrentSession => {
            let viewport = layout.strip_viewport;
            layout
                .blocks
                .last()
                .and_then(|block| clip_horizontal(block.x, block.w, viewport))
                .unwrap_or(HorizontalClip {
                    x: viewport.x,
                    w: 0.0,
                })
        }
        SemanticLineSpan::FullPane => HorizontalClip {
            x: f32::from(bounds.origin.x),
            w: f32::from(bounds.size.width),
        },
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
    let mut line =
        |price: Option<Price>, color: gpui::Hsla, thickness: f32, span: SemanticLineSpan| {
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
                let horizontal = semantic_line_horizontal_span(bounds, &prepaint.layout, span);
                if horizontal.w > 0.0 {
                    window.paint_quad(fill(
                        Bounds::new(
                            point(px(horizontal.x), px(y - thickness / 2.0)),
                            size(px(horizontal.w), px(thickness)),
                        ),
                        color,
                    ));
                }
            }
        };
    // Lowest to highest priority: open < IB < VA < VPOC < current.
    line(
        prepaint.markers.open,
        palette.session_open,
        1.0 * scale,
        SemanticLineSpan::CurrentSession,
    );
    for (price, color) in [
        (prepaint.markers.ib_high, palette.ib),
        (prepaint.markers.ib_low, palette.ib),
        (prepaint.markers.vah, palette.vah_val),
        (prepaint.markers.val, palette.vah_val),
    ] {
        line(price, color, 1.0 * scale, SemanticLineSpan::CurrentSession);
    }
    line(
        prepaint.markers.vpoc,
        palette.vpoc,
        1.5 * scale,
        SemanticLineSpan::CurrentSession,
    );
    line(
        prepaint.markers.current_price,
        palette.current_price,
        2.0 * scale,
        SemanticLineSpan::FullPane,
    );
}

pub(crate) fn paint_dividers(
    bounds: Bounds<Pixels>,
    body_h: f32,
    cols: MpStrips,
    viewport: Strip,
    palette: &Palette,
    scale: f32,
    window: &mut Window,
) {
    let thickness = scale.max(1.0);
    for x in [cols.ep.x, cols.pv.x, cols.sv.x] {
        if let Some(clipped) = clip_horizontal(x, thickness, viewport) {
            window.paint_quad(fill(
                Bounds::new(
                    point(px(clipped.x), bounds.origin.y),
                    size(px(clipped.w), bounds.size.height),
                ),
                palette.divider,
            ));
        }
    }
    // The axis divider is pinned and intentionally outside the scrolling strip clip.
    window.paint_quad(fill(
        Bounds::new(
            point(px(cols.axis.x), bounds.origin.y),
            size(px(thickness), bounds.size.height),
        ),
        palette.divider,
    ));
    window.paint_quad(fill(
        Bounds::new(
            point(bounds.origin.x, px(f32::from(bounds.origin.y) + body_h)),
            size(bounds.size.width, px(1.0)),
        ),
        palette.divider,
    ));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn horizontal_clip_intersects_viewport() {
        let viewport = Strip { x: 10.0, w: 20.0 };
        assert_eq!(
            clip_horizontal(5.0, 10.0, viewport),
            Some(HorizontalClip { x: 10.0, w: 5.0 })
        );
        assert_eq!(
            clip_horizontal(20.0, 20.0, viewport),
            Some(HorizontalClip { x: 20.0, w: 10.0 })
        );
        assert_eq!(clip_horizontal(0.0, 10.0, viewport), None);
        assert_eq!(clip_horizontal(30.0, 2.0, viewport), None);
    }

    #[test]
    fn semantic_line_span_policy_keeps_current_scoped_and_price_full_pane() {
        let bounds = Bounds::new(point(px(10.0), px(20.0)), size(px(500.0), px(300.0)));
        // Block wider than viewport (zoom=1, multi-session): clip right edge to viewport.
        let layout = crate::mp_layout::session_layout(10.0, 500.0, 5, 60.0, 1.0, 1.0);
        let block = layout.blocks.last().unwrap();
        let viewport = layout.strip_viewport;
        let current =
            semantic_line_horizontal_span(bounds, &layout, SemanticLineSpan::CurrentSession);
        assert_eq!(current.x, block.x.max(viewport.x));
        assert_eq!(
            current.x + current.w,
            (block.x + block.w).min(viewport.x + viewport.w)
        );
        assert_eq!(current.x + current.w, layout.axis.x);

        let full = semantic_line_horizontal_span(bounds, &layout, SemanticLineSpan::FullPane);
        assert_eq!(full, HorizontalClip { x: 10.0, w: 500.0 });
        assert!(full.x < current.x);
        assert!(full.x + full.w > layout.axis.x);
    }

    #[test]
    fn semantic_line_span_clips_to_current_block_when_zoomed_out() {
        let bounds = Bounds::new(point(px(10.0), px(20.0)), size(px(500.0), px(300.0)));
        // Single session, zoom=0.5, pan=0: current body is half the strip; lines must not
        // overpaint empty space past the session block.
        let layout = crate::mp_layout::session_layout(10.0, 500.0, 1, 0.0, 0.5, 1.0);
        let block = layout.blocks.last().unwrap();
        let viewport = layout.strip_viewport;
        assert!(block.x + block.w < viewport.x + viewport.w);

        let current =
            semantic_line_horizontal_span(bounds, &layout, SemanticLineSpan::CurrentSession);
        assert_eq!(current.x, block.x.max(viewport.x));
        assert_eq!(current.x + current.w, block.x + block.w);
        assert!(current.x + current.w < viewport.x + viewport.w);

        let full = semantic_line_horizontal_span(bounds, &layout, SemanticLineSpan::FullPane);
        assert_eq!(full, HorizontalClip { x: 10.0, w: 500.0 });
    }

    #[test]
    fn subpixel_last_period_cursor_stays_inside_ep_and_viewport() {
        let ep = Strip { x: 10.0, w: 23.0 };
        let step = ep.w / ETH_PERIOD_COUNT as f32;
        assert!(step < 1.0);
        let x = ep.x + (ETH_PERIOD_COUNT - 1) as f32 * step;
        let width = step.max(1.0).min(ep.x + ep.w - x);
        let clipped = clip_horizontal(x, width, ep).unwrap();
        assert!(clipped.x + clipped.w <= ep.x + ep.w + f32::EPSILON);
        assert!((clipped.w - step).abs() < f32::EPSILON);
    }
}
