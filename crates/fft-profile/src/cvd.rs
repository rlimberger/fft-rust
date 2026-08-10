//! Cumulative volume delta (buy − sell aggressor) with per-period OHLC
//! candles, plus Grady cB/cA traded-at-touch counters (reset on price change).

use fft_core::{Price, Side};

/// One 30-minute period of cumulative CVD as an OHLC candle.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct CvdCandle {
    pub open: i64,
    pub high: i64,
    pub low: i64,
    pub close: i64,
}

impl CvdCandle {
    pub fn flat(v: i64) -> CvdCandle {
        CvdCandle {
            open: v,
            high: v,
            low: v,
            close: v,
        }
    }

    pub fn is_up(self) -> bool {
        self.close >= self.open
    }

    pub fn period_delta(self) -> i64 {
        self.close - self.open
    }
}

/// One side's traded-at-touch counter (Grady cB/cA): accumulates aggressor
/// volume at the current touch price and resets when that price changes.
///
/// A feed gap marks the counter (`is_gap`) without discarding the count —
/// panes render an honesty marker. The mark clears on the next price change,
/// when the counter is fresh again.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct TouchCounter {
    pub(crate) price: Option<Price>,
    pub(crate) volume: u64,
    pub(crate) gap: bool,
}

impl TouchCounter {
    fn on_trade(&mut self, price: Price, size: u64) {
        if self.price != Some(price) {
            self.price = Some(price);
            self.volume = 0;
            self.gap = false;
        }
        self.volume += size;
    }

    /// Counter value if `price` is the current touch price, else 0.
    pub fn at(&self, price: Price) -> u64 {
        if self.price == Some(price) {
            self.volume
        } else {
            0
        }
    }

    pub fn price(&self) -> Option<Price> {
        self.price
    }

    pub fn volume(&self) -> u64 {
        self.volume
    }

    /// True when a feed gap occurred since the counter last reset.
    pub fn is_gap(&self) -> bool {
        self.gap
    }
}

/// Session-scoped cumulative volume delta: running buy/sell aggressor totals,
/// session CVD high/low, per-period candles (candle `i` = ETH-anchored period
/// `i`), and the cB/cA touch counters.
#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub struct Cvd {
    pub(crate) buy_volume: u64,
    pub(crate) sell_volume: u64,
    pub(crate) high: i64,
    pub(crate) low: i64,
    pub(crate) candles: Vec<CvdCandle>,
    pub(crate) cur_bid: TouchCounter,
    pub(crate) cur_ask: TouchCounter,
}

impl Cvd {
    /// `aggressor` per the Databento MBO trade convention: `Bid` = buy
    /// aggressor (lifts the offer → cA), `Ask` = sell aggressor (hits the bid
    /// → cB). A sideless trade carries no delta information and is ignored
    /// here (its volume still lands in the profile arrays).
    pub(crate) fn on_trade(&mut self, price: Price, size: u64, aggressor: Side, period: u32) {
        match aggressor {
            Side::Bid => {
                self.buy_volume += size;
                self.cur_ask.on_trade(price, size);
            }
            Side::Ask => {
                self.sell_volume += size;
                self.cur_bid.on_trade(price, size);
            }
            Side::None => return,
        }
        let d = self.delta();
        push_candle(&mut self.candles, period, d);
        self.high = self.high.max(d);
        self.low = self.low.min(d);
    }

    pub(crate) fn mark_gap(&mut self) {
        self.cur_bid.gap = true;
        self.cur_ask.gap = true;
    }

    /// Session delta: buy-aggressor − sell-aggressor volume.
    pub fn delta(&self) -> i64 {
        i64::try_from(self.buy_volume).expect("buy volume fits i64")
            - i64::try_from(self.sell_volume).expect("sell volume fits i64")
    }

    pub fn buy_volume(&self) -> u64 {
        self.buy_volume
    }

    pub fn sell_volume(&self) -> u64 {
        self.sell_volume
    }

    /// `(cvd_low, cvd_high)` over the session (both include the 0 start).
    pub fn range(&self) -> (i64, i64) {
        (self.low, self.high)
    }

    /// Per-period candles; index = ETH-anchored period index. Periods without
    /// trades are flat carries of the prior close.
    pub fn candles(&self) -> &[CvdCandle] {
        &self.candles
    }

    /// cB: sell-aggressor volume traded at the current bid touch.
    pub fn current_bid(&self) -> &TouchCounter {
        &self.cur_bid
    }

    /// cA: buy-aggressor volume traded at the current ask touch.
    pub fn current_ask(&self) -> &TouchCounter {
        &self.cur_ask
    }
}

/// Extend or update the candle series so candle `period` reflects `cvd`.
/// Skipped periods are filled flat at the prior close.
pub(crate) fn push_candle(candles: &mut Vec<CvdCandle>, period: u32, cvd: i64) {
    let idx = period as usize;
    if candles.len() <= idx {
        while candles.len() < idx {
            let prev = candles.last().map_or(0, |c| c.close);
            candles.push(CvdCandle::flat(prev));
        }
        let open = candles.last().map_or(0, |c| c.close);
        candles.push(CvdCandle {
            open,
            high: cvd.max(open),
            low: cvd.min(open),
            close: cvd,
        });
    } else {
        let c = &mut candles[idx];
        c.close = cvd;
        c.high = c.high.max(cvd);
        c.low = c.low.min(cvd);
    }
}
