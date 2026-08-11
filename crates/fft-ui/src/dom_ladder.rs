//! Daytradr DOM ladder as one custom GPUI `Element` — quads + shaped text, no div-per-cell.

use std::cell::RefCell;
use std::ops::Range;
use std::rc::Rc;
use std::sync::Arc;

use fft_engine::RenderSnapshot;
use gpui::{
    App, Bounds, Element, ElementId, GlobalElementId, InspectorElementId, IntoElement, LayoutId,
    Pixels, Point, ShapedLine, Style, TextAlign, Window, relative,
};

use crate::dom_ladder_paint;
use crate::dom_ladder_prepare;
use crate::dom_view::{AggregatedDom, DomView};
use crate::glyph_cache::GlyphCache;
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

pub(crate) struct PreparedText {
    pub(crate) line: ShapedLine,
    pub(crate) origin: Point<Pixels>,
    pub(crate) align_width: Pixels,
    pub(crate) align: TextAlign,
}

/// Prepaint cache for shaped glyph runs (associated type on [`Element`]; must be public).
pub struct Prepaint {
    pub(crate) texts: Vec<PreparedText>,
    pub(crate) hover_texts: Vec<PreparedText>,
    pub(crate) hover_box: Option<Bounds<Pixels>>,
    pub(crate) hovered_from_top: Option<usize>,
    pub(crate) dom: AggregatedDom,
    pub(crate) row_range: Range<usize>,
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
        dom_ladder_prepare::prepare(
            &self.snapshot,
            &self.view,
            &self.glyph_cache,
            &self.palette,
            self.scale,
            bounds,
            window,
        )
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
        dom_ladder_paint::paint(prepaint, &self.palette, self.scale, bounds, window, cx)
    }
}
