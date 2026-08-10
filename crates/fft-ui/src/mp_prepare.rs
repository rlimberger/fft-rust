//! Compact TPO glyph-run preparation shared by CP and EP.

use gpui::{Bounds, Pixels, TextAlign, Window, hsla, point, px, size};

use crate::glyph_cache::GlyphCache;
use crate::mp_element::PreparedText;
use crate::mp_layout::{MP_ROW_H, MpStrips, Strip};
use crate::mp_view::{ETH_PERIOD_COUNT, MpRow, TpoKind, for_each_tpo};

pub(crate) fn prepare_tpos(
    cache: &mut GlyphCache,
    window: &mut Window,
    texts: &mut Vec<PreparedText>,
    row: &MpRow,
    cols: MpStrips,
    y: f32,
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
    let cp_font = px(((cols.cp.w - 6.0) / cp_count as f32 / 0.62).clamp(5.0, 8.0));
    let ep_font = px((cols.ep.w / ETH_PERIOD_COUNT as f32 / 0.62).clamp(5.0, 9.0));
    prepare_line(
        cache,
        window,
        texts,
        cp_eth,
        cols.cp,
        y,
        cp_font,
        eth_color(),
    );
    prepare_line(
        cache,
        window,
        texts,
        cp_rth,
        cols.cp,
        y,
        cp_font,
        rth_color(),
    );
    prepare_line(
        cache,
        window,
        texts,
        String::from_utf8(ep_eth.to_vec()).expect("MP ETH labels are ASCII"),
        cols.ep,
        y,
        ep_font,
        eth_color(),
    );
    prepare_line(
        cache,
        window,
        texts,
        String::from_utf8(ep_rth.to_vec()).expect("MP RTH labels are ASCII"),
        cols.ep,
        y,
        ep_font,
        rth_color(),
    );
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
    color: gpui::Hsla,
) {
    if text.trim().is_empty() {
        return;
    }
    let line = cache.get_or_shape(window, text, color, font_size);
    texts.push(PreparedText {
        line,
        origin: point(px(strip.x + 3.0), px(y)),
        align_width: px((strip.w - 6.0).max(0.0)),
        align: TextAlign::Left,
        line_height: px(MP_ROW_H - 1.0),
        clip: Bounds::new(
            point(px(strip.x), px(y - 1.0)),
            size(px(strip.w), px(MP_ROW_H)),
        ),
    });
}

fn eth_color() -> gpui::Hsla {
    hsla(0.58, 0.10, 0.64, 1.0)
}

fn rth_color() -> gpui::Hsla {
    hsla(0.10, 0.58, 0.68, 1.0)
}
