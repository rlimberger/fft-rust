//! MP row text preparation (split from `mp_element` to stay under ~500 lines).

use gpui::{Bounds, Pixels, TextAlign, Window, point, px, size};

use crate::glyph_cache::GlyphCache;
use crate::layout::{format_price, format_size};
use crate::mp_layout::{SessionBlock, SessionLayout, mp_row_h, row_y};
use crate::mp_prepare::prepare_tpos;
use crate::mp_sessions::clip_prepared_texts;
use crate::mp_view::VisibleProfile;
use crate::theme::Palette;

use super::PreparedText;

#[allow(clippy::too_many_arguments)]
pub(super) fn prepare_current_rows(
    profile: &VisibleProfile,
    block: &SessionBlock,
    layout: &SessionLayout,
    bounds: Bounds<Pixels>,
    window: &mut Window,
    cache: &mut GlyphCache,
    texts: &mut Vec<PreparedText>,
    palette: &Palette,
    scale: f32,
) {
    let origin_y = f32::from(bounds.origin.y);
    let rh = mp_row_h(scale);
    let strip_left = layout.strip_viewport.x;
    let strip_right = layout.strip_viewport.x + layout.strip_viewport.w;
    let mut cols = block.strips;
    cols.axis = layout.axis;
    for (from_top, row) in profile.rows.iter().rev().enumerate() {
        let y = row_y(origin_y, from_top, scale) + 1.0 * scale;
        let before = texts.len();
        prepare_tpos(cache, window, texts, row, cols, y, palette, scale);
        prepare_number(
            cache,
            window,
            texts,
            row.period_volume,
            cols.pv,
            y,
            palette.text,
            scale,
        );
        prepare_number(
            cache,
            window,
            texts,
            row.session_volume,
            cols.sv,
            y,
            palette.text,
            scale,
        );
        // Clip CP/EP/PV/SV glyphs to the strip viewport (axis stays unclipped).
        clip_prepared_texts(texts, before, strip_left, strip_right);
        if cols.axis.w > 0.0 {
            let line = cache.get_or_shape(
                window,
                format_price(row.price.0),
                palette.text,
                px(10.0 * scale),
            );
            texts.push(PreparedText {
                line,
                origin: point(px(cols.axis.x + 2.0), px(y)),
                align_width: px((cols.axis.w - 4.0).max(0.0)),
                align: TextAlign::Right,
                line_height: px(rh - 1.0 * scale),
                clip: Bounds::new(
                    point(px(cols.axis.x), px(y - 1.0 * scale)),
                    size(px(cols.axis.w), px(rh)),
                ),
            });
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn prepare_number(
    cache: &mut GlyphCache,
    window: &mut Window,
    texts: &mut Vec<PreparedText>,
    value: u64,
    strip: crate::mp_layout::Strip,
    y: f32,
    color: gpui::Hsla,
    scale: f32,
) {
    let text = format_size(value);
    if text.is_empty() || strip.w <= 0.0 {
        return;
    }
    let rh = mp_row_h(scale);
    let line = cache.get_or_shape(window, text, color, px(9.0 * scale));
    texts.push(PreparedText {
        line,
        origin: point(px(strip.x + 2.0), px(y)),
        align_width: px((strip.w - 4.0).max(0.0)),
        align: TextAlign::Right,
        line_height: px(rh - 1.0 * scale),
        clip: Bounds::new(
            point(px(strip.x), px(y - 1.0 * scale)),
            size(px(strip.w), px(rh)),
        ),
    });
}
