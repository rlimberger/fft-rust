//! One session's Market Profile: dense per-price arrays plus derived stats.

use crate::cvd::Cvd;
use crate::tpo::SessionClock;
use crate::value_area;
use fft_core::{CanonicalEvent, EventKind, Price, Side, Ts};

/// Pad ticks when growing the dense price arrays.
const GROW_PAD: usize = 64;
/// Hard ceiling on the per-session price span, in ticks. A span past this is
/// corrupt input (ES trades a few thousand ticks per session), so exceeding it
/// is a loud panic, not a silent drop (deliberate change from legacy).
pub(crate) const MAX_SPAN_TICKS: i64 = 1_000_000;
/// Period bitsets are `u64`; a 23-hour session has 46 periods, so every valid
/// index fits. Exceeding this means the clock produced garbage.
const MAX_PERIOD_BIT: u32 = 63;

/// Per-price render row. `eth_periods` letters run continuously from the
/// Globex open (bit 0 = ETH A) across the whole session; `rth_periods`
/// restarts at A at the RTH open. Both are maintained for every trade — the
/// pane chooses (mask `eth_periods` below [`SessionClock::rth_offset_periods`]
/// to reproduce the legacy split view).
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct ProfileRow {
    pub eth_periods: u64,
    pub rth_periods: u64,
    /// Total 30-minute periods that touched this price (ETH-stream count).
    pub tpo_count: u32,
    pub volume: u64,
    /// Volume at this price during the developing period only (PV feed).
    pub period_volume: u64,
    /// Buy-aggressor volume (traded at the ask).
    pub buy_volume: u64,
    /// Sell-aggressor volume (traded at the bid).
    pub sell_volume: u64,
    pub single_print: bool,
    pub is_poc: bool,
}

/// Developing (or frozen prior) session profile. All prices are exact
/// multiples of the tick; internally rows live in dense tick-indexed arrays.
#[derive(Clone, Debug)]
pub struct SessionProfile {
    pub(crate) clock: SessionClock,
    pub(crate) tick: Price,
    pub(crate) base_tick: i64,
    pub(crate) eth_periods: Vec<u64>,
    pub(crate) rth_periods: Vec<u64>,
    pub(crate) volume: Vec<u64>,
    pub(crate) period_volume: Vec<u64>,
    pub(crate) buy_volume: Vec<u64>,
    pub(crate) sell_volume: Vec<u64>,
    pub(crate) total_volume: u64,
    pub(crate) low_tick: Option<i64>,
    pub(crate) high_tick: Option<i64>,
    pub(crate) open_tick: Option<i64>,
    pub(crate) poc_tick: Option<i64>,
    pub(crate) poc_volume: u64,
    pub(crate) ib_low_tick: Option<i64>,
    pub(crate) ib_high_tick: Option<i64>,
    /// ETH-anchored index of the developing period.
    pub(crate) current_eth_period: u32,
    /// True when a feed gap landed in the developing period: its PV
    /// accumulation is incomplete. Clears at the next period roll.
    pub(crate) period_gap: bool,
    pub(crate) cvd: Cvd,
}

/// Equality is semantic: dense arrays compare over the traded range only.
/// Growth padding is capacity, not state — a restored profile (trimmed) must
/// equal the live profile it checkpointed (padded).
impl PartialEq for SessionProfile {
    fn eq(&self, other: &SessionProfile) -> bool {
        fn trimmed<'a>(s: &'a SessionProfile, arr: &'a [u64]) -> &'a [u64] {
            match (s.low_tick, s.high_tick) {
                (Some(lo), Some(hi)) => {
                    let a = usize::try_from(lo - s.base_tick).expect("low within arrays");
                    let b = usize::try_from(hi - s.base_tick).expect("high within arrays");
                    &arr[a..=b]
                }
                _ => &[],
            }
        }
        self.clock == other.clock
            && self.tick == other.tick
            && self.total_volume == other.total_volume
            && self.low_tick == other.low_tick
            && self.high_tick == other.high_tick
            && self.open_tick == other.open_tick
            && self.poc_tick == other.poc_tick
            && self.poc_volume == other.poc_volume
            && self.ib_low_tick == other.ib_low_tick
            && self.ib_high_tick == other.ib_high_tick
            && self.current_eth_period == other.current_eth_period
            && self.period_gap == other.period_gap
            && self.cvd == other.cvd
            && trimmed(self, &self.eth_periods) == trimmed(other, &other.eth_periods)
            && trimmed(self, &self.rth_periods) == trimmed(other, &other.rth_periods)
            && trimmed(self, &self.volume) == trimmed(other, &other.volume)
            && trimmed(self, &self.period_volume) == trimmed(other, &other.period_volume)
            && trimmed(self, &self.buy_volume) == trimmed(other, &other.buy_volume)
            && trimmed(self, &self.sell_volume) == trimmed(other, &other.sell_volume)
    }
}

impl Eq for SessionProfile {}

impl SessionProfile {
    /// `tick` = instrument `min_price_increment` (1e-9 price units).
    pub fn new(clock: SessionClock, tick: Price) -> SessionProfile {
        assert!(tick.0 > 0, "tick must be positive, got {tick:?}");
        SessionProfile {
            clock,
            tick,
            base_tick: 0,
            eth_periods: Vec::new(),
            rth_periods: Vec::new(),
            volume: Vec::new(),
            period_volume: Vec::new(),
            buy_volume: Vec::new(),
            sell_volume: Vec::new(),
            total_volume: 0,
            low_tick: None,
            high_tick: None,
            open_tick: None,
            poc_tick: None,
            poc_volume: 0,
            ib_low_tick: None,
            ib_high_tick: None,
            current_eth_period: 0,
            period_gap: false,
            cvd: Cvd::default(),
        }
    }

    /// Apply one canonical event. Trades accumulate; a Gap marks the in-flight
    /// counters (cB/cA, developing-period volume) as gap state; every other
    /// kind is book/engine business and is ignored here. `Fill` is the
    /// resting-side notice of a `Trade` — counting both would double-count.
    pub fn apply(&mut self, ev: &CanonicalEvent) {
        match ev.kind {
            EventKind::Trade => self.on_trade(ev.price, ev.size, ev.side, ev.ts),
            EventKind::Gap => self.on_gap(),
            _ => {}
        }
    }

    /// `aggressor` = trade `side` per the Databento MBO convention (`Bid` =
    /// buy aggressor). A sideless trade counts volume and TPO but carries no
    /// delta or touch-counter information.
    fn on_trade(&mut self, price: Price, size: u32, aggressor: Side, ts: Ts) {
        assert!(size > 0, "zero-size trade at {ts:?}");
        let t = self.to_tick(price);
        let eth_p = self.clock.eth_period(ts);
        assert!(
            eth_p >= self.current_eth_period,
            "trade time went backwards: period {eth_p} after {}",
            self.current_eth_period
        );
        if eth_p > self.current_eth_period {
            self.period_volume.fill(0);
            self.period_gap = false;
            self.current_eth_period = eth_p;
        }
        let rth_p = self.clock.rth_period(ts);

        let idx = self.ensure(t);
        let sz = u64::from(size);
        assert!(eth_p <= MAX_PERIOD_BIT, "period {eth_p} exceeds bitset");
        self.eth_periods[idx] |= 1 << eth_p;
        if let Some(r) = rth_p {
            assert!(r <= MAX_PERIOD_BIT, "RTH period {r} exceeds bitset");
            self.rth_periods[idx] |= 1 << r;
        }
        self.volume[idx] += sz;
        self.period_volume[idx] += sz;
        match aggressor {
            Side::Bid => self.buy_volume[idx] += sz,
            Side::Ask => self.sell_volume[idx] += sz,
            Side::None => {}
        }
        self.total_volume += sz;

        self.low_tick = Some(self.low_tick.map_or(t, |l| l.min(t)));
        self.high_tick = Some(self.high_tick.map_or(t, |h| h.max(t)));
        if self.open_tick.is_none() {
            self.open_tick = Some(t);
        }
        if matches!(rth_p, Some(0 | 1)) {
            self.ib_low_tick = Some(self.ib_low_tick.map_or(t, |l| l.min(t)));
            self.ib_high_tick = Some(self.ib_high_tick.map_or(t, |h| h.max(t)));
        }
        if self.volume[idx] > self.poc_volume {
            self.poc_volume = self.volume[idx];
            self.poc_tick = Some(t);
        }

        self.cvd.on_trade(price, sz, aggressor, eth_p);
    }

    fn on_gap(&mut self) {
        self.period_gap = true;
        self.cvd.mark_gap();
    }

    pub fn clock(&self) -> &SessionClock {
        &self.clock
    }

    pub fn trade_date(&self) -> u32 {
        self.clock.trade_date()
    }

    pub fn tick(&self) -> Price {
        self.tick
    }

    /// Per-price row; the zero row for untraded prices. Panics on a price off
    /// the tick grid — that is a caller bug, not missing data.
    pub fn row(&self, price: Price) -> ProfileRow {
        let t = self.to_tick(price);
        let Some(idx) = self.index_of(t) else {
            return ProfileRow::default();
        };
        let eth = self.eth_periods[idx];
        let tpo_count = eth.count_ones();
        ProfileRow {
            eth_periods: eth,
            rth_periods: self.rth_periods[idx],
            tpo_count,
            volume: self.volume[idx],
            period_volume: self.period_volume[idx],
            buy_volume: self.buy_volume[idx],
            sell_volume: self.sell_volume[idx],
            single_print: tpo_count == 1,
            is_poc: self.poc_tick == Some(t),
        }
    }

    /// Volume point of control.
    pub fn vpoc(&self) -> Option<Price> {
        self.poc_tick.map(|t| self.to_price(t))
    }

    /// `(VAL, VAH)`: 70% volume value area around the VPOC, computed on
    /// demand from the dense array (no cached/debounced state to checkpoint).
    pub fn value_area(&self) -> Option<(Price, Price)> {
        let (low, high, poc) = (self.low_tick?, self.high_tick?, self.poc_tick?);
        let off = |t: i64| usize::try_from(t - self.base_tick).expect("tick within arrays");
        let (lo_i, hi_i) = value_area::compute(
            &self.volume,
            off(low),
            off(high),
            off(poc),
            self.total_volume,
        );
        let up = |i: usize| self.to_price(self.base_tick + i64::try_from(i).expect("index fits"));
        Some((up(lo_i), up(hi_i)))
    }

    /// Initial Balance range: first two RTH periods (RTH A and B).
    pub fn initial_balance(&self) -> Option<(Price, Price)> {
        match (self.ib_low_tick, self.ib_high_tick) {
            (Some(lo), Some(hi)) => Some((self.to_price(lo), self.to_price(hi))),
            _ => None,
        }
    }

    /// Session `(low, high)`.
    pub fn range(&self) -> Option<(Price, Price)> {
        match (self.low_tick, self.high_tick) {
            (Some(lo), Some(hi)) => Some((self.to_price(lo), self.to_price(hi))),
            _ => None,
        }
    }

    /// First print of the session.
    pub fn open_price(&self) -> Option<Price> {
        self.open_tick.map(|t| self.to_price(t))
    }

    pub fn total_volume(&self) -> u64 {
        self.total_volume
    }

    pub fn session_delta(&self) -> i64 {
        self.cvd.delta()
    }

    /// ETH-anchored index of the developing period.
    pub fn current_eth_period(&self) -> u32 {
        self.current_eth_period
    }

    /// RTH-anchored index of the developing period; `None` before RTH.
    pub fn current_rth_period(&self) -> Option<u32> {
        self.current_eth_period
            .checked_sub(self.clock.rth_offset_periods())
    }

    /// True when a feed gap landed in the developing period, so PV columns
    /// for it are incomplete. Session volume/TPO already accumulated stays.
    pub fn period_gap(&self) -> bool {
        self.period_gap
    }

    /// CVD candles, session delta components, and cB/cA touch counters.
    pub fn cvd(&self) -> &Cvd {
        &self.cvd
    }

    fn to_tick(&self, price: Price) -> i64 {
        assert!(
            price.0.rem_euclid(self.tick.0) == 0,
            "price {price:?} off the {:?} tick grid",
            self.tick
        );
        price.0.div_euclid(self.tick.0)
    }

    fn to_price(&self, tick: i64) -> Price {
        Price(tick * self.tick.0)
    }

    fn index_of(&self, t: i64) -> Option<usize> {
        if self.volume.is_empty() || t < self.base_tick {
            return None;
        }
        let idx = usize::try_from(t - self.base_tick).expect("checked non-negative");
        (idx < self.volume.len()).then_some(idx)
    }

    fn ensure(&mut self, t: i64) -> usize {
        if self.volume.is_empty() {
            self.base_tick = t - i64::try_from(GROW_PAD).expect("pad fits");
            self.resize_to(2 * GROW_PAD + 1);
            return GROW_PAD;
        }
        let off = t - self.base_tick;
        if off < 0 {
            assert!(
                -off <= MAX_SPAN_TICKS,
                "price {t} ticks blows the session span ceiling"
            );
            self.grow_front(usize::try_from(-off).expect("checked") + GROW_PAD);
        } else if usize::try_from(off).expect("checked") >= self.volume.len() {
            assert!(
                off - i64::try_from(self.volume.len()).expect("len fits") <= MAX_SPAN_TICKS,
                "price {t} ticks blows the session span ceiling"
            );
            self.resize_to(usize::try_from(off).expect("checked") + 1 + GROW_PAD);
        }
        let len = i64::try_from(self.volume.len()).expect("len fits");
        assert!(len <= MAX_SPAN_TICKS, "session span exceeds ceiling");
        usize::try_from(t - self.base_tick).expect("in range after grow")
    }

    fn resize_to(&mut self, len: usize) {
        self.eth_periods.resize(len, 0);
        self.rth_periods.resize(len, 0);
        self.volume.resize(len, 0);
        self.period_volume.resize(len, 0);
        self.buy_volume.resize(len, 0);
        self.sell_volume.resize(len, 0);
    }

    fn grow_front(&mut self, n: usize) {
        for v in [
            &mut self.eth_periods,
            &mut self.rth_periods,
            &mut self.volume,
            &mut self.period_volume,
            &mut self.buy_volume,
            &mut self.sell_volume,
        ] {
            v.splice(0..0, std::iter::repeat_n(0, n));
        }
        self.base_tick -= i64::try_from(n).expect("grow fits");
    }
}
