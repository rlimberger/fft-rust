//! WindoTrader/Dalton Market Profile as one custom GPUI `Element`.

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use fft_core::Price;
use fft_engine::RenderSnapshot;
use gpui::{
    App, Bounds, ContentMask, Element, ElementId, GlobalElementId, InspectorElementId, IntoElement,
    LayoutId, Pixels, Point, ShapedLine, Style, TextAlign, Window, fill, point, px, relative, size,
};

use crate::glyph_cache::GlyphCache;
use crate::mp_layout::{SessionLayout, mp_footer_h, session_layout};
use crate::mp_paint::{paint_dividers, paint_period_cursor, paint_rows, paint_semantic_lines};
use crate::mp_sessions::{
    block_intersects_viewport, is_prior, paint_prior_va_vpoc, paint_session_dividers,
    prepare_prior_session, prior_markers, same_price_ladder,
};
use crate::mp_view::{VisibleProfile, current_session, session_open_footer, visible_rows};
use crate::theme::Palette;

#[path = "mp_element_prepare.rs"]
mod mp_element_prepare;
use mp_element_prepare::prepare_current_rows;

#[derive(Clone)]
pub struct MarketProfile {
    snapshot: Arc<RenderSnapshot>,
    center: Option<Price>,
    tick_scale: u8,
    glyph_cache: Rc<RefCell<GlyphCache>>,
    palette: Rc<Palette>,
    scale: f32,
    pan_px: f32,
    zoom: f32,
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
            pan_px: 0.0,
            zoom: 1.0,
        }
    }

    /// Inject horizontal strip pan/zoom from `PaneState` without changing shell.rs.
    pub fn with_pan_zoom(mut self, pan_px: f32, zoom: f32) -> Self {
        self.pan_px = pan_px;
        self.zoom = zoom;
        self
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
    pub(crate) layout: SessionLayout,
    pub(crate) prior_markers: Vec<(usize, Markers)>,
    pub(crate) has_current_session: bool,
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
        let origin_x = f32::from(bounds.origin.x);
        let Some(current) = current_session(&self.snapshot.profile) else {
            return MpPrepaint {
                texts: Vec::new(),
                profile: VisibleProfile::default(),
                max_pv: 0,
                max_sv: 0,
                markers: Markers::default(),
                layout: session_layout(origin_x, width, 1, 0.0, 1.0, scale),
                prior_markers: Vec::new(),
                has_current_session: false,
            };
        };
        let sessions = &self.snapshot.profile.sessions;
        let layout = session_layout(
            origin_x,
            width,
            sessions.len(),
            self.pan_px,
            self.zoom,
            scale,
        );
        let max_rows = crate::mp_layout::max_rows(height, scale);
        let profile = visible_rows(
            current,
            self.snapshot.dom.tick_size,
            self.tick_scale,
            self.center,
            max_rows,
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
        let prior_count = sessions.len().saturating_sub(1);
        let mut texts =
            Vec::with_capacity(profile.rows.len() * (7 + prior_count * 2) + sessions.len());
        let mut cache = self.glyph_cache.borrow_mut();
        let body_h = (height - footer_h).max(0.0);

        // Prior sessions: letters-only CP + footer on the shared visible price ladder.
        // Cull before visible_rows / glyph shaping — offscreen priors cost zero.
        let shared_center = profile
            .rows
            .get(profile.rows.len() / 2)
            .map(|row| row.price);
        let mut prior_markers_out = Vec::with_capacity(prior_count);
        for block in layout.blocks.iter().filter(|b| is_prior(b)) {
            if !block_intersects_viewport(block, layout.strip_viewport) {
                continue;
            }
            let session = &sessions[block.session_index];
            let prior_profile = visible_rows(
                session,
                self.snapshot.dom.tick_size,
                self.tick_scale,
                shared_center,
                max_rows,
            );
            debug_assert!(same_price_ladder(&profile, &prior_profile));
            prepare_prior_session(
                session,
                &prior_profile,
                block,
                &layout,
                bounds,
                body_h,
                window,
                &mut cache,
                &mut texts,
                &self.palette,
                scale,
            );
            prior_markers_out.push((block.session_index, prior_markers(session)));
        }

        // Current session: full CP→EP→PV→SV + axis.
        let current_block = layout
            .blocks
            .iter()
            .find(|b| b.session_index + 1 == sessions.len())
            .expect("current session block present");
        prepare_current_rows(
            &profile,
            current_block,
            &layout,
            bounds,
            window,
            &mut cache,
            &mut texts,
            &self.palette,
            scale,
        );
        let footer = session_open_footer(current.trade_date);
        let line = cache.get_or_shape(window, footer, self.palette.text, px(11.0 * scale));
        let footer_x = current_block.x.max(layout.strip_viewport.x);
        let footer_right = (current_block.x + current_block.w)
            .min(layout.strip_viewport.x + layout.strip_viewport.w);
        let footer_w = (footer_right - footer_x).max(0.0);
        if footer_w > 0.0 {
            texts.push(PreparedText {
                line,
                origin: point(
                    px(footer_x + 6.0 * scale),
                    px(f32::from(bounds.origin.y) + height - footer_h + 4.0 * scale),
                ),
                align_width: px((footer_w - 12.0 * scale).max(0.0)),
                align: TextAlign::Left,
                line_height: px(footer_h - 4.0 * scale),
                clip: Bounds::new(
                    point(px(footer_x), px(f32::from(bounds.origin.y) + body_h)),
                    size(px(footer_w), px(footer_h)),
                ),
            });
        }
        drop(cache);
        MpPrepaint {
            texts,
            profile,
            max_pv,
            max_sv,
            markers: Markers {
                open: current.open,
                vpoc: current.vpoc,
                vah: current.vah,
                val: current.val,
                ib_low: current.ib_low,
                ib_high: current.ib_high,
                current_price: self.snapshot.dom.last_trade.map(|trade| trade.price),
                current_period: current.current_period,
                period_gap: current.period_gap,
            },
            layout,
            prior_markers: prior_markers_out,
            has_current_session: true,
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
        let height = f32::from(bounds.size.height);
        let origin_y = f32::from(bounds.origin.y);
        let body_h = (height - footer_h).max(0.0);
        let footer = Bounds::new(
            point(bounds.origin.x, px(origin_y + body_h)),
            size(bounds.size.width, px(footer_h)),
        );
        window.paint_quad(fill(footer, palette.footer_bg));

        let layout = &prepaint.layout;
        if prepaint.has_current_session && !prepaint.profile.rows.is_empty() {
            let current_block = layout
                .blocks
                .last()
                .expect("session layout has a current block");
            let mut cols = current_block.strips;
            cols.axis = layout.axis;

            paint_period_cursor(
                bounds,
                body_h,
                cols,
                layout.strip_viewport,
                prepaint.markers,
                palette,
                window,
            );
            paint_rows(bounds, cols, prepaint, palette, scale, window);
            paint_semantic_lines(bounds, body_h, prepaint, palette, scale, window);
            // Prior semantic hairlines are clipped to their own blocks.
            for block in layout.blocks.iter().filter(|b| is_prior(b)) {
                if let Some((_, markers)) = prepaint
                    .prior_markers
                    .iter()
                    .find(|(idx, _)| *idx == block.session_index)
                {
                    paint_prior_va_vpoc(
                        body_h,
                        block,
                        layout,
                        &prepaint.profile,
                        *markers,
                        palette,
                        scale,
                        origin_y,
                        window,
                    );
                }
            }
            paint_dividers(
                bounds,
                body_h,
                cols,
                layout.strip_viewport,
                palette,
                scale,
                window,
            );
            paint_session_dividers(bounds, layout, palette, scale, window);
        }

        for prepared in prepaint.texts.drain(..) {
            let paint_result = window.with_content_mask(
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
            );
            if paint_result.is_err() {
                note_mp_shaped_line_paint_failure(self.snapshot.generation);
            }
        }
    }
}

fn note_mp_shaped_line_paint_failure(generation: u64) {
    static FAIL_COUNT: AtomicU64 = AtomicU64::new(0);
    static WARNED_GEN: AtomicU64 = AtomicU64::new(u64::MAX);
    let n = FAIL_COUNT.fetch_add(1, Ordering::Relaxed) + 1;
    let prev = WARNED_GEN.load(Ordering::Relaxed);
    if prev != generation
        && WARNED_GEN
            .compare_exchange(prev, generation, Ordering::Relaxed, Ordering::Relaxed)
            .is_ok()
    {
        eprintln!(
            "fft: WARNING MP shaped-line paint failed (generation={generation}, count={n}); skipping run, frame continues"
        );
    }
}
