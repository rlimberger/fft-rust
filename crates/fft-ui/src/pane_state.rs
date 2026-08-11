//! Pure linked-pane, hover-scale, and latest-wins splitter state.

use fft_core::Price;
use fft_engine::{DomRenderState, ProfileRenderState};

use crate::mp_layout::{clamp_pan, current_session_rest_pan};
use crate::prefs::Prefs;

pub const SPLITTER_WIDTH: f32 = 6.0;
const MIN_PANE_WIDTH: f32 = 180.0;
const MP_REST_EPSILON_PX: f32 = 0.01;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Pane {
    MarketProfile,
    Dom,
}

#[derive(Debug)]
pub struct PaneState {
    /// `None` follows the raw inside market; `Some` is shared by both panes.
    pub center: Option<Price>,
    pub mp_scale: u8,
    pub dom_scale: u8,
    /// Horizontal strip pan in px (content-space; positive reveals older sessions).
    pub mp_pan_px: f32,
    /// Horizontal strip zoom factor (0.5..=3.0); 1.0 matches today's widths.
    pub mp_zoom: f32,
    /// Unclamped user-selected pan, retained while narrower geometry clamps the display.
    mp_desired_pan_px: f32,
    /// Logical current-session rest survives geometry changes without persisting raw pan.
    mp_at_rest: bool,
    pub hovered: Option<Pane>,
    dom_visible: bool,
    pub splitter: SplitterState,
}

impl Default for PaneState {
    fn default() -> Self {
        Self {
            center: None,
            mp_scale: 1,
            dom_scale: 1,
            mp_pan_px: 0.0,
            mp_zoom: 1.0,
            mp_desired_pan_px: 0.0,
            mp_at_rest: true,
            hovered: None,
            dom_visible: false,
            splitter: SplitterState::default(),
        }
    }
}

/// Fields of [`PaneState`] that survive across runs (prefs v1).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PanePrefsSnapshot {
    pub mp_scale: u8,
    pub dom_scale: u8,
    pub splitter_ratio: f32,
    pub mp_zoom: f32,
}

impl PaneState {
    /// Construct pane state from loaded prefs (clamped values already applied).
    pub fn from_prefs(prefs: &Prefs) -> Self {
        let mut state = Self::default();
        state.apply_prefs_snapshot(&PanePrefsSnapshot {
            mp_scale: prefs.mp_scale,
            dom_scale: prefs.dom_scale,
            splitter_ratio: prefs.splitter_ratio,
            mp_zoom: prefs.mp_zoom,
        });
        state
    }

    /// Snapshot of persisted pane fields for quit-time write.
    pub fn prefs_snapshot(&self) -> PanePrefsSnapshot {
        PanePrefsSnapshot {
            mp_scale: self.mp_scale,
            dom_scale: self.dom_scale,
            splitter_ratio: self.splitter.ratio(),
            mp_zoom: self.mp_zoom,
        }
    }

    /// Apply a prefs snapshot (scales must be 1/2/4; ratio/zoom trusted as clamped).
    pub fn apply_prefs_snapshot(&mut self, snap: &PanePrefsSnapshot) {
        validate_scale(snap.mp_scale);
        validate_scale(snap.dom_scale);
        self.mp_scale = snap.mp_scale;
        self.dom_scale = snap.dom_scale;
        self.mp_zoom = snap.mp_zoom;
        self.splitter.set_ratio(snap.splitter_ratio);
    }

    pub fn effective_center(&self, dom: &DomRenderState) -> Option<Price> {
        self.center.or_else(|| raw_follow_center(dom))
    }

    pub fn navigation_center(
        &self,
        profile: &ProfileRenderState,
        dom: &DomRenderState,
    ) -> Option<Price> {
        clamp_center(self.effective_center(dom), navigation_range(profile, dom))
    }

    pub fn dom_visible(&self) -> bool {
        self.dom_visible
    }

    /// Toggle the launch-local DOM surface without changing navigation or split ratio.
    pub fn toggle_dom(&mut self) -> bool {
        self.dom_visible = !self.dom_visible;
        if !self.dom_visible {
            if self.hovered == Some(Pane::Dom) {
                self.hovered = None;
            }
            self.splitter.cancel();
        }
        self.dom_visible
    }

    /// Width used by MP layout and pointer math for the current surface composition.
    ///
    /// Zero/non-finite viewport width is normal mid-resize; return a degenerate
    /// but valid 1.0 so pointer math and layout never abort the UI thread.
    pub fn effective_mp_width(&self, viewport_width: f32) -> f32 {
        if !(viewport_width.is_finite() && viewport_width > 0.0) {
            return 1.0;
        }
        if self.dom_visible {
            ((viewport_width - SPLITTER_WIDTH) * self.splitter.ratio()).max(1.0)
        } else {
            viewport_width
        }
    }

    pub fn mp_at_rest(&self) -> bool {
        self.mp_at_rest
    }

    /// Reconcile horizontal navigation with current geometry before constructing the MP.
    pub fn reconcile_mp_pan(&mut self, content_width: f32, viewport_width: f32) -> bool {
        let next = if self.mp_at_rest {
            let rest = current_session_rest_pan(content_width, viewport_width);
            self.mp_desired_pan_px = rest;
            rest
        } else {
            clamp_pan(self.mp_desired_pan_px, content_width, viewport_width)
        };
        if (next - self.mp_pan_px).abs() <= MP_REST_EPSILON_PX {
            return false;
        }
        self.mp_pan_px = next;
        true
    }

    /// Apply a horizontal drag to the logical pan, then clamp only the displayed pan.
    pub fn navigate_mp_pan(
        &mut self,
        delta_px: f32,
        content_width: f32,
        viewport_width: f32,
    ) -> bool {
        assert!(delta_px.is_finite(), "MP pan delta must be finite");
        let rest = current_session_rest_pan(content_width, viewport_width);
        let base = if self.mp_at_rest {
            rest
        } else {
            self.mp_desired_pan_px
        };
        self.mp_desired_pan_px = (base + delta_px).max(0.0);
        let next = clamp_pan(self.mp_desired_pan_px, content_width, viewport_width);
        let changed = (next - self.mp_pan_px).abs() > MP_REST_EPSILON_PX;
        self.mp_pan_px = next;
        self.mp_at_rest = (self.mp_desired_pan_px - rest).abs() <= MP_REST_EPSILON_PX;
        if self.mp_at_rest {
            self.mp_desired_pan_px = rest;
        }
        changed
    }

    /// Wheel zoom is navigation; a result at the new rest bound re-arms logical rest.
    pub fn navigate_mp_zoom(
        &mut self,
        zoom: f32,
        pan_px: f32,
        content_width: f32,
        viewport_width: f32,
    ) -> bool {
        assert!(zoom.is_finite() && zoom > 0.0, "MP zoom must be finite > 0");
        assert!(pan_px.is_finite(), "MP zoom pan must be finite");
        let next_pan = clamp_pan(pan_px, content_width, viewport_width);
        let rest = current_session_rest_pan(content_width, viewport_width);
        let changed = (zoom - self.mp_zoom).abs() > f32::EPSILON
            || (next_pan - self.mp_pan_px).abs() > MP_REST_EPSILON_PX;
        self.mp_zoom = zoom;
        self.mp_pan_px = next_pan;
        self.mp_at_rest = (next_pan - rest).abs() <= MP_REST_EPSILON_PX;
        self.mp_desired_pan_px = if self.mp_at_rest {
            rest
        } else {
            pan_px.max(0.0)
        };
        changed
    }

    pub fn set_hovered(&mut self, pane: Pane, hovered: bool) {
        if hovered {
            self.hovered = Some(pane);
        } else if self.hovered == Some(pane) {
            self.hovered = None;
        }
    }

    /// PRD §5 hover routing: 1/2/4 affects only the hovered pane.
    pub fn set_hovered_scale(&mut self, scale: u8) -> bool {
        validate_scale(scale);
        let target = match self.hovered {
            Some(Pane::MarketProfile) => &mut self.mp_scale,
            Some(Pane::Dom) => &mut self.dom_scale,
            None => return false,
        };
        if *target == scale {
            return false;
        }
        *target = scale;
        true
    }

    /// PRD §5 `t`: copy the hovered pane's tick scale onto the other pane.
    pub fn sync_scale_from_hovered(&mut self) -> bool {
        let (source, target) = match self.hovered {
            Some(Pane::MarketProfile) => (self.mp_scale, &mut self.dom_scale),
            Some(Pane::Dom) => (self.dom_scale, &mut self.mp_scale),
            None => return false,
        };
        if *target == source {
            return false;
        }
        *target = source;
        true
    }

    pub fn recenter(&mut self) -> bool {
        self.center.take().is_some()
    }

    pub fn clamp_center(&mut self, profile: &ProfileRenderState, dom: &DomRenderState) {
        self.center = clamp_center(self.center, navigation_range(profile, dom));
    }
}

pub fn navigation_range(
    profile: &ProfileRenderState,
    dom: &DomRenderState,
) -> Option<(Price, Price)> {
    let profile_range = crate::mp_view::current_session(profile)
        .and_then(|session| Some((session.rows.first()?.price, session.rows.last()?.price)));
    let dom_range = match (dom.rows.first(), dom.rows.last()) {
        (Some(first), Some(last)) => Some((first.price, last.price)),
        _ => None,
    };
    match (profile_range, dom_range) {
        (Some((profile_first, profile_last)), Some((dom_first, dom_last))) => Some((
            Price(profile_first.0.min(dom_first.0)),
            Price(profile_last.0.max(dom_last.0)),
        )),
        (Some(range), None) | (None, Some(range)) => Some(range),
        (None, None) => None,
    }
}

pub fn clamp_center(center: Option<Price>, range: Option<(Price, Price)>) -> Option<Price> {
    let (Some(center), Some((first, last))) = (center, range) else {
        return center;
    };
    assert!(first <= last, "navigation range must be ascending");
    Some(Price(center.0.clamp(first.0, last.0)))
}

pub fn pan_center(center: Price, tick: Price, scale: u8, delta: i64) -> Price {
    validate_scale(scale);
    assert!(tick.0 > 0, "pane tick size must be positive");
    let movement = i128::from(delta) * i128::from(tick.0) * i128::from(scale);
    Price(i64::try_from(i128::from(center.0) + movement).expect("pane pan center overflows i64"))
}

pub fn raw_follow_center(dom: &DomRenderState) -> Option<Price> {
    match (dom.best_bid, dom.best_ask, dom.last_trade) {
        (Some(bid), Some(ask), _) => {
            let midpoint = (i128::from(bid.0) + i128::from(ask.0)) / 2;
            Some(Price(
                i64::try_from(midpoint).expect("linked inside midpoint overflows i64"),
            ))
        }
        (Some(bid), None, _) => Some(bid),
        (None, Some(ask), _) => Some(ask),
        (None, None, Some(last)) => Some(last.price),
        (None, None, None) => dom.rows.get(dom.rows.len() / 2).map(|row| row.price),
    }
}

fn validate_scale(scale: u8) {
    assert!(matches!(scale, 1 | 2 | 4), "pane scale must be 1, 2, or 4");
}

#[derive(Debug)]
pub struct SplitterState {
    ratio: f32,
    dragging: bool,
    pending_x: Option<f32>,
}

impl Default for SplitterState {
    fn default() -> Self {
        Self {
            ratio: 0.48,
            dragging: false,
            pending_x: None,
        }
    }
}

impl SplitterState {
    pub fn ratio(&self) -> f32 {
        self.ratio
    }

    /// Set the split ratio directly (prefs restore; not for drag).
    pub fn set_ratio(&mut self, ratio: f32) {
        assert!(
            ratio.is_finite() && (0.0..=1.0).contains(&ratio),
            "splitter ratio must be finite in [0, 1]"
        );
        self.ratio = ratio;
    }

    pub fn begin(&mut self, x: f32) {
        self.dragging = true;
        self.queue(x);
    }

    pub fn queue(&mut self, x: f32) {
        assert!(x.is_finite(), "splitter coordinate must be finite");
        if self.dragging {
            self.pending_x = Some(x);
        }
    }

    pub fn end(&mut self) {
        self.dragging = false;
    }

    pub fn cancel(&mut self) {
        self.dragging = false;
        self.pending_x = None;
    }

    pub fn is_dragging(&self) -> bool {
        self.dragging
    }

    /// Consume only the most recent pointer coordinate queued before this frame.
    pub fn consume(&mut self, width: f32) -> bool {
        // Zero/non-finite widths happen mid-resize; keep the pending sample
        // (latest-wins) until a valid width can consume it.
        if !(width.is_finite() && width > 0.0) {
            return false;
        }
        let Some(x) = self.pending_x.take() else {
            return false;
        };
        let usable = (width - SPLITTER_WIDTH).max(1.0);
        let min = MIN_PANE_WIDTH.min(usable / 2.0);
        let ratio = x.clamp(min, usable - min) / usable;
        if (ratio - self.ratio).abs() <= f32::EPSILON {
            return false;
        }
        self.ratio = ratio;
        true
    }
}

#[cfg(test)]
#[path = "pane_state_tests.rs"]
mod tests;
