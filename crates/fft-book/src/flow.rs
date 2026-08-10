//! Per-level 5 s flow window (stacks / pulls / traded-at-touch) and the Grady
//! inside-market "current traded" counters.

use fft_core::Side;

pub(crate) const FLOW_BUCKETS: usize = 10;
pub(crate) const FLOW_BUCKET_NS: u64 = 500_000_000;

/// Rolling 5 s window as 10 × 500 ms buckets keyed by absolute bucket number.
/// `last_bucket == 0` means untouched (event time starts decades past 1970).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct Flow {
    pub added: [u32; FLOW_BUCKETS],
    pub cancelled: [u32; FLOW_BUCKETS],
    pub traded: [u32; FLOW_BUCKETS],
    pub last_bucket: u64,
}

impl Flow {
    fn roll(&mut self, ts: u64) {
        let b = ts / FLOW_BUCKET_NS;
        if b <= self.last_bucket {
            return;
        }
        let n = FLOW_BUCKETS as u64;
        let steps = (b - self.last_bucket).min(n);
        for k in 1..=steps {
            let i = ((self.last_bucket + k) % n) as usize;
            self.added[i] = 0;
            self.cancelled[i] = 0;
            self.traded[i] = 0;
        }
        self.last_bucket = b;
    }

    fn bucket(&mut self, ts: u64) -> usize {
        self.roll(ts);
        (self.last_bucket % FLOW_BUCKETS as u64) as usize
    }

    pub fn record_added(&mut self, ts: u64, size: u32) {
        let i = self.bucket(ts);
        self.added[i] = self.added[i].saturating_add(size);
    }

    pub fn record_cancelled(&mut self, ts: u64, size: u32) {
        let i = self.bucket(ts);
        self.cancelled[i] = self.cancelled[i].saturating_add(size);
    }

    pub fn record_traded(&mut self, ts: u64, size: u32) {
        let i = self.bucket(ts);
        self.traded[i] = self.traded[i].saturating_add(size);
    }

    /// `(added, cancelled, traded)` within the 5 s window ending at `now`.
    pub fn window(&self, now: u64) -> (u32, u32, u32) {
        let cur = now / FLOW_BUCKET_NS;
        let n = FLOW_BUCKETS as u64;
        let lb = self.last_bucket;
        if lb + n <= cur {
            return (0, 0, 0);
        }
        let rem = lb % n;
        let (mut a, mut c, mut t) = (0u32, 0u32, 0u32);
        for slot in 0..FLOW_BUCKETS {
            let back = (rem + n - slot as u64) % n;
            if lb < back {
                continue;
            }
            let b = lb - back;
            if b <= cur && b + n > cur {
                a = a.saturating_add(self.added[slot]);
                c = c.saturating_add(self.cancelled[slot]);
                t = t.saturating_add(self.traded[slot]);
            }
        }
        (a, c, t)
    }

    pub fn stale(&self, now: u64) -> bool {
        self.last_bucket + FLOW_BUCKETS as u64 <= now / FLOW_BUCKET_NS
    }

    pub fn untouched(&self) -> bool {
        self.last_bucket == 0
    }
}

/// Internal (tick-priced) form of [`crate::InsideTraded`].
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct TradedAtInsideTicks {
    pub bid_price: Option<i64>,
    pub bid_vol: u64,
    pub ask_price: Option<i64>,
    pub ask_vol: u64,
}

impl TradedAtInsideTicks {
    /// Aggressive sells hit the bid (cB); aggressive buys lift the ask (cA).
    /// Each counter resets when its trade price changes.
    pub fn on_trade(&mut self, price: i64, size: u32, aggressor: Side) {
        match aggressor {
            Side::Ask => {
                if self.bid_price != Some(price) {
                    self.bid_price = Some(price);
                    self.bid_vol = 0;
                }
                self.bid_vol = self.bid_vol.saturating_add(u64::from(size));
            }
            Side::Bid => {
                if self.ask_price != Some(price) {
                    self.ask_price = Some(price);
                    self.ask_vol = 0;
                }
                self.ask_vol = self.ask_vol.saturating_add(u64::from(size));
            }
            Side::None => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const S: u64 = 1_000_000_000;

    #[test]
    fn window_accumulates_and_expires() {
        let mut f = Flow::default();
        f.record_added(10 * S, 5);
        f.record_cancelled(10 * S + 400_000_000, 2);
        f.record_traded(11 * S, 3);
        assert_eq!(f.window(11 * S), (5, 2, 3));
        // 4.9 s after the first record: everything still in window.
        assert_eq!(f.window(14 * S + 900_000_000), (5, 2, 3));
        // 6 s after the last record: window empty.
        assert_eq!(f.window(17 * S), (0, 0, 0));
        assert!(f.stale(17 * S));
        assert!(!f.stale(12 * S));
    }

    #[test]
    fn partial_expiry_drops_old_buckets_only() {
        let mut f = Flow::default();
        f.record_added(10 * S, 5);
        f.record_added(13 * S, 7);
        // 10 s bucket has rolled out, 13 s bucket still in.
        assert_eq!(f.window(15 * S + 500_000_000), (7, 0, 0));
    }

    #[test]
    fn inside_traded_resets_on_price_change() {
        let mut t = TradedAtInsideTicks::default();
        t.on_trade(100, 5, Side::Ask);
        t.on_trade(100, 3, Side::Ask);
        assert_eq!((t.bid_price, t.bid_vol), (Some(100), 8));
        t.on_trade(99, 2, Side::Ask);
        assert_eq!((t.bid_price, t.bid_vol), (Some(99), 2));
        t.on_trade(101, 4, Side::Bid);
        t.on_trade(101, 1, Side::Bid);
        assert_eq!((t.ask_price, t.ask_vol), (Some(101), 5));
        t.on_trade(100, 9, Side::None);
        assert_eq!((t.bid_price, t.bid_vol), (Some(99), 2));
    }
}
