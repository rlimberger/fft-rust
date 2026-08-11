//! Independent queue oracles for PRD §4 claim 3 (test-only).
//!
//! - [`Shadow`]: Vec-based CME operation model (never walks book links).
//! - [`BookFifo`]: pure prefix-sum queue math over serialized BOOK v3 bytes
//!   (never reuses `query.rs`).
//!
//! Available as `common::queue_oracles`; only the claim-3 gate imports it.

use super::px;
use fft_book::{BOOK_SECTION_VERSION, Book, QueuePosition};
use fft_core::{OrderId, Price, Side};

// ── Shadow oracle (operation model) ─────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SOrder {
    pub id: u64,
    pub side: Side,
    pub ticks: i64,
    pub size: u32,
    /// Snapshot-origin resting order (FIFO prefix ahead of live at its level).
    pub snapshot: bool,
    /// Cumulative Fill qty against the current displayed tranche (size unchanged).
    pub cum_fill: u32,
    /// True once cum_fill ≥ displayed size (CME depletion; companion Modify reinserts).
    pub depleted: bool,
}

#[derive(Default)]
pub struct Shadow {
    pub orders: Vec<SOrder>,
}

impl Shadow {
    pub fn add(&mut self, id: u64, side: Side, ticks: i64, size: u32) {
        assert!(size > 0, "shadow: add size 0");
        assert!(
            self.orders.iter().all(|o| o.id != id),
            "shadow: duplicate id {id}"
        );
        self.orders.push(SOrder {
            id,
            side,
            ticks,
            size,
            snapshot: false,
            cum_fill: 0,
            depleted: false,
        });
    }

    /// Snapshot Add: after existing snapshot prefix at the level, before live.
    pub fn add_snapshot(&mut self, id: u64, side: Side, ticks: i64, size: u32) {
        assert!(size > 0, "shadow: add size 0");
        assert!(
            self.orders.iter().all(|o| o.id != id),
            "shadow: duplicate id {id}"
        );
        let mut after_snap: Option<usize> = None;
        let mut first_live: Option<usize> = None;
        for (i, o) in self.orders.iter().enumerate() {
            if o.side == side && o.ticks == ticks {
                if o.snapshot {
                    after_snap = Some(i);
                } else if first_live.is_none() {
                    first_live = Some(i);
                }
            }
        }
        let idx = match (after_snap, first_live) {
            (Some(i), _) => i + 1,
            (None, Some(i)) => i,
            (None, None) => self.orders.len(),
        };
        self.orders.insert(
            idx,
            SOrder {
                id,
                side,
                ticks,
                size,
                snapshot: true,
                cum_fill: 0,
                depleted: false,
            },
        );
    }

    pub fn cancel_full(&mut self, id: u64) {
        let i = self.pos(id);
        self.orders.remove(i);
    }

    pub fn cancel_qty(&mut self, id: u64, qty: u32) {
        let i = self.pos(id);
        assert!(qty > 0 && qty <= self.orders[i].size);
        if qty == self.orders[i].size {
            self.orders.remove(i);
        } else {
            self.orders[i].size -= qty;
            self.orders[i].cum_fill = 0;
            self.orders[i].depleted = false;
        }
    }

    /// Fill: tape/depletion only — displayed size and FIFO rank unchanged.
    /// Marks depleted when cumulative fill reaches displayed size (mutate.rs
    /// `is_depleted` → companion Modify reinserts at tail).
    pub fn fill(&mut self, id: u64, qty: u32) {
        assert!(qty > 0, "shadow: fill size 0");
        let i = self.pos(id);
        let o = &mut self.orders[i];
        o.cum_fill = o
            .cum_fill
            .checked_add(qty)
            .expect("shadow: cum_fill overflow");
        if o.cum_fill >= o.size {
            o.depleted = true;
        }
    }

    /// CME modify: depleted id → remove + live reinsert at back (native refresh);
    /// else same price + size ≤ current → in place (keeps snapshot flag);
    /// else back of level as live-origin (matches production).
    pub fn modify(&mut self, id: u64, ticks: i64, size: u32) {
        let i = self.pos(id);
        let o = self.orders[i];
        if size == 0 {
            self.orders.remove(i);
            return;
        }
        if o.depleted {
            self.orders.remove(i);
            self.orders.push(SOrder {
                id,
                side: o.side,
                ticks,
                size,
                snapshot: false,
                cum_fill: 0,
                depleted: false,
            });
            return;
        }
        if ticks == o.ticks && size <= o.size {
            self.orders[i].size = size;
            // Book `on_book_change` clears fill progress on non-depleting mutate.
            self.orders[i].cum_fill = 0;
            self.orders[i].depleted = false;
        } else {
            self.orders.remove(i);
            self.orders.push(SOrder {
                id,
                side: o.side,
                ticks,
                size,
                snapshot: false,
                cum_fill: 0,
                depleted: false,
            });
        }
    }

    fn pos(&self, id: u64) -> usize {
        self.orders
            .iter()
            .position(|o| o.id == id)
            .unwrap_or_else(|| panic!("shadow missing order {id}"))
    }

    pub fn queue(&self, id: u64) -> QueuePosition {
        let i = self.pos(id);
        let o = self.orders[i];
        let mut orders_ahead = 0u32;
        let mut contracts_ahead = 0u64;
        for e in &self.orders[..i] {
            if e.side == o.side && e.ticks == o.ticks {
                orders_ahead += 1;
                contracts_ahead += u64::from(e.size);
            }
        }
        QueuePosition {
            side: o.side,
            price: px(o.ticks),
            size: o.size,
            orders_ahead,
            contracts_ahead,
            rank: orders_ahead + 1,
        }
    }

    pub fn live_ids(&self) -> Vec<u64> {
        self.orders.iter().map(|o| o.id).collect()
    }
}

// ── BOOK-bytes oracle (checkpoint layout, not query.rs) ─────────────────────

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BookFifo {
    pub tick: i64,
    /// Per side: levels in serialize order, each a head→tail FIFO of (id, size).
    pub bids: Vec<(i64, Vec<(u64, u32)>)>,
    pub asks: Vec<(i64, Vec<(u64, u32)>)>,
}

impl BookFifo {
    pub fn parse(bytes: &[u8]) -> Self {
        let mut r = Cursor::new(bytes);
        let version = r.u16();
        assert_eq!(
            version, BOOK_SECTION_VERSION,
            "BOOK-bytes oracle: unexpected section version {version}"
        );
        let tick = r.i64();
        assert!(tick > 0);
        let _now = r.u64();
        let _last_seq = r.opt_u64();
        let _gap_pending = r.bool();
        let _gap_flag = r.u8();
        let _gap_exp = r.u64();
        let _gap_obs = r.u64();
        let _unknown = r.u64();
        let _trade_flag = r.u8();
        let _trade_px = r.i64();
        let _trade_sz = r.u32();
        let _trade_agg = r.u8();
        let bids = read_side_fifo(&mut r);
        let asks = read_side_fifo(&mut r);
        assert!(r.done(), "BOOK-bytes oracle: trailing bytes");
        Self { tick, bids, asks }
    }

    pub fn queue(&self, id: u64) -> Option<QueuePosition> {
        for (side, levels) in [(Side::Bid, &self.bids), (Side::Ask, &self.asks)] {
            for &(price_ticks, ref fifo) in levels {
                let mut contracts_ahead = 0u64;
                for (orders_ahead, &(oid, size)) in fifo.iter().enumerate() {
                    if oid == id {
                        let orders_ahead = orders_ahead as u32;
                        return Some(QueuePosition {
                            side,
                            price: Price(price_ticks * self.tick),
                            size,
                            orders_ahead,
                            contracts_ahead,
                            rank: orders_ahead + 1,
                        });
                    }
                    contracts_ahead += u64::from(size);
                }
            }
        }
        None
    }

    pub fn live_ids(&self) -> Vec<u64> {
        let mut ids = Vec::new();
        for levels in [&self.bids, &self.asks] {
            for (_, fifo) in levels {
                for &(id, _) in fifo {
                    ids.push(id);
                }
            }
        }
        ids
    }
}

// ── Triple-agreement helpers (gate + oracles only) ───────────────────────────

pub fn assert_queue_triple(book: &Book, shadow: &Shadow, id: u64) {
    let from_book = book
        .queue_position(OrderId(id))
        .unwrap_or_else(|| panic!("book missing order {id}"));
    let from_shadow = shadow.queue(id);
    assert_eq!(
        from_book, from_shadow,
        "book vs shadow queue for id {id}: book={from_book:?} shadow={from_shadow:?}"
    );
    let from_bytes = BookFifo::parse(&book.serialize_book())
        .queue(id)
        .unwrap_or_else(|| panic!("BOOK bytes missing order {id}"));
    assert_eq!(
        from_book, from_bytes,
        "book vs BOOK-bytes queue for id {id}: book={from_book:?} bytes={from_bytes:?}"
    );
}

pub fn assert_all_queues(book: &Book, shadow: &Shadow) {
    book.check_invariants();
    // Live id *set* only: for_each_* is a link walk (ranks still come from BOOK bytes / shadow).
    let mut book_ids = Vec::new();
    for side in [Side::Bid, Side::Ask] {
        book.for_each_level(side, |price, _| {
            book.for_each_order_at(side, price, |id, _| book_ids.push(id.0));
        });
    }
    let mut shadow_ids = shadow.live_ids();
    let mut bytes_ids = BookFifo::parse(&book.serialize_book()).live_ids();
    book_ids.sort_unstable();
    shadow_ids.sort_unstable();
    bytes_ids.sort_unstable();
    assert_eq!(book_ids, shadow_ids, "live id set: book vs shadow");
    assert_eq!(book_ids, bytes_ids, "live id set: book vs BOOK bytes");
    for id in &shadow_ids {
        assert_queue_triple(book, shadow, *id);
    }
}

pub fn assert_absent(book: &Book, id: u64) {
    assert!(
        book.queue_position(OrderId(id)).is_none(),
        "order {id} should not rest"
    );
    assert!(
        BookFifo::parse(&book.serialize_book()).queue(id).is_none(),
        "order {id} still in BOOK bytes"
    );
}

struct Cursor<'a> {
    bytes: &'a [u8],
    pos: usize,
}

impl<'a> Cursor<'a> {
    fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, pos: 0 }
    }
    fn take(&mut self, n: usize) -> &'a [u8] {
        let end = self.pos + n;
        assert!(end <= self.bytes.len(), "BOOK-bytes oracle: truncated");
        let out = &self.bytes[self.pos..end];
        self.pos = end;
        out
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
    fn bool(&mut self) -> bool {
        match self.u8() {
            0 => false,
            1 => true,
            b => panic!("BOOK-bytes oracle: bad bool {b}"),
        }
    }
    fn opt_u64(&mut self) -> Option<u64> {
        let p = self.bool();
        let v = self.u64();
        if p { Some(v) } else { None }
    }
    fn opt_i64(&mut self) -> Option<i64> {
        let p = self.bool();
        let v = self.i64();
        if p { Some(v) } else { None }
    }
    fn done(&self) -> bool {
        self.pos == self.bytes.len()
    }
}

fn read_side_fifo(r: &mut Cursor<'_>) -> Vec<(i64, Vec<(u64, u32)>)> {
    let initialised = r.bool();
    let _base = r.i64();
    let _best = r.opt_i64();
    let level_count = r.u32() as usize;
    if !initialised {
        assert_eq!(level_count, 0);
        return Vec::new();
    }
    let mut levels = Vec::with_capacity(level_count);
    for _ in 0..level_count {
        let price = r.i64();
        let order_count = r.u32() as usize;
        assert!(order_count > 0, "BOOK-bytes oracle: empty level");
        let mut fifo = Vec::with_capacity(order_count);
        for _ in 0..order_count {
            let id = r.u64();
            let size = r.u32();
            let _ts = r.u64();
            let _epoch = r.u32();
            let _origin = r.u8();
            assert!(id != 0 && size > 0);
            fifo.push((id, size));
        }
        levels.push((price, fifo));
    }
    levels
}
