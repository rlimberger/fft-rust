//! fftlog v2 BOOK checkpoint section (id 1, version 1) — `docs/FFTLOG-V2.md` §5.
//!
//! Layout v1, little-endian, no varints, deterministic (never hash-map order):
//!
//! ```text
//! u16 version (=1) · i64 tick · u64 now
//! u8+u64 last_seq · u8 gap_pending · u8+u64+u64 last_gap · u64 unknown_refs
//! u8+i64+u32+u8 last_trade(ticks,size,aggressor)
//! u8+i64+u64 cB · u8+i64+u64 cA
//! per side (bid, then ask):
//!   u8 initialised · i64 base_tick · u8+i64 best · u32 level_count
//!   per level, price order (bids desc, asks asc), levels with orders or a
//!   fresh flow window only:
//!     i64 price_ticks · u64 flow.last_bucket · 10×u32 added · 10×u32
//!     cancelled · 10×u32 traded · u32 order_count
//!     per order, strictly head-to-tail FIFO:
//!       u64 id · u32 size · u64 ts · u32 epoch
//! refresh:
//!   u32 gap_epoch
//!   u32 n · per live entry, ascending id: u64 id · u32 reloads · u64 hidden ·
//!     u8 unavailable
//!   u32 n · per tombstone, ascending id: u64 id · u8 side · i64 price_ticks ·
//!     u64 depleted_ts · u32 epoch · u32 reloads · u64 hidden
//!   u32 n · per price aggregate, ascending (side, price): u8 side ·
//!     i64 price_ticks · u32 refresh_count · u64 hidden_volume
//! ```
//!
//! Optional values are always a presence byte followed by zero-filled payload
//! bytes, so encode and decode never branch on layout.

use crate::BOOK_SECTION_VERSION;
use crate::book::Book;
use crate::flow::{Flow, TradedAtInsideTicks};
use crate::level::{Level, NIL, Order};
use crate::refresh::{LiveRefresh, RefreshTracker, Tombstone};
use crate::side::{SideBook, link_tail};
use fft_core::Side;
use slab::Slab;
use std::collections::HashMap;

fn w8(b: &mut Vec<u8>, v: u8) {
    b.push(v);
}
fn w16(b: &mut Vec<u8>, v: u16) {
    b.extend_from_slice(&v.to_le_bytes());
}
fn w32(b: &mut Vec<u8>, v: u32) {
    b.extend_from_slice(&v.to_le_bytes());
}
fn w64(b: &mut Vec<u8>, v: u64) {
    b.extend_from_slice(&v.to_le_bytes());
}
fn wi64(b: &mut Vec<u8>, v: i64) {
    b.extend_from_slice(&v.to_le_bytes());
}
fn wopt_u64(b: &mut Vec<u8>, v: Option<u64>) {
    w8(b, u8::from(v.is_some()));
    w64(b, v.unwrap_or(0));
}
fn wopt_i64(b: &mut Vec<u8>, v: Option<i64>) {
    w8(b, u8::from(v.is_some()));
    wi64(b, v.unwrap_or(0));
}

struct Rd<'a> {
    b: &'a [u8],
    pos: usize,
}

impl<'a> Rd<'a> {
    fn take(&mut self, n: usize) -> &'a [u8] {
        assert!(
            self.pos + n <= self.b.len(),
            "fft-book: truncated BOOK section at byte {} (want {n} more of {})",
            self.pos,
            self.b.len()
        );
        let s = &self.b[self.pos..self.pos + n];
        self.pos += n;
        s
    }
    fn u8(&mut self) -> u8 {
        self.take(1)[0]
    }
    fn u16(&mut self) -> u16 {
        u16::from_le_bytes(self.take(2).try_into().unwrap())
    }
    fn u32(&mut self) -> u32 {
        u32::from_le_bytes(self.take(4).try_into().unwrap())
    }
    fn u64(&mut self) -> u64 {
        u64::from_le_bytes(self.take(8).try_into().unwrap())
    }
    fn i64(&mut self) -> i64 {
        i64::from_le_bytes(self.take(8).try_into().unwrap())
    }
    fn opt_u64(&mut self) -> Option<u64> {
        let has = self.u8() != 0;
        let v = self.u64();
        has.then_some(v)
    }
    fn opt_i64(&mut self) -> Option<i64> {
        let has = self.u8() != 0;
        let v = self.i64();
        has.then_some(v)
    }
    fn side(&mut self) -> Side {
        let raw = self.u8();
        Side::from_u8(raw)
            .unwrap_or_else(|| panic!("fft-book: bad side byte {raw} in BOOK section"))
    }
    fn done(&self) {
        assert!(
            self.pos == self.b.len(),
            "fft-book: {} trailing bytes in BOOK section",
            self.b.len() - self.pos
        );
    }
}

fn write_side(b: &mut Vec<u8>, sb: &SideBook, orders: &Slab<Order>, now: u64) {
    w8(b, u8::from(sb.initialised));
    wi64(b, sb.base_tick);
    wopt_i64(b, sb.best);
    let mut n = 0u32;
    sb.for_each_alive(now, |_, _| n += 1);
    w32(b, n);
    sb.for_each_alive(now, |price, level| {
        wi64(b, price);
        w64(b, level.flow.last_bucket);
        for v in &level.flow.added {
            w32(b, *v);
        }
        for v in &level.flow.cancelled {
            w32(b, *v);
        }
        for v in &level.flow.traded {
            w32(b, *v);
        }
        w32(b, level.order_count);
        let mut cur = level.head;
        while cur != NIL {
            let o = &orders[cur as usize];
            w64(b, o.id);
            w32(b, o.size);
            w64(b, o.ts);
            w32(b, o.epoch);
            cur = o.next;
        }
    });
}

#[allow(clippy::type_complexity)]
fn read_side(
    r: &mut Rd<'_>,
    sb: &mut SideBook,
    side: Side,
    orders: &mut Slab<Order>,
    index: &mut HashMap<u64, u32>,
) {
    sb.initialised = r.u8() != 0;
    sb.base_tick = r.i64();
    sb.best = r.opt_i64();
    let n_levels = r.u32();
    for _ in 0..n_levels {
        let price = r.i64();
        let mut flow = Flow {
            last_bucket: r.u64(),
            ..Flow::default()
        };
        for v in &mut flow.added {
            *v = r.u32();
        }
        for v in &mut flow.cancelled {
            *v = r.u32();
        }
        for v in &mut flow.traded {
            *v = r.u32();
        }
        let n_orders = r.u32();
        sb.insert_restored_level(
            price,
            Level {
                flow,
                ..Level::default()
            },
        );
        for _ in 0..n_orders {
            let id = r.u64();
            let size = r.u32();
            let ts = r.u64();
            let epoch = r.u32();
            assert!(size > 0, "fft-book: zero-size order {id} in BOOK section");
            let o = Order {
                id,
                price,
                side,
                size,
                ts,
                epoch,
                prev: NIL,
                next: NIL,
            };
            let slot = u32::try_from(orders.insert(o)).expect("fft-book: order slot exceeds u32");
            assert!(
                index.insert(id, slot).is_none(),
                "fft-book: duplicate order id {id} in BOOK section"
            );
            link_tail(sb, orders, slot);
        }
    }
}

impl Book {
    /// Encode the complete book state as a BOOK checkpoint section payload
    /// (version [`BOOK_SECTION_VERSION`]). Deterministic: equal state encodes
    /// to equal bytes.
    pub fn serialize(&self) -> Vec<u8> {
        let mut b = Vec::new();
        w16(&mut b, BOOK_SECTION_VERSION);
        wi64(&mut b, self.tick);
        w64(&mut b, self.now);
        wopt_u64(&mut b, self.last_seq);
        w8(&mut b, u8::from(self.gap_pending));
        let (has_gap, ge, go) = match self.last_gap {
            Some((e, o)) => (1u8, e, o),
            None => (0, 0, 0),
        };
        w8(&mut b, has_gap);
        w64(&mut b, ge);
        w64(&mut b, go);
        w64(&mut b, self.unknown_refs);
        let (has_lt, lt_p, lt_s, lt_a) = match self.last_trade {
            Some((p, s, a)) => (1u8, p, s, a as u8),
            None => (0, 0, 0, 0),
        };
        w8(&mut b, has_lt);
        wi64(&mut b, lt_p);
        w32(&mut b, lt_s);
        w8(&mut b, lt_a);
        wopt_i64(&mut b, self.tai.bid_price);
        w64(&mut b, self.tai.bid_vol);
        wopt_i64(&mut b, self.tai.ask_price);
        w64(&mut b, self.tai.ask_vol);

        write_side(&mut b, &self.bids, &self.orders, self.now);
        write_side(&mut b, &self.asks, &self.orders, self.now);

        w32(&mut b, self.refresh.gap_epoch);
        let mut live: Vec<(&u64, &LiveRefresh)> = self.refresh.live.iter().collect();
        live.sort_by_key(|(id, _)| **id);
        w32(
            &mut b,
            u32::try_from(live.len()).expect("fft-book: refresh entry count"),
        );
        for (id, e) in live {
            w64(&mut b, *id);
            w32(&mut b, e.reloads);
            w64(&mut b, e.hidden);
            w8(&mut b, u8::from(e.unavailable));
        }
        let mut tombs: Vec<(&u64, &Tombstone)> = self.refresh.tombstones.iter().collect();
        tombs.sort_by_key(|(id, _)| **id);
        w32(
            &mut b,
            u32::try_from(tombs.len()).expect("fft-book: tombstone count"),
        );
        for (id, t) in tombs {
            w64(&mut b, *id);
            w8(&mut b, t.side as u8);
            wi64(&mut b, t.price);
            w64(&mut b, t.depleted_ts);
            w32(&mut b, t.epoch);
            w32(&mut b, t.reloads);
            w64(&mut b, t.hidden);
        }
        let aggs = &self.refresh.per_price;
        w32(
            &mut b,
            u32::try_from(aggs.len()).expect("fft-book: aggregate count"),
        );
        for (&(side, price), agg) in aggs {
            w8(&mut b, side);
            wi64(&mut b, price);
            w32(&mut b, agg.refresh_count);
            w64(&mut b, agg.hidden_volume);
        }
        b
    }

    /// Reconstruct a book from a BOOK section payload — FIFO order, flow
    /// windows, refresh and sequence state, everything. Never replays events.
    /// Panics loudly on version mismatch, truncation, or inconsistent payload
    /// (the restored book is invariant-checked before it is returned).
    pub fn restore(bytes: &[u8]) -> Book {
        let mut r = Rd { b: bytes, pos: 0 };
        let ver = r.u16();
        assert!(
            ver == BOOK_SECTION_VERSION,
            "fft-book: BOOK section version {ver}, this build reads {BOOK_SECTION_VERSION}"
        );
        let tick = r.i64();
        assert!(
            tick > 0,
            "fft-book: non-positive tick {tick} in BOOK section"
        );
        let now = r.u64();
        let last_seq = r.opt_u64();
        let gap_pending = r.u8() != 0;
        let has_gap = r.u8() != 0;
        let ge = r.u64();
        let go = r.u64();
        let last_gap = has_gap.then_some((ge, go));
        let unknown_refs = r.u64();
        let has_lt = r.u8() != 0;
        let lt_p = r.i64();
        let lt_s = r.u32();
        let lt_a = Side::from_u8(r.u8())
            .unwrap_or_else(|| panic!("fft-book: bad aggressor byte in BOOK section"));
        let last_trade = has_lt.then_some((lt_p, lt_s, lt_a));
        let tai = TradedAtInsideTicks {
            bid_price: r.opt_i64(),
            bid_vol: r.u64(),
            ask_price: r.opt_i64(),
            ask_vol: r.u64(),
        };

        let mut orders = Slab::new();
        let mut index = HashMap::new();
        let mut bids = SideBook::new(true);
        let mut asks = SideBook::new(false);
        read_side(&mut r, &mut bids, Side::Bid, &mut orders, &mut index);
        read_side(&mut r, &mut asks, Side::Ask, &mut orders, &mut index);

        let mut refresh = RefreshTracker {
            gap_epoch: r.u32(),
            ..RefreshTracker::default()
        };
        for _ in 0..r.u32() {
            let id = r.u64();
            let e = LiveRefresh {
                reloads: r.u32(),
                hidden: r.u64(),
                unavailable: r.u8() != 0,
            };
            refresh.live.insert(id, e);
        }
        for _ in 0..r.u32() {
            let id = r.u64();
            let t = Tombstone {
                side: r.side(),
                price: r.i64(),
                depleted_ts: r.u64(),
                epoch: r.u32(),
                reloads: r.u32(),
                hidden: r.u64(),
            };
            refresh.tombstones.insert(id, t);
        }
        for _ in 0..r.u32() {
            let side = r.u8();
            let price = r.i64();
            let agg = crate::PriceRefreshAgg {
                refresh_count: r.u32(),
                hidden_volume: r.u64(),
            };
            refresh.per_price.insert((side, price), agg);
        }
        r.done();

        let book = Book {
            tick,
            bids,
            asks,
            orders,
            index,
            refresh,
            tai,
            last_trade,
            now,
            since_gc: 0,
            last_seq,
            gap_pending,
            last_gap,
            unknown_refs,
        };
        book.check_invariants();
        book
    }
}
