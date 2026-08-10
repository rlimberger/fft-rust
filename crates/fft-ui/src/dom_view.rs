//! Pure DOM row aggregation and viewport state.
//!
//! Prices remain in the shared 1e-9 fixed-point [`Price`] type through the render boundary.

use std::ops::Range;

use fft_core::Price;
use fft_engine::{DomPriceRow, DomRenderState};

/// Interaction state for one DOM ladder.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DomView {
    /// `None` follows the inside market; `Some` pins the center price.
    pub anchor: Option<Price>,
    /// Number of instrument ticks per rendered row. Valid values are 1, 2, and 4.
    pub tick_scale: u8,
}

impl Default for DomView {
    fn default() -> Self {
        Self {
            anchor: None,
            tick_scale: 1,
        }
    }
}

impl DomView {
    /// Construct a view and fail loudly for an unsupported scale.
    pub fn new(tick_scale: u8) -> Self {
        validate_scale(tick_scale);
        Self {
            tick_scale,
            ..Self::default()
        }
    }

    /// Change scale while preserving the raw anchor price.
    pub fn set_tick_scale(&mut self, tick_scale: u8) -> bool {
        validate_scale(tick_scale);
        if self.tick_scale == tick_scale {
            return false;
        }
        self.tick_scale = tick_scale;
        true
    }

    /// Return to automatic inside-market following.
    pub fn recenter(&mut self) -> bool {
        self.anchor.take().is_some()
    }

    /// Aggregate the engine's ascending instrument-tick rows for this view.
    pub fn aggregate(&self, dom: &DomRenderState) -> AggregatedDom {
        aggregate_rows(dom, self.tick_scale)
    }

    /// Index range for a centered window of at most `row_count` rows.
    pub fn window_range(&self, dom: &AggregatedDom, row_count: usize) -> Range<usize> {
        if dom.rows.is_empty() || row_count == 0 {
            return 0..0;
        }

        let count = row_count.min(dom.rows.len());
        let center = self
            .center_index(dom)
            .expect("non-empty rows have a center");
        let mut start = center.saturating_sub(count / 2);
        start = start.min(dom.rows.len() - count);
        start..start + count
    }

    /// Pin and move the center by rendered-row units, clamped to present rows.
    /// Positive deltas move toward higher prices.
    pub fn pan_rows(&mut self, dom: &AggregatedDom, delta: i64) -> bool {
        let Some(center) = self.center_index(dom) else {
            return false;
        };
        let last = dom.rows.len() - 1;
        let target = ((center as i128) + i128::from(delta)).clamp(0, last as i128) as usize;
        let anchor = Some(dom.rows[target].price);
        if self.anchor == anchor {
            return false;
        }
        self.anchor = anchor;
        true
    }

    fn center_index(&self, dom: &AggregatedDom) -> Option<usize> {
        if dom.rows.is_empty() {
            return None;
        }
        Some(match self.anchor {
            Some(anchor) => nearest_row(&dom.rows, bucket_price(anchor, dom.scaled_tick_size)),
            None => ceiling_row(&dom.rows, dom.follow_price()),
        })
    }
}

/// One scaled render row. All quantities are sums over its instrument-tick rows.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct DomViewRow {
    /// Lower boundary of this scaled-tick bucket.
    pub price: Price,
    pub bid_size: u64,
    pub ask_size: u64,
    pub session_volume: u64,
    pub cb: u64,
    pub ca: u64,
    pub bid_added_5s: u32,
    pub bid_cancelled_5s: u32,
    pub ask_added_5s: u32,
    pub ask_cancelled_5s: u32,
    pub refresh_bid_count: u32,
    pub refresh_ask_count: u32,
    pub refresh_bid_hidden: u64,
    pub refresh_ask_hidden: u64,
}

/// DOM state after tick-scale aggregation.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AggregatedDom {
    /// Instrument tick in raw fixed-point units.
    pub tick_size: Price,
    /// Render-row tick (`tick_size * scale`).
    pub scaled_tick_size: Price,
    /// Best bid mapped to its containing render bucket.
    pub best_bid: Option<Price>,
    /// Best ask mapped to its containing render bucket.
    pub best_ask: Option<Price>,
    /// Last trade mapped to its containing render bucket.
    pub last_trade: Option<Price>,
    /// Rows in ascending bucket-price order.
    pub rows: Vec<DomViewRow>,
}

impl AggregatedDom {
    fn follow_price(&self) -> Price {
        match (self.best_bid, self.best_ask) {
            (Some(bid), Some(ask)) => {
                let midpoint = (i128::from(bid.0) + i128::from(ask.0)) / 2;
                Price(i64::try_from(midpoint).expect("inside midpoint overflows i64"))
            }
            (Some(bid), None) => bid,
            (None, Some(ask)) => ask,
            (None, None) => self
                .last_trade
                .unwrap_or_else(|| self.rows[self.rows.len() / 2].price),
        }
    }
}

/// Aggregate ascending engine rows into scaled-tick buckets.
pub fn aggregate_rows(dom: &DomRenderState, tick_scale: u8) -> AggregatedDom {
    validate_scale(tick_scale);
    if dom == &DomRenderState::default() {
        return AggregatedDom::default();
    }
    assert!(dom.tick_size.0 > 0, "DOM tick size must be positive");
    let scaled_tick_size = Price(
        dom.tick_size
            .0
            .checked_mul(i64::from(tick_scale))
            .expect("scaled DOM tick size overflows i64"),
    );

    let mut rows: Vec<DomViewRow> = Vec::with_capacity(dom.rows.len());
    let mut previous = None;
    for source in &dom.rows {
        if let Some(prior) = previous {
            assert!(source.price.0 >= prior, "DOM rows must be ascending");
        }
        previous = Some(source.price.0);
        let price = bucket_price(source.price, scaled_tick_size);
        if rows.last().is_none_or(|row| row.price != price) {
            rows.push(DomViewRow {
                price,
                ..DomViewRow::default()
            });
        }
        merge_row(rows.last_mut().expect("bucket was inserted"), source);
    }

    let map_inside = |price: Price| bucket_price(price, scaled_tick_size);
    AggregatedDom {
        tick_size: dom.tick_size,
        scaled_tick_size,
        best_bid: dom.best_bid.map(map_inside),
        best_ask: dom.best_ask.map(map_inside),
        last_trade: dom.last_trade.map(|trade| map_inside(trade.price)),
        rows,
    }
}

fn validate_scale(tick_scale: u8) {
    assert!(
        matches!(tick_scale, 1 | 2 | 4),
        "DOM tick scale must be 1, 2, or 4"
    );
}

fn bucket_price(price: Price, scaled_tick: Price) -> Price {
    Price(
        price
            .0
            .div_euclid(scaled_tick.0)
            .checked_mul(scaled_tick.0)
            .expect("DOM bucket price overflows i64"),
    )
}

fn merge_row(target: &mut DomViewRow, source: &DomPriceRow) {
    macro_rules! add {
        ($target:expr, $source:expr, $name:literal) => {
            $target = $target
                .checked_add($source)
                .unwrap_or_else(|| panic!(concat!("DOM ", $name, " aggregation overflow")))
        };
    }
    add!(target.bid_size, source.bid_size, "bid_size");
    add!(target.ask_size, source.ask_size, "ask_size");
    add!(
        target.session_volume,
        source.session_volume,
        "session_volume"
    );
    add!(target.cb, source.cb, "cb");
    add!(target.ca, source.ca, "ca");
    add!(target.bid_added_5s, source.bid_added_5s, "bid_added_5s");
    add!(
        target.bid_cancelled_5s,
        source.bid_cancelled_5s,
        "bid_cancelled_5s"
    );
    add!(target.ask_added_5s, source.ask_added_5s, "ask_added_5s");
    add!(
        target.ask_cancelled_5s,
        source.ask_cancelled_5s,
        "ask_cancelled_5s"
    );
    add!(
        target.refresh_bid_count,
        source.refresh_agg.bid_count,
        "refresh bid_count"
    );
    add!(
        target.refresh_ask_count,
        source.refresh_agg.ask_count,
        "refresh ask_count"
    );
    add!(
        target.refresh_bid_hidden,
        source.refresh_agg.bid_hidden,
        "refresh bid_hidden"
    );
    add!(
        target.refresh_ask_hidden,
        source.refresh_agg.ask_hidden,
        "refresh ask_hidden"
    );
}

fn nearest_row(rows: &[DomViewRow], target: Price) -> usize {
    match rows.binary_search_by_key(&target, |row| row.price) {
        Ok(index) => index,
        Err(0) => 0,
        Err(index) if index == rows.len() => rows.len() - 1,
        Err(index) => {
            let below = i128::from(target.0) - i128::from(rows[index - 1].price.0);
            let above = i128::from(rows[index].price.0) - i128::from(target.0);
            if below <= above { index - 1 } else { index }
        }
    }
}

fn ceiling_row(rows: &[DomViewRow], target: Price) -> usize {
    match rows.binary_search_by_key(&target, |row| row.price) {
        Ok(index) => index,
        Err(index) => index.min(rows.len() - 1),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(price: i64, value: u64) -> DomPriceRow {
        let mut row = DomPriceRow::default();
        row.price.0 = price;
        row.bid_size = value;
        row.ask_size = value;
        row.session_volume = value;
        row.cb = value;
        row.ca = value;
        row.bid_added_5s = u32::try_from(value).unwrap();
        row.bid_cancelled_5s = u32::try_from(value).unwrap();
        row.ask_added_5s = u32::try_from(value).unwrap();
        row.ask_cancelled_5s = u32::try_from(value).unwrap();
        row.refresh_agg.bid_count = u32::try_from(value).unwrap();
        row.refresh_agg.ask_count = u32::try_from(value).unwrap();
        row.refresh_agg.bid_hidden = value;
        row.refresh_agg.ask_hidden = value;
        row
    }

    fn dom(tick: i64, prices: &[i64]) -> DomRenderState {
        let mut dom = DomRenderState::default();
        dom.tick_size.0 = tick;
        dom.rows = prices.iter().map(|&price| row(price, 1)).collect();
        dom
    }

    #[test]
    fn empty_book_is_a_no_op() {
        let aggregated = DomView::default().aggregate(&dom(5, &[]));
        assert!(aggregated.rows.is_empty());
        let mut view = DomView::default();
        assert!(!view.pan_rows(&aggregated, 10));
        assert_eq!(view.anchor, None);
        assert_eq!(view.window_range(&aggregated, 7), 0..0);
    }

    #[test]
    fn default_engine_state_aggregates_for_instant_shell() {
        assert_eq!(
            aggregate_rows(&DomRenderState::default(), 1),
            AggregatedDom::default()
        );
    }

    #[test]
    fn aggregates_exact_boundaries_and_all_metadata() {
        let aggregated = aggregate_rows(&dom(5, &[0, 5, 10, 15]), 2);
        assert_eq!(aggregated.rows.len(), 2);
        assert_eq!(aggregated.rows[0].price, Price(0));
        assert_eq!(aggregated.rows[1].price, Price(10));
        let first = aggregated.rows[0];
        assert_eq!(
            (first.bid_size, first.ask_size, first.session_volume),
            (2, 2, 2)
        );
        assert_eq!((first.cb, first.ca), (2, 2));
        assert_eq!((first.bid_added_5s, first.ask_cancelled_5s), (2, 2));
        assert_eq!((first.refresh_bid_count, first.refresh_ask_count), (2, 2));
        assert_eq!((first.refresh_bid_hidden, first.refresh_ask_hidden), (2, 2));
    }

    #[test]
    fn scales_one_two_and_four() {
        let source = dom(5, &[0, 5, 10, 15]);
        assert_eq!(aggregate_rows(&source, 1).rows.len(), 4);
        assert_eq!(aggregate_rows(&source, 2).rows.len(), 2);
        assert_eq!(aggregate_rows(&source, 4).rows.len(), 1);
    }

    #[test]
    fn odd_source_count_keeps_partial_edge_bucket() {
        let aggregated = aggregate_rows(&dom(5, &[0, 5, 10, 15, 20]), 2);
        assert_eq!(
            aggregated
                .rows
                .iter()
                .map(|row| (row.price, row.bid_size))
                .collect::<Vec<_>>(),
            vec![(Price(0), 2), (Price(10), 2), (Price(20), 1)]
        );
    }

    #[test]
    fn inside_prices_map_to_containing_bucket() {
        let mut source = dom(5, &[0, 5, 10, 15]);
        source.best_bid = Some(source.rows[1].price);
        source.best_ask = Some(source.rows[2].price);
        let aggregated = aggregate_rows(&source, 4);
        assert_eq!(aggregated.best_bid, Some(Price(0)));
        assert_eq!(aggregated.best_ask, Some(Price(0)));
    }

    #[test]
    fn follow_mode_preserves_inside_midpoint_ceiling() {
        let mut source = dom(10, &[0, 10]);
        source.best_bid = Some(Price(0));
        source.best_ask = Some(Price(10));
        let aggregated = aggregate_rows(&source, 1);
        assert_eq!(DomView::default().window_range(&aggregated, 1), 1..2);
    }

    #[test]
    fn odd_window_centers_and_shifts_at_edges() {
        let aggregated = aggregate_rows(&dom(1, &[0, 1, 2, 3, 4]), 1);
        let mut view = DomView {
            anchor: Some(Price(2)),
            ..DomView::default()
        };
        assert_eq!(view.window_range(&aggregated, 3), 1..4);
        view.anchor = Some(Price(0));
        assert_eq!(view.window_range(&aggregated, 3), 0..3);
        view.anchor = Some(Price(4));
        assert_eq!(view.window_range(&aggregated, 3), 2..5);
    }

    #[test]
    fn panning_clamps_to_present_aggregated_rows() {
        let aggregated = aggregate_rows(&dom(1, &[10, 11, 12]), 1);
        let mut view = DomView::default();
        assert!(view.pan_rows(&aggregated, i64::MAX));
        assert_eq!(view.anchor, Some(Price(12)));
        assert!(!view.pan_rows(&aggregated, i64::MAX));
        assert!(view.pan_rows(&aggregated, i64::MIN));
        assert_eq!(view.anchor, Some(Price(10)));
    }

    #[test]
    #[should_panic(expected = "DOM tick scale must be 1, 2, or 4")]
    fn invalid_scale_panics_even_for_empty_book() {
        aggregate_rows(&dom(1, &[]), 3);
    }

    #[test]
    #[should_panic(expected = "DOM tick size must be positive")]
    fn invalid_tick_panics_for_nonempty_book() {
        aggregate_rows(&dom(0, &[0]), 1);
    }

    #[test]
    #[should_panic(expected = "DOM tick size must be positive")]
    fn invalid_tick_panics_for_inconsistent_empty_state() {
        let source = DomRenderState {
            best_bid: Some(Price(1)),
            ..DomRenderState::default()
        };
        aggregate_rows(&source, 1);
    }

    #[test]
    fn mutation_helpers_report_changes() {
        let mut view = DomView::default();
        assert!(!view.set_tick_scale(1));
        assert!(view.set_tick_scale(2));
        assert!(!view.recenter());
        view.anchor = Some(Price(10));
        assert!(view.recenter());
    }

    #[test]
    #[should_panic(expected = "DOM bid_size aggregation overflow")]
    fn aggregation_overflow_is_loud() {
        let mut source = dom(1, &[0, 1]);
        source.rows[0].bid_size = u64::MAX;
        aggregate_rows(&source, 2);
    }
}
