//! WindoTrader/Dalton Market Profile as one custom GPUI `Element`.

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;

use fft_core::Price;
use fft_engine::RenderSnapshot;
use gpui::{
    App, Bounds, ContentMask, Element, ElementId, GlobalElementId, InspectorElementId, IntoElement,
    LayoutId, Pixels, Point, ShapedLine, Style, TextAlign, Window, fill, point, px, relative, size,
};

use crate::glyph_cache::GlyphCache;
use crate::layout::format_price;
use crate::layout::format_size;
use crate::mp_layout::{MpStrips, mp_footer_h, mp_row_h, row_y, strips};
use crate::mp_paint::{paint_dividers, paint_period_cursor, paint_rows, paint_semantic_lines};
use crate::mp_prepare::prepare_tpos;
use crate::mp_view::{VisibleProfile, display_session, session_open_footer, visible_rows};
use crate::theme::Palette;

pub struct MarketProfile {
    snapshot: Arc<RenderSnapshot>,
    center: Option<Price>,
    tick_scale: u8,
    glyph_cache: Rc<RefCell<GlyphCache>>,
    palette: Rc<Palette>,
    scale: f32,
}

impl MarketProfile {
    pub fn new(
        snapshot: Arc<RenderSnapshot>,
        center: Option<Price>,
        tick_scale: u8,
        glyph_cache: Rc<RefCell<GlyphCache>>,
        palette: Rc<Palette>,
        scale: f32,
    ) -> Self {
        Self {
            snapshot,
            center,
            tick_scale,
            glyph_cache,
            palette,
            scale,
        }
    }
}

impl IntoElement for MarketProfile {
    type Element = Self;

    fn into_element(self) -> Self::Element {
        self
    }
}

pub(crate) struct PreparedText {
    pub line: ShapedLine,
    pub origin: Point<Pixels>,
    pub align_width: Pixels,
    pub align: TextAlign,
    pub line_height: Pixels,
    pub clip: Bounds<Pixels>,
}

#[derive(Clone, Copy, Default)]
pub(crate) struct Markers {
    pub open: Option<Price>,
    pub vpoc: Option<Price>,
    pub vah: Option<Price>,
    pub val: Option<Price>,
    pub ib_low: Option<Price>,
    pub ib_high: Option<Price>,
    pub current_price: Option<Price>,
    pub current_period: u32,
    pub period_gap: bool,
}

pub struct MpPrepaint {
    pub(crate) texts: Vec<PreparedText>,
    pub(crate) profile: VisibleProfile,
    pub(crate) max_pv: u64,
    pub(crate) max_sv: u64,
    pub(crate) markers: Markers,
}

impl Element for MarketProfile {
    type RequestLayoutState = ();
    type PrepaintState = MpPrepaint;

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
        let footer_h = mp_footer_h(scale);
        let width = f32::from(bounds.size.width);
        let height = f32::from(bounds.size.height);
        let cols = strips(f32::from(bounds.origin.x), width);
        let Some(session) = display_session(&self.snapshot.profile) else {
            return MpPrepaint {
                texts: Vec::new(),
                profile: VisibleProfile::default(),
                max_pv: 0,
                max_sv: 0,
                markers: Markers::default(),
            };
        };
        let profile = visible_rows(
            session,
            self.snapshot.dom.tick_size,
            self.tick_scale,
            self.center,
            crate::mp_layout::max_rows(height, scale),
        );
        let max_pv = profile
            .rows
            .iter()
            .map(|row| row.period_volume)
            .max()
            .unwrap_or(0);
        let max_sv = profile
            .rows
            .iter()
            .map(|row| row.session_volume)
            .max()
            .unwrap_or(0);
        let mut texts = Vec::with_capacity(profile.rows.len() * 7 + 1);
        let mut cache = self.glyph_cache.borrow_mut();
        prepare_rows(
            &profile,
            cols,
            bounds,
            window,
            &mut cache,
            &mut texts,
            &self.palette,
            scale,
        );
        let footer = session_open_footer(session.trade_date);
        let line = cache.get_or_shape(window, footer, self.palette.text, px(11.0 * scale));
        texts.push(PreparedText {
            line,
            origin: point(
                px(f32::from(bounds.origin.x) + 6.0),
                px(f32::from(bounds.origin.y) + height - footer_h + 4.0 * scale),
            ),
            align_width: px((width - 12.0).max(0.0)),
            align: TextAlign::Left,
            line_height: px(footer_h - 4.0 * scale),
            clip: Bounds::new(
                point(
                    bounds.origin.x,
                    px(f32::from(bounds.origin.y) + height - footer_h),
                ),
                size(bounds.size.width, px(footer_h)),
            ),
        });
        drop(cache);
        MpPrepaint {
            texts,
            profile,
            max_pv,
            max_sv,
            markers: Markers {
                open: session.open,
                vpoc: session.vpoc,
                vah: session.vah,
                val: session.val,
                ib_low: session.ib_low,
                ib_high: session.ib_high,
                current_price: self.snapshot.dom.last_trade.map(|trade| trade.price),
                current_period: session.current_period,
                period_gap: session.period_gap,
            },
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
        let footer_h = mp_footer_h(scale);
        let palette = &*self.palette;
        window.paint_quad(fill(bounds, palette.base));
        let width = f32::from(bounds.size.width);
        let height = f32::from(bounds.size.height);
        let origin_x = f32::from(bounds.origin.x);
        let origin_y = f32::from(bounds.origin.y);
        let cols = strips(origin_x, width);
        let body_h = (height - footer_h).max(0.0);
        let footer = Bounds::new(
            point(bounds.origin.x, px(origin_y + body_h)),
            size(bounds.size.width, px(footer_h)),
        );
        window.paint_quad(fill(footer, palette.footer_bg));

        paint_period_cursor(bounds, body_h, cols, prepaint.markers, palette, window);
        paint_rows(bounds, cols, prepaint, palette, scale, window);
        paint_semantic_lines(bounds, body_h, prepaint, palette, scale, window);
        paint_dividers(bounds, body_h, cols, palette, window);

        for prepared in prepaint.texts.drain(..) {
            window
                .with_content_mask(
                    Some(ContentMask {
                        bounds: prepared.clip,
                    }),
                    |window| {
                        prepared.line.paint(
                            prepared.origin,
                            prepared.line_height,
                            prepared.align,
                            Some(prepared.align_width),
                            window,
                            cx,
                        )
                    },
                )
                .expect("fft: MP shaped line paint failed");
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn prepare_rows(
    profile: &VisibleProfile,
    cols: MpStrips,
    bounds: Bounds<Pixels>,
    window: &mut Window,
    cache: &mut GlyphCache,
    texts: &mut Vec<PreparedText>,
    palette: &Palette,
    scale: f32,
) {
    let origin_y = f32::from(bounds.origin.y);
    let rh = mp_row_h(scale);
    for (from_top, row) in profile.rows.iter().rev().enumerate() {
        let y = row_y(origin_y, from_top, scale) + 1.0 * scale;
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
    if text.is_empty() {
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
