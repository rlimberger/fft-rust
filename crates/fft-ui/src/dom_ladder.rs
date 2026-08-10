//! Daytradr DOM ladder as one custom GPUI `Element` — quads + shaped text, no div-per-cell.

use std::sync::Arc;

use fft_engine::{DomPriceRow, DomRenderState, RenderSnapshot};
use gpui::{
    App, Bounds, Element, ElementId, GlobalElementId, InspectorElementId, IntoElement, LayoutId,
    Pixels, Point, ShapedLine, SharedString, Style, TextAlign, TextRun, Window, fill, hsla, point,
    px, relative, rgb, size,
};

use crate::layout::{
    COL_LABELS, HEADER_H, ROW_H, column_rects, depth_block_width, format_price, format_size,
    is_inside_market, max_visible_rows, row_top_y, visible_row_window,
};

const BG: u32 = 0x101010;
const HEADER_BG: u32 = 0x1a1a1a;
const INSIDE_BG: u32 = 0x243040;
const BID_DEPTH: u32 = 0x1a3a6e;
const ASK_DEPTH: u32 = 0x8b1a1a;
const VOL_BAR: u32 = 0x3a3a3a;
const GRID: u32 = 0x222222;

/// Single-element DOM ladder driven by one coherent [`RenderSnapshot`].
pub struct DomLadder {
    snapshot: Arc<RenderSnapshot>,
}

impl DomLadder {
    pub fn new(snapshot: Arc<RenderSnapshot>) -> Self {
        Self { snapshot }
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
}

fn text_color() -> gpui::Hsla {
    hsla(0.0, 0.0, 0.85, 1.0)
}

fn muted_color() -> gpui::Hsla {
    hsla(0.0, 0.0, 0.55, 1.0)
}

fn shape(
    window: &mut Window,
    text: impl Into<SharedString>,
    color: gpui::Hsla,
    font_size: Pixels,
) -> ShapedLine {
    let text = text.into();
    let style = window.text_style();
    let run = TextRun {
        len: text.len(),
        font: style.font(),
        color,
        background_color: None,
        underline: None,
        strikethrough: None,
    };
    window
        .text_system()
        .shape_line(text, font_size, &[run], None)
}

fn center_row_index(dom: &DomRenderState) -> Option<usize> {
    if dom.rows.is_empty() {
        return None;
    }
    let target = match (dom.best_bid, dom.best_ask) {
        (Some(b), Some(a)) => (b.0 + a.0) / 2,
        (Some(b), None) => b.0,
        (None, Some(a)) => a.0,
        (None, None) => return Some(dom.rows.len() / 2),
    };
    match dom.rows.binary_search_by_key(&target, |r| r.price.0) {
        Ok(i) => Some(i),
        Err(i) => Some(i.min(dom.rows.len() - 1)),
    }
}

fn max_side_sizes(rows: &[DomPriceRow]) -> (u64, u64, u64) {
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
        let dom = &self.snapshot.dom;
        let font_size = px(12.);
        let origin_x = f32::from(bounds.origin.x);
        let origin_y = f32::from(bounds.origin.y);
        let width = f32::from(bounds.size.width);
        let height = f32::from(bounds.size.height);
        let cols = column_rects(origin_x, width);
        let body_h = (height - HEADER_H).max(0.0);
        let max_rows = max_visible_rows(body_h);
        let (start, end) = visible_row_window(dom.rows.len(), center_row_index(dom), max_rows);
        let mut texts = Vec::with_capacity(6 + (end - start) * 6);

        for (i, label) in COL_LABELS.iter().enumerate() {
            let line = shape(window, *label, muted_color(), font_size);
            let col = cols[i];
            texts.push(PreparedText {
                line,
                origin: point(px(col.x + 4.0), px(origin_y + 4.0)),
                align_width: px(col.w - 8.0),
                align: TextAlign::Left,
            });
        }

        // Descending price: highest at top.
        let slice = &dom.rows[start..end];
        for (from_top, row) in slice.iter().rev().enumerate() {
            let y = row_top_y(origin_y, from_top) + 2.0;
            let cells = [
                (0usize, format_price(row.price.0), TextAlign::Right),
                (1, format_size(row.session_volume), TextAlign::Right),
                (2, format_size(row.bid_size), TextAlign::Right),
                (3, format_size(row.cb), TextAlign::Right),
                (4, format_size(row.ca), TextAlign::Right),
                (5, format_size(row.ask_size), TextAlign::Left),
            ];
            for (ci, text, align) in cells {
                if text.is_empty() {
                    continue;
                }
                let col = cols[ci];
                let line = shape(window, text, text_color(), font_size);
                texts.push(PreparedText {
                    line,
                    origin: point(px(col.x + 4.0), px(y)),
                    align_width: px(col.w - 8.0),
                    align,
                });
            }
        }

        Prepaint { texts }
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
        window.paint_quad(fill(bounds, rgb(BG)));

        let header = Bounds::new(bounds.origin, size(bounds.size.width, px(HEADER_H)));
        window.paint_quad(fill(header, rgb(HEADER_BG)));

        let dom = &self.snapshot.dom;
        let origin_x = f32::from(bounds.origin.x);
        let origin_y = f32::from(bounds.origin.y);
        let width = f32::from(bounds.size.width);
        let height = f32::from(bounds.size.height);
        let cols = column_rects(origin_x, width);
        let body_h = (height - HEADER_H).max(0.0);
        let max_rows = max_visible_rows(body_h);
        let (start, end) = visible_row_window(dom.rows.len(), center_row_index(dom), max_rows);
        let slice = &dom.rows[start..end];
        let (max_bid, max_ask, max_vol) = max_side_sizes(slice);
        let best_bid = dom.best_bid.map(|p| p.0);
        let best_ask = dom.best_ask.map(|p| p.0);

        for (from_top, row) in slice.iter().rev().enumerate() {
            let y = row_top_y(origin_y, from_top);
            let row_bounds = Bounds::new(
                point(bounds.origin.x, px(y)),
                size(bounds.size.width, px(ROW_H)),
            );
            if is_inside_market(row.price.0, best_bid, best_ask) {
                window.paint_quad(fill(row_bounds, rgb(INSIDE_BG)));
            }

            let vol_w = depth_block_width(row.session_volume, max_vol, cols[1].w - 8.0);
            if vol_w > 0.0 {
                window.paint_quad(fill(
                    Bounds::new(
                        point(px(cols[1].x + 4.0), px(y + 3.0)),
                        size(px(vol_w), px(ROW_H - 6.0)),
                    ),
                    rgb(VOL_BAR),
                ));
            }

            let bid_w = depth_block_width(row.bid_size, max_bid, cols[2].w - 4.0);
            if bid_w > 0.0 {
                window.paint_quad(fill(
                    Bounds::new(
                        point(px(cols[2].x + cols[2].w - bid_w - 2.0), px(y + 1.0)),
                        size(px(bid_w), px(ROW_H - 2.0)),
                    ),
                    rgb(BID_DEPTH),
                ));
            }

            let ask_w = depth_block_width(row.ask_size, max_ask, cols[5].w - 4.0);
            if ask_w > 0.0 {
                window.paint_quad(fill(
                    Bounds::new(
                        point(px(cols[5].x + 2.0), px(y + 1.0)),
                        size(px(ask_w), px(ROW_H - 2.0)),
                    ),
                    rgb(ASK_DEPTH),
                ));
            }

            window.paint_quad(fill(
                Bounds::new(
                    point(bounds.origin.x, px(y + ROW_H - 1.0)),
                    size(bounds.size.width, px(1.0)),
                ),
                rgb(GRID),
            ));
        }

        for col in &cols[1..] {
            window.paint_quad(fill(
                Bounds::new(
                    point(px(col.x), bounds.origin.y),
                    size(px(1.0), bounds.size.height),
                ),
                rgb(GRID),
            ));
        }

        let line_height = px(ROW_H - 2.0);
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
