//! Prior-session CP strip prepare/paint helpers (keeps mp_element/mp_paint ≤ ~500).

use fft_core::Price;
use fft_engine::ProfileSessionRender;
use gpui::{Bounds, Pixels, TextAlign, Window, fill, point, px, size};

use crate::glyph_cache::GlyphCache;
use crate::mp_element::{Markers, PreparedText};
use crate::mp_layout::{
    SessionBlock, SessionBlockKind, SessionLayout, Strip, mp_footer_h, price_line_y, row_y,
};
use crate::mp_prepare::prepare_cp_only;
use crate::mp_view::{VisibleProfile, session_open_footer};
use crate::theme::Palette;

fn horizontal_intersection(x: f32, width: f32, left: f32, right: f32) -> Option<(f32, f32)> {
    let clipped_left = x.max(left);
    let width = (x + width).min(right) - clipped_left;
    (width > 0.0).then_some((clipped_left, width))
}

pub(crate) fn clip_prepared_texts(
    texts: &mut Vec<PreparedText>,
    from: usize,
    left: f32,
    right: f32,
) {
    let mut index = from;
    while index < texts.len() {
        let prepared = &mut texts[index];
        let clipped = horizontal_intersection(
            f32::from(prepared.clip.origin.x),
            f32::from(prepared.clip.size.width),
            left,
            right,
        );
        let Some((clipped_left, width)) = clipped else {
            texts.remove(index);
            continue;
        };
        prepared.clip = Bounds::new(
            point(px(clipped_left), prepared.clip.origin.y),
            size(px(width), prepared.clip.size.height),
        );
        index += 1;
    }
}

pub(crate) fn same_price_ladder(left: &VisibleProfile, right: &VisibleProfile) -> bool {
    left.scaled_tick == right.scaled_tick
        && left.rows.len() == right.rows.len()
        && left
            .rows
            .iter()
            .zip(&right.rows)
            .all(|(left, right)| left.price == right.price)
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn prepare_prior_session(
    session: &ProfileSessionRender,
    profile: &VisibleProfile,
    block: &SessionBlock,
    layout: &SessionLayout,
    bounds: Bounds<Pixels>,
    body_h: f32,
    window: &mut Window,
    cache: &mut GlyphCache,
    texts: &mut Vec<PreparedText>,
    palette: &Palette,
    scale: f32,
) {
    let origin_y = f32::from(bounds.origin.y);
    let footer_h = mp_footer_h(scale);
    let height = f32::from(bounds.size.height);
    let strip_left = layout.strip_viewport.x;
    let strip_right = layout.strip_viewport.x + layout.strip_viewport.w;
    for (from_top, row) in profile.rows.iter().rev().enumerate() {
        let y = row_y(origin_y, from_top, scale) + 1.0 * scale;
        let before = texts.len();
        prepare_cp_only(
            cache,
            window,
            texts,
            row,
            block.strips.cp,
            y,
            palette,
            scale,
        );
        clip_prepared_texts(texts, before, strip_left, strip_right);
    }
    let footer = session_open_footer(session.trade_date);
    let line = cache.get_or_shape(window, footer, palette.subtext, px(11.0 * scale));
    let footer_x = block.x.max(strip_left);
    let footer_right = (block.x + block.w).min(strip_right);
    let footer_w = (footer_right - footer_x).max(0.0);
    if footer_w > 0.0 {
        texts.push(PreparedText {
            line,
            origin: point(
                px(footer_x + 6.0 * scale),
                px(origin_y + height - footer_h + 4.0 * scale),
            ),
            align_width: px((footer_w - 12.0 * scale).max(0.0)),
            align: TextAlign::Left,
            line_height: px(footer_h - 4.0 * scale),
            clip: Bounds::new(
                point(px(footer_x), px(origin_y + body_h)),
                size(px(footer_w), px(footer_h)),
            ),
        });
    }
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn paint_prior_va_vpoc(
    body_h: f32,
    block: &SessionBlock,
    layout: &SessionLayout,
    profile: &VisibleProfile,
    markers: Markers,
    palette: &Palette,
    scale: f32,
    origin_y: f32,
    window: &mut Window,
) {
    let Some(top) = profile.rows.last().map(|row| row.price) else {
        return;
    };
    let clip_left = layout.strip_viewport.x;
    let clip_right = layout.strip_viewport.x + layout.strip_viewport.w;
    let left = block.x.max(clip_left);
    let right = (block.x + block.w).min(clip_right);
    let w = right - left;
    if w <= 0.0 {
        return;
    }
    let mut line = |price: Option<Price>, color: gpui::Hsla, thickness: f32| {
        let Some(y) = price.and_then(|price| {
            let bucket = Price(
                price
                    .0
                    .div_euclid(profile.scaled_tick.0)
                    .checked_mul(profile.scaled_tick.0)
                    .expect("MP prior marker bucket overflows i64"),
            );
            price_line_y(bucket.0, top.0, profile.scaled_tick.0, origin_y, scale)
        }) else {
            return;
        };
        if y >= origin_y && y < origin_y + body_h {
            window.paint_quad(fill(
                Bounds::new(
                    point(px(left), px(y - thickness / 2.0)),
                    size(px(w), px(thickness)),
                ),
                color,
            ));
        }
    };
    line(markers.ib_high, palette.ib, 1.0 * scale);
    line(markers.ib_low, palette.ib, 1.0 * scale);
    line(markers.vah, palette.vah_val, 1.0 * scale);
    line(markers.val, palette.vah_val, 1.0 * scale);
    line(markers.vpoc, palette.vpoc, 1.0 * scale);
}

pub(crate) fn paint_session_dividers(
    bounds: Bounds<Pixels>,
    layout: &SessionLayout,
    palette: &Palette,
    scale: f32,
    window: &mut Window,
) {
    let clip_left = layout.strip_viewport.x;
    let clip_right = layout.strip_viewport.x + layout.strip_viewport.w;
    let thickness = 2.0 * scale;
    for x in &layout.dividers {
        let left = (*x).max(clip_left);
        let right = (*x + thickness).min(clip_right);
        if right <= left {
            continue;
        }
        window.paint_quad(fill(
            Bounds::new(
                point(px(left), bounds.origin.y),
                size(px(right - left), bounds.size.height),
            ),
            palette.divider,
        ));
    }
}

pub(crate) fn prior_markers(session: &ProfileSessionRender) -> Markers {
    Markers {
        open: session.open,
        vpoc: session.vpoc,
        vah: session.vah,
        val: session.val,
        ib_low: session.ib_low,
        ib_high: session.ib_high,
        current_price: None,
        current_period: 0,
        period_gap: false,
    }
}

pub(crate) fn is_prior(block: &SessionBlock) -> bool {
    block.kind == SessionBlockKind::Prior
}

/// True when `[block.x, block.x + block.w)` overlaps `[viewport.x, viewport.x + viewport.w)`.
/// Touching an edge (zero-width intersection) is empty and returns false.
/// Empty blocks/viewports never intersect.
#[inline]
pub(crate) fn block_intersects_viewport(block: &SessionBlock, viewport: Strip) -> bool {
    let block_right = block.x + block.w;
    let viewport_right = viewport.x + viewport.w;
    block.w > 0.0 && viewport.w > 0.0 && block.x < viewport_right && block_right > viewport.x
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mp_layout::{MpStrips, SessionBlockKind, current_session_rest_pan, session_layout};

    fn block(x: f32, w: f32) -> SessionBlock {
        SessionBlock {
            kind: SessionBlockKind::Prior,
            session_index: 0,
            x,
            w,
            strips: MpStrips {
                cp: Strip { x, w },
                ..MpStrips::default()
            },
        }
    }

    #[test]
    fn horizontal_text_clip_rejects_zero_width_masks() {
        assert_eq!(horizontal_intersection(0.0, 10.0, 10.0, 30.0), None);
        assert_eq!(horizontal_intersection(30.0, 10.0, 10.0, 30.0), None);
        assert_eq!(
            horizontal_intersection(5.0, 10.0, 10.0, 30.0),
            Some((10.0, 5.0))
        );
        assert_eq!(
            horizontal_intersection(20.0, 20.0, 10.0, 30.0),
            Some((20.0, 10.0))
        );
    }

    #[test]
    fn block_intersects_viewport_boundaries() {
        let viewport = Strip { x: 100.0, w: 200.0 };

        // Touching edges → empty intersection.
        assert!(!block_intersects_viewport(&block(0.0, 100.0), viewport));
        assert!(!block_intersects_viewport(&block(300.0, 50.0), viewport));
        // Fully outside.
        assert!(!block_intersects_viewport(&block(-50.0, 50.0), viewport));
        assert!(!block_intersects_viewport(&block(350.0, 40.0), viewport));
        // Empty ranges.
        assert!(!block_intersects_viewport(&block(120.0, 0.0), viewport));
        assert!(!block_intersects_viewport(
            &block(100.0, 50.0),
            Strip { x: 100.0, w: 0.0 }
        ));

        // Partials + contained + span + exact cover.
        assert!(block_intersects_viewport(&block(50.0, 60.0), viewport));
        assert!(block_intersects_viewport(&block(280.0, 40.0), viewport));
        assert!(block_intersects_viewport(&block(150.0, 50.0), viewport));
        assert!(block_intersects_viewport(&block(50.0, 300.0), viewport));
        assert!(block_intersects_viewport(&block(100.0, 200.0), viewport));
    }

    #[test]
    fn rest_pan_full_width_priors_are_offscreen() {
        let pane_w = 400.0;
        let scale = 1.0;
        let sessions = 5; // 4 priors + current
        let unpanned = session_layout(0.0, pane_w, sessions, 0.0, 1.0, scale);
        let rest = current_session_rest_pan(unpanned.content_width, unpanned.strip_viewport.w);
        let layout = session_layout(0.0, pane_w, sessions, rest, 1.0, scale);

        for prior in layout.blocks.iter().filter(|b| is_prior(b)) {
            assert!(
                !block_intersects_viewport(prior, layout.strip_viewport),
                "prior {} at rest should be culled (x={} w={} viewport=[{}, {}))",
                prior.session_index,
                prior.x,
                prior.w,
                layout.strip_viewport.x,
                layout.strip_viewport.x + layout.strip_viewport.w
            );
        }
        let current = layout.blocks.last().expect("current block");
        assert!(block_intersects_viewport(current, layout.strip_viewport));
    }

    #[test]
    fn partially_visible_prior_is_kept() {
        let pane_w = 400.0;
        let scale = 1.0;
        let sessions = 5;
        let unpanned = session_layout(0.0, pane_w, sessions, 0.0, 1.0, scale);
        let rest = current_session_rest_pan(unpanned.content_width, unpanned.strip_viewport.w);
        // Ease pan left of rest so the newest prior peeks into the strip viewport.
        let layout = session_layout(0.0, pane_w, sessions, (rest - 10.0).max(0.0), 1.0, scale);
        let newest_prior = layout
            .blocks
            .iter()
            .rev()
            .find(|b| is_prior(b))
            .expect("prior present");
        assert!(block_intersects_viewport(
            newest_prior,
            layout.strip_viewport
        ));
    }
}
