//! DOM ladder prepaint helpers (split from `dom_ladder` to stay under ~500 lines).

use std::cell::RefCell;
use std::rc::Rc;

use fft_engine::RenderSnapshot;
use gpui::{Bounds, Pixels, TextAlign, Window, point, px, size};

use crate::dom_badges::{
    HoverReadoutBoxArgs, IcebergSide, format_hover_readout, format_reload_count, hover_readout_box,
    iceberg_badge_bounds, iceberg_badge_visible, reload_count_text_origin,
};
use crate::dom_input::hover_row_from_y;
use crate::dom_ladder::{Prepaint, PreparedText};
use crate::dom_view::{DomView, DomViewRow};
use crate::glyph_cache::GlyphCache;
use crate::layout::{
    COL_LABELS, ColRect, column_rects, format_price, format_size, header_h, max_visible_rows,
    row_h, row_top_y,
};
use crate::theme::Palette;

struct ReloadCountArgs<'a> {
    texts: &'a mut Vec<PreparedText>,
    glyph_cache: &'a mut GlyphCache,
    window: &'a mut Window,
    side: IcebergSide,
    col: ColRect,
    row_y: f32,
    count: u32,
    color: gpui::Hsla,
    font_size: Pixels,
    scale: f32,
}

fn push_reload_count(args: ReloadCountArgs<'_>) {
    let ReloadCountArgs {
        texts,
        glyph_cache,
        window,
        side,
        col,
        row_y,
        count,
        color,
        font_size,
        scale,
    } = args;
    let (badge_x, _, badge_w, _) = iceberg_badge_bounds(side, col, row_y, row_h(scale), scale);
    let label = format_reload_count(count);
    let line = glyph_cache.get_or_shape(window, label, color, font_size);
    let text_w = f32::from(line.width());
    let Some((x, y, avail)) =
        reload_count_text_origin(side, col, badge_x, badge_w, row_y, scale, text_w)
    else {
        return;
    };
    let align = match side {
        IcebergSide::Bid => TextAlign::Right,
        IcebergSide::Ask => TextAlign::Left,
    };
    texts.push(PreparedText {
        line,
        origin: point(px(x), px(y)),
        align_width: px(avail),
        align,
    });
}

struct HoverReadoutArgs<'a> {
    texts: &'a mut Vec<PreparedText>,
    glyph_cache: &'a mut GlyphCache,
    window: &'a mut Window,
    row: &'a DomViewRow,
    origin_x: f32,
    origin_y: f32,
    width: f32,
    height: f32,
    from_top: usize,
    scale: f32,
    color: gpui::Hsla,
}

fn prepare_hover_readout(args: HoverReadoutArgs<'_>) -> Bounds<Pixels> {
    let HoverReadoutArgs {
        texts,
        glyph_cache,
        window,
        row,
        origin_x,
        origin_y,
        width,
        height,
        from_top,
        scale,
        color,
    } = args;
    let font_size = px(11.0 * scale);
    let (line1, line2) = format_hover_readout(row);
    let shaped1 = glyph_cache.get_or_shape(window, line1, color, font_size);
    let shaped2 = glyph_cache.get_or_shape(window, line2, color, font_size);
    let content_w = f32::from(shaped1.width()).max(f32::from(shaped2.width()));
    let line_h = row_h(scale) - 4.0 * scale;
    let content_h = line_h * 2.0;
    let (bx, by, bw, bh) = hover_readout_box(HoverReadoutBoxArgs {
        origin_x,
        origin_y,
        width,
        height,
        from_top,
        scale,
        content_w,
        content_h,
    });
    let pad = 4.0 * scale;
    let text_w = (bw - pad * 2.0).max(0.0);
    texts.push(PreparedText {
        line: shaped1,
        origin: point(px(bx + pad), px(by + pad)),
        align_width: px(text_w),
        align: TextAlign::Left,
    });
    texts.push(PreparedText {
        line: shaped2,
        origin: point(px(bx + pad), px(by + pad + line_h)),
        align_width: px(text_w),
        align: TextAlign::Left,
    });
    Bounds::new(point(px(bx), px(by)), size(px(bw), px(bh)))
}

pub(crate) fn prepare(
    snapshot: &RenderSnapshot,
    view: &DomView,
    glyph_cache: &Rc<RefCell<GlyphCache>>,
    palette: &Palette,
    scale: f32,
    bounds: Bounds<Pixels>,
    window: &mut Window,
) -> Prepaint {
    let font_size = px(12.0 * scale);
    let origin_x = f32::from(bounds.origin.x);
    let origin_y = f32::from(bounds.origin.y);
    let width = f32::from(bounds.size.width);
    let height = f32::from(bounds.size.height);
    let cols = column_rects(origin_x, width);
    let hh = header_h(scale);
    let body_h = (height - hh).max(0.0);
    let max_rows = max_visible_rows(body_h, scale);
    let dom = view.aggregate_window(&snapshot.dom, max_rows);
    let row_range = view.window_range(&dom, max_rows);
    let mut texts = Vec::with_capacity(6 + row_range.len() * 8);
    let mut glyph_cache = glyph_cache.borrow_mut();
    let text = palette.text;
    let subtext = palette.subtext;

    for (i, label) in COL_LABELS.iter().enumerate() {
        let line = glyph_cache.get_or_shape(window, *label, subtext, font_size);
        let col = cols[i];
        texts.push(PreparedText {
            line,
            origin: point(px(col.x + 4.0), px(origin_y + 4.0 * scale)),
            align_width: px(col.w - 8.0),
            align: TextAlign::Left,
        });
    }

    // Descending price: highest at top.
    let slice = &dom.rows[row_range.clone()];
    for (from_top, row) in slice.iter().rev().enumerate() {
        let y = row_top_y(origin_y, from_top, scale) + 2.0 * scale;
        let cells = [
            (0usize, format_price(row.price.0), TextAlign::Right),
            (1, format_size(row.session_volume), TextAlign::Right),
            (2, format_size(row.bid_size), TextAlign::Right),
            (3, format_size(row.cb), TextAlign::Right),
            (4, format_size(row.ca), TextAlign::Right),
            (5, format_size(row.ask_size), TextAlign::Left),
        ];
        for (ci, cell_text, align) in cells {
            if cell_text.is_empty() {
                continue;
            }
            let col = cols[ci];
            let line = glyph_cache.get_or_shape(window, cell_text, text, font_size);
            texts.push(PreparedText {
                line,
                origin: point(px(col.x + 4.0), px(y)),
                align_width: px(col.w - 8.0),
                align,
            });
        }

        // Hidden volume stays off the VOL column — too tight at scale 1
        // (COL_FRACTIONS[1]=0.14 already hosts session volume + depth bar;
        // a secondary right-aligned figure collides). Hover track owns it.
        if iceberg_badge_visible(row.refresh_bid_count) {
            push_reload_count(ReloadCountArgs {
                texts: &mut texts,
                glyph_cache: &mut glyph_cache,
                window,
                side: IcebergSide::Bid,
                col: cols[2],
                row_y: row_top_y(origin_y, from_top, scale),
                count: row.refresh_bid_count,
                color: subtext,
                font_size,
                scale,
            });
        }
        if iceberg_badge_visible(row.refresh_ask_count) {
            push_reload_count(ReloadCountArgs {
                texts: &mut texts,
                glyph_cache: &mut glyph_cache,
                window,
                side: IcebergSide::Ask,
                col: cols[5],
                row_y: row_top_y(origin_y, from_top, scale),
                count: row.refresh_ask_count,
                color: subtext,
                font_size,
                scale,
            });
        }
    }

    let mouse = window.mouse_position();
    let hovered_from_top = if bounds.contains(&mouse) {
        hover_row_from_y(f32::from(mouse.y), origin_y, scale, slice.len())
    } else {
        None
    };
    let mut hover_texts = Vec::new();
    let hover_box = hovered_from_top.and_then(|from_top| {
        let row = slice.get(slice.len().checked_sub(1 + from_top)?)?;
        Some(prepare_hover_readout(HoverReadoutArgs {
            texts: &mut hover_texts,
            glyph_cache: &mut glyph_cache,
            window,
            row,
            origin_x,
            origin_y,
            width,
            height,
            from_top,
            scale,
            color: text,
        }))
    });

    drop(glyph_cache);
    Prepaint {
        texts,
        hover_texts,
        hover_box,
        hovered_from_top,
        dom,
        row_range,
    }
}
