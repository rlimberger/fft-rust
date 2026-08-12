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

    /// Build the render source for a viewport. An explicit linked anchor stays on the
    /// requested center row even at the edges of bounded engine depth; missing buckets are
    /// zero-filled while overlapping source buckets are retained.
    pub fn aggregate_window(&self, dom: &DomRenderState, row_count: usize) -> AggregatedDom {
        let aggregated = self.aggregate(dom);
        let Some(anchor) = self.anchor else {
            return aggregated;
        };
        if row_count == 0 {
            return AggregatedDom {
                rows: Vec::new(),
                ..aggregated
            };
        }
        let anchor_bucket = if aggregated.scaled_tick_size.0 > 0 {
            bucket_price(anchor, aggregated.scaled_tick_size)
        } else {
            anchor
        };
        let normal_range = self.window_range(&aggregated, row_count);
        let normal_centers_anchor = normal_range.len() == row_count
            && aggregated
                .rows
                .get(normal_range.start + row_count / 2)
                .is_some_and(|row| row.price == anchor_bucket);
        if normal_centers_anchor {
            return aggregated;
        }
        synthesize_window(aggregated, anchor, row_count)
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

    /// Pin and move the center by rendered-row units. Free canvas: not clamped to
    /// present engine rows (paint synthesizes empty buckets outside depth).
    /// Positive deltas move toward higher prices.
    pub fn pan_rows(&mut self, dom: &AggregatedDom, delta: i64) -> bool {
        if delta == 0 || dom.scaled_tick_size.0 <= 0 {
            return false;
        }
        let base = match self.anchor {
            Some(anchor) => bucket_price(anchor, dom.scaled_tick_size),
            None => {
                let Some(center) = self.center_index(dom) else {
                    return false;
                };
                dom.rows[center].price
            }
        };
        let movement = i128::from(delta) * i128::from(dom.scaled_tick_size.0);
        let next = Price(
            i64::try_from(i128::from(base.0) + movement).expect("DOM pan center overflows i64"),
        );
        let anchor = Some(next);
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
    /// At least one bounded engine row contributed to this bucket.
    pub source_present: bool,
    pub bid_size: u64,
    pub ask_size: u64,
    /// Resting bid order count across the bucket.
    pub bid_orders: u32,
    /// Resting ask order count across the bucket.
    pub ask_orders: u32,
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
                source_present: true,
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
    add!(target.bid_orders, source.bid_orders, "bid_orders");
    add!(target.ask_orders, source.ask_orders, "ask_orders");
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

fn synthesize_window(mut dom: AggregatedDom, anchor: Price, row_count: usize) -> AggregatedDom {
    assert!(
        dom.scaled_tick_size.0 > 0,
        "linked DOM synthesis requires positive tick size"
    );
    let center = bucket_price(anchor, dom.scaled_tick_size);
    let below = i64::try_from(row_count / 2).expect("DOM row count fits i64");
    let start = center
        .0
        .checked_sub(
            below
                .checked_mul(dom.scaled_tick_size.0)
                .expect("linked DOM start offset overflows i64"),
        )
        .expect("linked DOM start price overflows i64");
    let source = std::mem::take(&mut dom.rows);
    let mut source_index = source.partition_point(|row| row.price.0 < start);
    let mut rows = Vec::with_capacity(row_count);
    for offset in 0..row_count {
        let price = Price(
            start
                .checked_add(
                    i64::try_from(offset)
                        .expect("DOM row offset fits i64")
                        .checked_mul(dom.scaled_tick_size.0)
                        .expect("linked DOM row offset overflows i64"),
                )
                .expect("linked DOM row price overflows i64"),
        );
        if source
            .get(source_index)
            .is_some_and(|row| row.price == price)
        {
            rows.push(source[source_index]);
            source_index += 1;
        } else {
            rows.push(DomViewRow {
                price,
                ..DomViewRow::default()
            });
        }
    }
    dom.rows = rows;
    dom
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
#[path = "dom_view_tests.rs"]
mod tests;
