//! Prior-session CP strip prepare/paint helpers (keeps mp_element/mp_paint ≤ ~500).

use fft_core::Price;
use fft_engine::ProfileSessionRender;
use gpui::{Bounds, Pixels, TextAlign, Window, fill, point, px, size};

use crate::glyph_cache::GlyphCache;
use crate::mp_element::{Markers, PreparedText};
use crate::mp_layout::{
    SessionBlock, SessionBlockKind, SessionLayout, mp_footer_h, price_line_y, row_y,
};
use crate::mp_prepare::prepare_cp_only;
use crate::mp_view::session_open_footer;
use crate::theme::Palette;

#[allow(clippy::too_many_arguments)]
pub(crate) fn prepare_prior_session(
    session: &ProfileSessionRender,
    profile: &crate::mp_view::VisibleProfile,
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
        for prepared in &mut texts[before..] {
            let left = f32::from(prepared.clip.origin.x).max(strip_left);
            let right = (f32::from(prepared.clip.origin.x) + f32::from(prepared.clip.size.width))
                .min(strip_right);
            let w = (right - left).max(0.0);
            prepared.clip = Bounds::new(
                point(px(left), prepared.clip.origin.y),
                size(px(w), prepared.clip.size.height),
            );
        }
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
                px(footer_x + 2.0),
                px(origin_y + height - footer_h + 4.0 * scale),
            ),
            align_width: px((footer_w - 4.0).max(0.0)),
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
    profile: &crate::mp_view::VisibleProfile,
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
    line(markers.vah, palette.vah_val, 1.0);
    line(markers.val, palette.vah_val, 1.0);
    line(markers.vpoc, palette.vpoc, 1.0);
}

pub(crate) fn paint_session_dividers(
    bounds: Bounds<Pixels>,
    layout: &SessionLayout,
    palette: &Palette,
    window: &mut Window,
) {
    let clip_left = layout.strip_viewport.x;
    let clip_right = layout.strip_viewport.x + layout.strip_viewport.w;
    for x in &layout.dividers {
        if *x < clip_left || *x >= clip_right {
            continue;
        }
        window.paint_quad(fill(
            Bounds::new(
                point(px(*x), bounds.origin.y),
                size(px(2.0), bounds.size.height),
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
        ib_low: None,
        ib_high: None,
        current_price: None,
        current_period: 0,
        period_gap: false,
    }
}

pub(crate) fn is_prior(block: &SessionBlock) -> bool {
    block.kind == SessionBlockKind::Prior
}
