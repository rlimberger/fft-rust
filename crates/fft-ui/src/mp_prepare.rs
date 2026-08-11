//! Compact TPO glyph-run preparation shared by CP and EP.

use gpui::{Bounds, Hsla, Pixels, TextAlign, Window, point, px, size};

use crate::glyph_cache::GlyphCache;
use crate::mp_element::PreparedText;
use crate::mp_layout::{MpStrips, Strip, mp_row_h};
use crate::mp_view::{ETH_PERIOD_COUNT, MpRow, TpoKind, for_each_tpo};
use crate::theme::Palette;

#[allow(clippy::too_many_arguments)]
pub(crate) fn prepare_tpos(
    cache: &mut GlyphCache,
    window: &mut Window,
    texts: &mut Vec<PreparedText>,
    row: &MpRow,
    cols: MpStrips,
    y: f32,
    palette: &Palette,
    scale: f32,
) {
    let mut cp_eth = String::new();
    let mut cp_rth = String::new();
    let mut ep_eth = [b' '; ETH_PERIOD_COUNT];
    let mut ep_rth = [b' '; ETH_PERIOD_COUNT];
    for_each_tpo(
        row.eth_periods,
        row.rth_periods,
        |physical, letter, kind| {
            let (cp, other, ep) = match kind {
                TpoKind::Eth => (&mut cp_eth, &mut cp_rth, &mut ep_eth),
                TpoKind::Rth => (&mut cp_rth, &mut cp_eth, &mut ep_rth),
            };
            cp.push(letter);
            other.push(' ');
            ep[physical] = letter as u8;
        },
    );
    let cp_count = cp_eth.len().max(1);
    let cp_font = px(cp_font_size(cols.cp.w, cp_count, scale));
    let ep_font = px(ep_font_size(cols.ep.w, scale));
    prepare_line(
        cache,
        window,
        texts,
        cp_eth,
        cols.cp,
        y,
        cp_font,
        palette.eth_tpo,
        scale,
    );
    prepare_line(
        cache,
        window,
        texts,
        cp_rth,
        cols.cp,
        y,
        cp_font,
        palette.rth_tpo,
        scale,
    );
    prepare_line(
        cache,
        window,
        texts,
        String::from_utf8(ep_eth.to_vec()).expect("MP ETH labels are ASCII"),
        cols.ep,
        y,
        ep_font,
        palette.eth_tpo,
        scale,
    );
    prepare_line(
        cache,
        window,
        texts,
        String::from_utf8(ep_rth.to_vec()).expect("MP RTH labels are ASCII"),
        cols.ep,
        y,
        ep_font,
        palette.rth_tpo,
        scale,
    );
}

/// Letters-only CP column for collapsed prior-session strips (no EP).
#[allow(clippy::too_many_arguments)]
pub(crate) fn prepare_cp_only(
    cache: &mut GlyphCache,
    window: &mut Window,
    texts: &mut Vec<PreparedText>,
    row: &MpRow,
    cp: Strip,
    y: f32,
    palette: &Palette,
    scale: f32,
) {
    let mut cp_eth = String::new();
    let mut cp_rth = String::new();
    for_each_tpo(
        row.eth_periods,
        row.rth_periods,
        |_physical, letter, kind| match kind {
            TpoKind::Eth => {
                cp_eth.push(letter);
                cp_rth.push(' ');
            }
            TpoKind::Rth => {
                cp_rth.push(letter);
                cp_eth.push(' ');
            }
        },
    );
    let cp_count = cp_eth.len().max(1);
    let cp_font = px(cp_font_size(cp.w, cp_count, scale));
    prepare_line(
        cache,
        window,
        texts,
        cp_eth,
        cp,
        y,
        cp_font,
        dimmed(palette.eth_tpo),
        scale,
    );
    prepare_line(
        cache,
        window,
        texts,
        cp_rth,
        cp,
        y,
        cp_font,
        dimmed(palette.rth_tpo),
        scale,
    );
}

fn fitted_font_size(width: f32, glyphs: usize, side_padding: f32, min: f32, max: f32) -> f32 {
    ((width - side_padding) / glyphs.max(1) as f32 / 0.62).clamp(min, max)
}

fn cp_font_size(width: f32, glyphs: usize, scale: f32) -> f32 {
    fitted_font_size(width, glyphs, 6.0 * scale, 7.0 * scale, 8.0 * scale)
}

fn ep_font_size(width: f32, scale: f32) -> f32 {
    fitted_font_size(width, ETH_PERIOD_COUNT, 0.0, 7.0 * scale, 9.0 * scale)
}

fn dimmed(mut color: Hsla) -> Hsla {
    color.a *= 0.55;
    color
}

#[allow(clippy::too_many_arguments)]
fn prepare_line(
    cache: &mut GlyphCache,
    window: &mut Window,
    texts: &mut Vec<PreparedText>,
    text: String,
    strip: Strip,
    y: f32,
    font_size: Pixels,
    color: Hsla,
    scale: f32,
) {
    if text.trim().is_empty() || strip.w <= 0.0 {
        return;
    }
    let rh = mp_row_h(scale);
    let line = cache.get_or_shape(window, text, color, font_size);
    let horizontal_padding = 3.0 * scale;
    texts.push(PreparedText {
        line,
        origin: point(px(strip.x + horizontal_padding), px(y)),
        align_width: px((strip.w - 2.0 * horizontal_padding).max(0.0)),
        align: TextAlign::Left,
        line_height: px(rh - 1.0 * scale),
        clip: Bounds::new(
            point(px(strip.x), px(y - 1.0 * scale)),
            size(px(strip.w), px(rh)),
        ),
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fitted_fonts_use_physical_width_without_scale_squared() {
        assert_eq!(cp_font_size(28.0, 1, 1.0), 8.0);
        assert_eq!(cp_font_size(42.0, 1, 1.5), 12.0);
        assert_eq!(cp_font_size(42.0, 20, 1.5), 10.5);
        assert_eq!(ep_font_size(380.0, 1.0), 9.0);
        assert_eq!(ep_font_size(570.0, 1.5), 13.5);
    }
}
