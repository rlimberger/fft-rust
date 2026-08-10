use arc_swap::ArcSwap;
use fft_book::{Book, LastTrade};
use fft_core::{Price, Side};
use fft_profile::{CvdCandle, MultiProfile};
use std::mem::size_of;
use std::sync::Arc;

/// Per-side native-refresh totals rendered at one price.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct PriceRefreshRender {
    /// Bid-side observed reload count.
    pub bid_count: u32,
    /// Ask-side observed reload count.
    pub ask_count: u32,
    /// Bid-side observed hidden volume.
    pub bid_hidden: u64,
    /// Ask-side observed hidden volume.
    pub ask_hidden: u64,
}

/// One visible DOM ladder row.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct DomPriceRow {
    /// Exact 1e-9 fixed-point price.
    pub price: Price,
    /// Resting bid contracts.
    pub bid_size: u64,
    /// Resting ask contracts.
    pub ask_size: u64,
    /// Current-session traded volume.
    pub session_volume: u64,
    /// Current traded-at-bid counter.
    pub cb: u64,
    /// Current traded-at-ask counter.
    pub ca: u64,
    /// Bid additions in the five-second flow window.
    pub bid_added_5s: u32,
    /// Bid pulls in the five-second flow window.
    pub bid_cancelled_5s: u32,
    /// Ask additions in the five-second flow window.
    pub ask_added_5s: u32,
    /// Ask pulls in the five-second flow window.
    pub ask_cancelled_5s: u32,
    /// Native-refresh badges.
    pub refresh_agg: PriceRefreshRender,
}

/// Bounded visible ladder state.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DomRenderState {
    /// Instrument tick size.
    pub tick_size: Price,
    /// Best resting bid.
    pub best_bid: Option<Price>,
    /// Best resting ask.
    pub best_ask: Option<Price>,
    /// Most recent trade.
    pub last_trade: Option<LastTrade>,
    /// Contiguous visible rows in ascending price order.
    pub rows: Vec<DomPriceRow>,
}

/// One price row in a rendered session profile.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ProfilePriceRow {
    /// Exact price.
    pub price: Price,
    /// ETH-anchored 30-minute period bitset.
    pub eth_periods: u64,
    /// RTH-anchored 30-minute period bitset.
    pub rth_periods: u64,
    /// Session volume spectrum value at this price.
    pub session_volume: u64,
    /// Developing-period volume at this price.
    pub period_volume: u64,
    /// Buy-aggressor volume.
    pub buy_volume: u64,
    /// Sell-aggressor volume.
    pub sell_volume: u64,
}

/// Render data for one CT trade-date session.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ProfileSessionRender {
    /// CT trade date, days since Unix epoch.
    pub trade_date: u32,
    /// Traded price rows in ascending order.
    pub rows: Vec<ProfilePriceRow>,
    /// Volume point of control.
    pub vpoc: Option<Price>,
    /// Value-area high.
    pub vah: Option<Price>,
    /// Value-area low.
    pub val: Option<Price>,
    /// Initial-balance low.
    pub ib_low: Option<Price>,
    /// Initial-balance high.
    pub ib_high: Option<Price>,
    /// Current ETH period index driving PV.
    pub current_period: u32,
    /// True when the developing PV period crosses a source gap.
    pub period_gap: bool,
    /// Session cumulative volume delta.
    pub cvd: i64,
    /// Session volume-delta spectrum `(sell, buy)` is carried per price in rows.
    pub cvd_candles: Vec<CvdCandle>,
}

/// All sessions needed by the Market Profile pane.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ProfileRenderState {
    /// Sessions in ascending CT trade-date order.
    pub sessions: Vec<ProfileSessionRender>,
}

/// Event-coverage accounting for forward/live flow (`docs/ENGINE.md` §3). Seek
/// resolution is accounted by its bit-identical gate, not these counters.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct CoverageCounters {
    /// Events decoded from the source since `SetSource`.
    pub events_read: u64,
    /// Events applied to book+profile exactly once.
    pub events_applied: u64,
    /// Gap events encountered (downstream state = unavailable, loud, not a drop).
    pub gap_records: u64,
}

/// One coherent generation consumed by both panes.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RenderSnapshot {
    /// Monotonic publication generation.
    pub generation: u64,
    /// Last source sequence reflected in the state.
    pub applied_seq: u64,
    /// Event timestamp of that sequence.
    pub applied_ts: u64,
    /// Seek generation answered, or zero for forward/live flow.
    pub seek_generation: u64,
    /// Bounded DOM state.
    pub dom: DomRenderState,
    /// Session profile state.
    pub profile: ProfileRenderState,
    /// Event-coverage accounting (M3 gate surface).
    pub coverage: CoverageCounters,
}

impl RenderSnapshot {
    /// Conservative heap-byte estimate from vector capacities.
    pub fn estimated_heap_bytes(&self) -> usize {
        let dom = self.dom.rows.capacity() * size_of::<DomPriceRow>();
        let sessions = self.profile.sessions.capacity() * size_of::<ProfileSessionRender>();
        let profile_rows = self
            .profile
            .sessions
            .iter()
            .map(|session| session.rows.capacity() * size_of::<ProfilePriceRow>())
            .sum::<usize>();
        let candles = self
            .profile
            .sessions
            .iter()
            .map(|session| session.cvd_candles.capacity() * size_of::<CvdCandle>())
            .sum::<usize>();
        dom + sessions + profile_rows + candles
    }
}

/// Lock-free latest-value slot. Loading clones only the stored [`Arc`].
#[derive(Clone)]
pub struct SnapshotSlot {
    inner: Arc<ArcSwap<RenderSnapshot>>,
}

impl SnapshotSlot {
    /// Create a slot containing `initial`.
    pub fn new(initial: Arc<RenderSnapshot>) -> Self {
        Self {
            inner: Arc::new(ArcSwap::from(initial)),
        }
    }

    /// Load exactly one coherent snapshot.
    pub fn load(&self) -> Arc<RenderSnapshot> {
        self.inner.load_full()
    }

    pub(crate) fn publish(&self, snapshot: RenderSnapshot) {
        self.inner.store(Arc::new(snapshot));
    }
}

/// Construct a bounded snapshot without cloning or walking the far L3 book.
pub fn build_snapshot(
    generation: u64,
    applied_seq: u64,
    applied_ts: u64,
    seek_generation: u64,
    visible_ticks: usize,
    book: &Book,
    profiles: &MultiProfile,
) -> RenderSnapshot {
    assert!(visible_ticks > 0, "visible tick span must be non-zero");
    let tick = book.tick_size();
    let best_bid = book.best_bid();
    let best_ask = book.best_ask();
    let center = match (best_bid, best_ask, book.last_trade()) {
        (Some(bid), Some(ask), _) => Price((bid.0 + ask.0) / 2),
        (Some(bid), None, _) => bid,
        (None, Some(ask), _) => ask,
        (None, None, Some(last)) => last.price,
        (None, None, None) => Price(0),
    };
    let center_tick = center.0.div_euclid(tick.0);
    let first_tick = center_tick - i64::try_from(visible_ticks / 2).expect("span fits i64");
    let inside = book.traded_at_inside();
    let current = profiles.current();
    let mut rows = Vec::with_capacity(visible_ticks);
    for offset in 0..visible_ticks {
        let price = Price(
            (first_tick + i64::try_from(offset).expect("offset fits i64"))
                .checked_mul(tick.0)
                .expect("visible price overflows i64"),
        );
        let bid = book.level(Side::Bid, price);
        let ask = book.level(Side::Ask, price);
        let bid_refresh = book.refresh_at(Side::Bid, price);
        let ask_refresh = book.refresh_at(Side::Ask, price);
        rows.push(DomPriceRow {
            price,
            bid_size: bid.total_size,
            ask_size: ask.total_size,
            session_volume: current.map_or(0, |session| session.row(price).volume),
            cb: u64::from(inside.bid_price == Some(price)) * inside.bid_vol,
            ca: u64::from(inside.ask_price == Some(price)) * inside.ask_vol,
            bid_added_5s: bid.added_5s,
            bid_cancelled_5s: bid.cancelled_5s,
            ask_added_5s: ask.added_5s,
            ask_cancelled_5s: ask.cancelled_5s,
            refresh_agg: PriceRefreshRender {
                bid_count: bid_refresh.refresh_count,
                ask_count: ask_refresh.refresh_count,
                bid_hidden: bid_refresh.hidden_volume,
                ask_hidden: ask_refresh.hidden_volume,
            },
        });
    }

    RenderSnapshot {
        generation,
        applied_seq,
        applied_ts,
        seek_generation,
        // Stamped by the engine service at publication; build stays coverage-agnostic.
        coverage: CoverageCounters::default(),
        dom: DomRenderState {
            tick_size: tick,
            best_bid,
            best_ask,
            last_trade: book.last_trade(),
            rows,
        },
        profile: build_profiles(profiles),
    }
}

fn build_profiles(profiles: &MultiProfile) -> ProfileRenderState {
    let sessions = profiles
        .sessions()
        .iter()
        .map(|session| {
            let rows = session.range().map_or_else(Vec::new, |(low, high)| {
                let count = (high.0 - low.0).div_euclid(session.tick().0) + 1;
                (0..count)
                    .map(|offset| {
                        let price = Price(low.0 + offset * session.tick().0);
                        let row = session.row(price);
                        ProfilePriceRow {
                            price,
                            eth_periods: row.eth_periods,
                            rth_periods: row.rth_periods,
                            session_volume: row.volume,
                            period_volume: row.period_volume,
                            buy_volume: row.buy_volume,
                            sell_volume: row.sell_volume,
                        }
                    })
                    .collect()
            });
            let (val, vah) = session
                .value_area()
                .map_or((None, None), |(lo, hi)| (Some(lo), Some(hi)));
            let (ib_low, ib_high) = session
                .initial_balance()
                .map_or((None, None), |(lo, hi)| (Some(lo), Some(hi)));
            ProfileSessionRender {
                trade_date: session.trade_date(),
                rows,
                vpoc: session.vpoc(),
                vah,
                val,
                ib_low,
                ib_high,
                current_period: session.current_eth_period(),
                period_gap: session.period_gap(),
                cvd: session.session_delta(),
                cvd_candles: session.cvd().candles().to_vec(),
            }
        })
        .collect();
    ProfileRenderState { sessions }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn snapshot_header_stays_small() {
        assert!(size_of::<RenderSnapshot>() <= 256);
        assert!(size_of::<DomPriceRow>() <= 112);
        assert!(size_of::<ProfilePriceRow>() <= 64);
    }
}
