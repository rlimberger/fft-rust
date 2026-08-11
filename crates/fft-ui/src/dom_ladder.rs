//! Daytradr DOM ladder as one custom GPUI `Element` — quads + shaped text, no div-per-cell.

use std::cell::RefCell;
use std::ops::Range;
use std::rc::Rc;
use std::sync::Arc;

use fft_engine::RenderSnapshot;
use gpui::{
    App, Bounds, Element, ElementId, GlobalElementId, InspectorElementId, IntoElement, LayoutId,
    Pixels, Point, ShapedLine, Style, TextAlign, Window, fill, point, px, relative, size,
};

use crate::dom_badges::{
    IcebergSide, format_reload_count, iceberg_badge_bounds, iceberg_badge_visible,
    reload_count_text_origin,
};
use crate::dom_view::{AggregatedDom, DomView, DomViewRow};
use crate::glyph_cache::GlyphCache;
use crate::layout::{
    COL_LABELS, ColRect, column_rects, depth_block_width, format_price, format_size, header_h,
    is_inside_market, max_visible_rows, row_h, row_top_y,
};
use crate::theme::Palette;

/// Single-element DOM ladder driven by one coherent [`RenderSnapshot`].
pub struct DomLadder {
    snapshot: Arc<RenderSnapshot>,
    view: DomView,
    glyph_cache: Rc<RefCell<GlyphCache>>,
    palette: Rc<Palette>,
    scale: f32,
}

impl DomLadder {
    pub fn new(
        snapshot: Arc<RenderSnapshot>,
        view: DomView,
        glyph_cache: Rc<RefCell<GlyphCache>>,
        palette: Rc<Palette>,
        scale: f32,
    ) -> Self {
        Self {
            snapshot,
            view,
            glyph_cache,
            palette,
            scale,
        }
    }
}

impl IntoElement for DomLadder {
    type Element = Self;

    fn into_element(self) -> Self::Element {
        self
    }
}

struct PreparedText {
    line: ShapedLine,
    origin: Point<Pixels>,
    align_width: Pixels,
    align: TextAlign,
}

/// Prepaint cache for shaped glyph runs (associated type on [`Element`]; must be public).
pub struct Prepaint {
    texts: Vec<PreparedText>,
    dom: AggregatedDom,
    row_range: Range<usize>,
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

impl Element for DomLadder {
    type RequestLayoutState = ();
    type PrepaintState = Prepaint;

    fn id(&self) -> Option<ElementId> {
        None
    }

    fn source_location(&self) -> Option<&'static std::panic::Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, Self::RequestLayoutState) {
        let mut style = Style::default();
        style.size.width = relative(1.).into();
        style.size.height = relative(1.).into();
        (window.request_layout(style, [], cx), ())
    }

    fn prepaint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _request_layout: &mut Self::RequestLayoutState,
        window: &mut Window,
        _cx: &mut App,
    ) -> Self::PrepaintState {
        let scale = self.scale;
        let dom = self.view.aggregate(&self.snapshot.dom);
        let font_size = px(12.0 * scale);
        let origin_x = f32::from(bounds.origin.x);
        let origin_y = f32::from(bounds.origin.y);
        let width = f32::from(bounds.size.width);
        let height = f32::from(bounds.size.height);
        let cols = column_rects(origin_x, width);
        let hh = header_h(scale);
        let body_h = (height - hh).max(0.0);
        let max_rows = max_visible_rows(body_h, scale);
        let row_range = self.view.window_range(&dom, max_rows);
        let mut texts = Vec::with_capacity(6 + row_range.len() * 8);
        let mut glyph_cache = self.glyph_cache.borrow_mut();
        let text = self.palette.text;
        let subtext = self.palette.subtext;

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
            // a secondary right-aligned figure collides). Hover/tooltip track owns it.
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

        drop(glyph_cache);
        Prepaint {
            texts,
            dom,
            row_range,
        }
    }

    fn paint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _request_layout: &mut Self::RequestLayoutState,
        prepaint: &mut Self::PrepaintState,
        window: &mut Window,
        cx: &mut App,
    ) {
        let scale = self.scale;
        let rh = row_h(scale);
        let hh = header_h(scale);
        let palette = &*self.palette;
        window.paint_quad(fill(bounds, palette.base));

        let header = Bounds::new(bounds.origin, size(bounds.size.width, px(hh)));
        window.paint_quad(fill(header, palette.mantle));

        let dom = &prepaint.dom;
        let origin_x = f32::from(bounds.origin.x);
        let origin_y = f32::from(bounds.origin.y);
        let width = f32::from(bounds.size.width);
        let cols = column_rects(origin_x, width);
        let slice = &dom.rows[prepaint.row_range.clone()];
        let (max_bid, max_ask, max_vol) = max_side_sizes(slice);
        let best_bid = dom.best_bid.map(|price| price.0);
        let best_ask = dom.best_ask.map(|price| price.0);

        for (from_top, row) in slice.iter().rev().enumerate() {
            let y = row_top_y(origin_y, from_top, scale);
            let row_bounds = Bounds::new(
                point(bounds.origin.x, px(y)),
                size(bounds.size.width, px(rh)),
            );
            if is_inside_market(row.price.0, best_bid, best_ask) {
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

        let line_height = px(rh - 2.0 * scale);
        for prepared in prepaint.texts.drain(..) {
            prepared
                .line
                .paint(
                    prepared.origin,
                    line_height,
                    prepared.align,
                    Some(prepared.align_width),
                    window,
                    cx,
                )
                .expect("fft: shaped line paint failed");
        }
    }
}
