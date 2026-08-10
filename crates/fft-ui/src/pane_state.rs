//! Pure linked-pane, hover-scale, and latest-wins splitter state.

use fft_core::Price;
use fft_engine::DomRenderState;

pub const SPLITTER_WIDTH: f32 = 6.0;
const MIN_PANE_WIDTH: f32 = 180.0;

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
    pub hovered: Option<Pane>,
    pub splitter: SplitterState,
}

impl Default for PaneState {
    fn default() -> Self {
        Self {
            center: None,
            mp_scale: 1,
            dom_scale: 1,
            hovered: None,
            splitter: SplitterState::default(),
        }
    }
}

impl PaneState {
    pub fn effective_center(&self, dom: &DomRenderState) -> Option<Price> {
        self.center.or_else(|| raw_follow_center(dom))
    }

    pub fn set_hovered(&mut self, pane: Pane, hovered: bool) {
        if hovered {
            self.hovered = Some(pane);
        } else if self.hovered == Some(pane) {
            self.hovered = None;
        }
    }

    /// Provisional M4 binding: 1/2/4 affects only the hovered pane.
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

    pub fn recenter(&mut self) -> bool {
        self.center.take().is_some()
    }

    pub fn clamp_center_to_dom(&mut self, dom: &DomRenderState) {
        let (Some(center), Some(first), Some(last)) =
            (self.center, dom.rows.first(), dom.rows.last())
        else {
            return;
        };
        self.center = Some(Price(center.0.clamp(first.price.0, last.price.0)));
    }
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

    pub fn is_dragging(&self) -> bool {
        self.dragging
    }

    /// Consume only the most recent pointer coordinate queued before this frame.
    pub fn consume(&mut self, width: f32) -> bool {
        let Some(x) = self.pending_x.take() else {
            return false;
        };
        assert!(
            width.is_finite() && width > 0.0,
            "split width must be positive"
        );
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
mod tests {
    use fft_engine::DomPriceRow;

    use super::*;

    #[test]
    fn raw_follow_center_is_scale_independent() {
        let dom = DomRenderState {
            best_bid: Some(Price(100)),
            best_ask: Some(Price(110)),
            ..Default::default()
        };
        assert_eq!(raw_follow_center(&dom), Some(Price(105)));
    }

    #[test]
    fn scale_key_changes_only_hovered_pane() {
        let mut state = PaneState::default();
        assert!(!state.set_hovered_scale(4));
        state.set_hovered(Pane::MarketProfile, true);
        assert!(state.set_hovered_scale(4));
        assert_eq!((state.mp_scale, state.dom_scale), (4, 1));
        state.set_hovered(Pane::Dom, true);
        assert!(state.set_hovered_scale(2));
        assert_eq!((state.mp_scale, state.dom_scale), (4, 2));
    }

    #[test]
    fn splitter_is_latest_wins_and_clamped() {
        let mut split = SplitterState::default();
        split.begin(250.0);
        split.queue(300.0);
        split.queue(400.0);
        assert!(split.consume(1_000.0));
        assert!((split.ratio() - 400.0 / 994.0).abs() < 1e-6);
        split.queue(-50.0);
        assert!(split.consume(1_000.0));
        assert!((split.ratio() - 180.0 / 994.0).abs() < 1e-6);
    }

    #[test]
    fn splitter_keeps_final_pending_position_after_mouse_up() {
        let mut split = SplitterState::default();
        split.begin(300.0);
        split.queue(420.0);
        split.end();
        assert!(split.consume(1_000.0));
        assert!((split.ratio() - 420.0 / 994.0).abs() < 1e-6);
    }

    #[test]
    fn shared_center_clamps_to_bounded_dom_window() {
        let mut state = PaneState {
            center: Some(Price(500)),
            ..Default::default()
        };
        let dom = DomRenderState {
            rows: vec![
                DomPriceRow {
                    price: Price(100),
                    ..Default::default()
                },
                DomPriceRow {
                    price: Price(120),
                    ..Default::default()
                },
            ],
            ..Default::default()
        };
        state.clamp_center_to_dom(&dom);
        assert_eq!(state.center, Some(Price(120)));
    }
}
