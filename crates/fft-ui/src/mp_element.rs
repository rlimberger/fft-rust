//! WindoTrader/Dalton Market Profile as one custom GPUI `Element`.

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;

use fft_core::Price;
use fft_engine::RenderSnapshot;
use gpui::{
    App, Bounds, ContentMask, Element, ElementId, GlobalElementId, InspectorElementId, IntoElement,
    LayoutId, Pixels, Point, ShapedLine, Style, TextAlign, Window, fill, hsla, point, px, relative,
    rgb, size,
};

use crate::glyph_cache::GlyphCache;
use crate::layout::{format_price, format_size};
use crate::mp_layout::{
    MP_FOOTER_H, MP_ROW_H, MpStrips, price_line_y, row_y, strips, volume_width,
};
use crate::mp_prepare::prepare_tpos;
use crate::mp_view::{
    ETH_PERIOD_COUNT, VisibleProfile, display_session, session_open_footer, visible_rows,
};

const BG: u32 = 0x0d1014;
const FOOTER_BG: u32 = 0x15191f;
const VA_BG: u32 = 0x151c25;
const PV_BAR: u32 = 0x35404c;
const SV_TOTAL: u32 = 0x30363d;
const BUY: u32 = 0x315f82;
const SELL: u32 = 0x7a3d43;
const DIVIDER: u32 = 0x2a3038;

pub struct MarketProfile {
    snapshot: Arc<RenderSnapshot>,
    center: Option<Price>,
    tick_scale: u8,
    glyph_cache: Rc<RefCell<GlyphCache>>,
}

impl MarketProfile {
    pub fn new(
        snapshot: Arc<RenderSnapshot>,
        center: Option<Price>,
        tick_scale: u8,
        glyph_cache: Rc<RefCell<GlyphCache>>,
    ) -> Self {
        Self {
            snapshot,
            center,
            tick_scale,
            glyph_cache,
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
struct Markers {
    vpoc: Option<Price>,
    vah: Option<Price>,
    val: Option<Price>,
    ib_low: Option<Price>,
    ib_high: Option<Price>,
    current_price: Option<Price>,
    current_period: u32,
    period_gap: bool,
}

pub struct MpPrepaint {
    texts: Vec<PreparedText>,
    profile: VisibleProfile,
    max_pv: u64,
    max_sv: u64,
    markers: Markers,
}

fn text_color() -> gpui::Hsla {
    hsla(0.0, 0.0, 0.82, 1.0)
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
            crate::mp_layout::max_rows(height),
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
        prepare_rows(&profile, cols, bounds, window, &mut cache, &mut texts);
        let footer = session_open_footer(session.trade_date);
        let line = cache.get_or_shape(window, footer, text_color(), px(11.0));
        texts.push(PreparedText {
            line,
            origin: point(
                px(f32::from(bounds.origin.x) + 6.0),
                px(f32::from(bounds.origin.y) + height - MP_FOOTER_H + 4.0),
            ),
            align_width: px((width - 12.0).max(0.0)),
            align: TextAlign::Left,
            line_height: px(MP_FOOTER_H - 4.0),
            clip: Bounds::new(
                point(
                    bounds.origin.x,
                    px(f32::from(bounds.origin.y) + height - MP_FOOTER_H),
                ),
                size(bounds.size.width, px(MP_FOOTER_H)),
            ),
        });
        drop(cache);
        MpPrepaint {
            texts,
            profile,
            max_pv,
            max_sv,
            markers: Markers {
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
        window.paint_quad(fill(bounds, rgb(BG)));
        let width = f32::from(bounds.size.width);
        let height = f32::from(bounds.size.height);
        let origin_x = f32::from(bounds.origin.x);
        let origin_y = f32::from(bounds.origin.y);
        let cols = strips(origin_x, width);
        let body_h = (height - MP_FOOTER_H).max(0.0);
        let footer = Bounds::new(
            point(bounds.origin.x, px(origin_y + body_h)),
            size(bounds.size.width, px(MP_FOOTER_H)),
        );
        window.paint_quad(fill(footer, rgb(FOOTER_BG)));

        paint_period_cursor(bounds, body_h, cols, prepaint.markers, window);
        paint_rows(bounds, cols, prepaint, window);
        paint_semantic_lines(bounds, body_h, prepaint, window);
        paint_dividers(bounds, body_h, cols, window);

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

fn prepare_rows(
    profile: &VisibleProfile,
    cols: MpStrips,
    bounds: Bounds<Pixels>,
    window: &mut Window,
    cache: &mut GlyphCache,
    texts: &mut Vec<PreparedText>,
) {
    let origin_y = f32::from(bounds.origin.y);
    for (from_top, row) in profile.rows.iter().rev().enumerate() {
        let y = row_y(origin_y, from_top) + 1.0;
        prepare_tpos(cache, window, texts, row, cols, y);
        prepare_number(cache, window, texts, row.period_volume, cols.pv, y);
        prepare_number(cache, window, texts, row.session_volume, cols.sv, y);
        let line = cache.get_or_shape(window, format_price(row.price.0), text_color(), px(10.0));
        texts.push(PreparedText {
            line,
            origin: point(px(cols.axis.x + 2.0), px(y)),
            align_width: px((cols.axis.w - 4.0).max(0.0)),
            align: TextAlign::Right,
            line_height: px(MP_ROW_H - 1.0),
            clip: Bounds::new(
                point(px(cols.axis.x), px(y - 1.0)),
                size(px(cols.axis.w), px(MP_ROW_H)),
            ),
        });
    }
}

fn prepare_number(
    cache: &mut GlyphCache,
    window: &mut Window,
    texts: &mut Vec<PreparedText>,
    value: u64,
    strip: crate::mp_layout::Strip,
    y: f32,
) {
    let text = format_size(value);
    if text.is_empty() {
        return;
    }
    let line = cache.get_or_shape(window, text, text_color(), px(9.0));
    texts.push(PreparedText {
        line,
        origin: point(px(strip.x + 2.0), px(y)),
        align_width: px((strip.w - 4.0).max(0.0)),
        align: TextAlign::Right,
        line_height: px(MP_ROW_H - 1.0),
        clip: Bounds::new(
            point(px(strip.x), px(y - 1.0)),
            size(px(strip.w), px(MP_ROW_H)),
        ),
    });
}

fn paint_rows(bounds: Bounds<Pixels>, cols: MpStrips, prepaint: &MpPrepaint, window: &mut Window) {
    let origin_y = f32::from(bounds.origin.y);
    for (from_top, row) in prepaint.profile.rows.iter().rev().enumerate() {
        let y = row_y(origin_y, from_top);
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
            window.paint_quad(fill(
                Bounds::new(
                    point(bounds.origin.x, px(y)),
                    size(px(cols.axis.x - f32::from(bounds.origin.x)), px(MP_ROW_H)),
                ),
                rgb(VA_BG),
            ));
        }
        let pv_w = volume_width(row.period_volume, prepaint.max_pv, cols.pv.w - 4.0);
        if pv_w > 0.0 {
            window.paint_quad(fill(
                Bounds::new(
                    point(px(cols.pv.x + 2.0), px(y + 3.0)),
                    size(px(pv_w), px(MP_ROW_H - 6.0)),
                ),
                rgb(PV_BAR),
            ));
        }
        let total_w = volume_width(row.session_volume, prepaint.max_sv, cols.sv.w - 4.0);
        if total_w > 0.0 {
            window.paint_quad(fill(
                Bounds::new(
                    point(px(cols.sv.x + 2.0), px(y + 4.0)),
                    size(px(total_w), px(MP_ROW_H - 8.0)),
                ),
                rgb(SV_TOTAL),
            ));
        }
        let half = (cols.sv.w - 4.0) / 2.0;
        let center = cols.sv.x + cols.sv.w / 2.0;
        let sell_w = volume_width(row.sell_volume, prepaint.max_sv, half);
        let buy_w = volume_width(row.buy_volume, prepaint.max_sv, half);
        if sell_w > 0.0 {
            window.paint_quad(fill(
                Bounds::new(
                    point(px(center - sell_w), px(y + 2.0)),
                    size(px(sell_w), px(MP_ROW_H - 4.0)),
                ),
                rgb(SELL),
            ));
        }
        if buy_w > 0.0 {
            window.paint_quad(fill(
                Bounds::new(
                    point(px(center), px(y + 2.0)),
                    size(px(buy_w), px(MP_ROW_H - 4.0)),
                ),
                rgb(BUY),
            ));
        }
    }
}

fn paint_period_cursor(
    bounds: Bounds<Pixels>,
    body_h: f32,
    cols: MpStrips,
    markers: Markers,
    window: &mut Window,
) {
    let period = usize::try_from(markers.current_period).expect("MP period fits usize");
    if period < ETH_PERIOD_COUNT {
        let step = cols.ep.w / ETH_PERIOD_COUNT as f32;
        window.paint_quad(fill(
            Bounds::new(
                point(px(cols.ep.x + period as f32 * step), bounds.origin.y),
                size(px(step.max(1.0)), px(body_h)),
            ),
            hsla(0.10, 0.30, 0.35, 0.16),
        ));
    }
    if markers.period_gap {
        window.paint_quad(fill(
            Bounds::new(
                point(px(cols.pv.x), bounds.origin.y),
                size(px(cols.pv.w), px(body_h)),
            ),
            hsla(0.0, 0.45, 0.35, 0.12),
        ));
    }
}

fn paint_semantic_lines(
    bounds: Bounds<Pixels>,
    body_h: f32,
    prepaint: &MpPrepaint,
    window: &mut Window,
) {
    let Some(top) = prepaint.profile.rows.last().map(|row| row.price) else {
        return;
    };
    let origin_y = f32::from(bounds.origin.y);
    let mut line = |price: Option<Price>, color: gpui::Hsla, thickness: f32| {
        let Some(y) = price.and_then(|price| {
            let bucket = Price(
                price
                    .0
                    .div_euclid(prepaint.profile.scaled_tick.0)
                    .checked_mul(prepaint.profile.scaled_tick.0)
                    .expect("MP marker bucket overflows i64"),
            );
            price_line_y(bucket.0, top.0, prepaint.profile.scaled_tick.0, origin_y)
        }) else {
            return;
        };
        if y >= origin_y && y < origin_y + body_h {
            window.paint_quad(fill(
                Bounds::new(
                    point(bounds.origin.x, px(y - thickness / 2.0)),
                    size(bounds.size.width, px(thickness)),
                ),
                color,
            ));
        }
    };
    line(prepaint.markers.vah, hsla(0.55, 0.20, 0.48, 0.55), 1.0);
    line(prepaint.markers.val, hsla(0.55, 0.20, 0.48, 0.55), 1.0);
    line(prepaint.markers.ib_high, hsla(0.12, 0.28, 0.52, 0.45), 1.0);
    line(prepaint.markers.ib_low, hsla(0.12, 0.28, 0.52, 0.45), 1.0);
    line(prepaint.markers.vpoc, hsla(0.08, 0.45, 0.62, 0.75), 1.5);
    line(
        prepaint.markers.current_price,
        hsla(0.0, 0.0, 0.86, 0.80),
        1.0,
    );
}

fn paint_dividers(bounds: Bounds<Pixels>, body_h: f32, cols: MpStrips, window: &mut Window) {
    for x in [cols.ep.x, cols.pv.x, cols.sv.x, cols.axis.x] {
        window.paint_quad(fill(
            Bounds::new(
                point(px(x), bounds.origin.y),
                size(px(1.0), bounds.size.height),
            ),
            rgb(DIVIDER),
        ));
    }
    window.paint_quad(fill(
        Bounds::new(
            point(bounds.origin.x, px(f32::from(bounds.origin.y) + body_h)),
            size(bounds.size.width, px(1.0)),
        ),
        rgb(DIVIDER),
    ));
}
