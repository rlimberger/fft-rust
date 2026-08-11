//! The book proper: canonical-event application with exact CME semantics.

use crate::flow::TradedAtInsideTicks;
use crate::level::{NIL, Order, OrderOrigin};
use crate::refresh::RefreshTracker;
use crate::side::{SideBook, link_snapshot, link_tail};
use fft_core::{CanonicalEvent, EventKind, Price, Side};
use slab::Slab;
use std::collections::HashMap;

/// Events between dead-level / expired-tombstone sweeps.
pub(crate) const GC_INTERVAL: u32 = 4096;

/// L3 MBO book for one instrument. See the crate docs for the full contract.
#[derive(Debug)]
pub struct Book {
    /// Tick size in 1e-9 price units; all internal prices are ticks.
    pub(crate) tick: i64,
    pub(crate) bids: SideBook,
    pub(crate) asks: SideBook,
    pub(crate) orders: Slab<Order>,
    /// order id → slab slot.
    pub(crate) index: HashMap<u64, u32>,
    pub(crate) refresh: RefreshTracker,
    pub(crate) tai: TradedAtInsideTicks,
    /// (price ticks, size, aggressor).
    pub(crate) last_trade: Option<(i64, u32, Side)>,
    /// Latest event time seen, ns.
    pub(crate) now: u64,
    pub(crate) since_gc: u32,
    /// Last applied source sequence (`None` until sequenced, and after a gap
    /// until re-anchored).
    pub(crate) last_seq: Option<u64>,
    /// A Gap event was applied and no sequenced event has followed yet.
    pub(crate) gap_pending: bool,
    /// `(expected, observed)` of the most recent Gap event.
    pub(crate) last_gap: Option<(u64, u64)>,
    /// Cancel/Modify/Fill events referencing unknown order ids (normal after a
    /// mid-stream join, a Clear, or a gap; the engine asserts 0 on full logs).
    pub(crate) unknown_refs: u64,
    /// Snapshot-flagged Clear records ignored (FFTLOG-V2 §4 block framing).
    pub(crate) snapshot_clears: u64,
    /// Gap-tainted Cancel whose size/price/side disagreed with retained depth
    /// (runtime diagnostic; not seek-relevant — restarts at 0 on restore).
    pub(crate) gap_desync_cancels: u64,
    /// Gap-tainted Modify whose side/price disagreed with retained depth
    /// (runtime diagnostic; not seek-relevant — restarts at 0 on restore).
    pub(crate) gap_desync_modifies: u64,
}

impl Book {
    /// `min_price_increment` in 1e-9 units (`InstrumentMeta::min_price_increment`).
    pub fn new(min_price_increment: Price) -> Self {
        assert!(
            min_price_increment.0 > 0,
            "fft-book: non-positive tick size {}",
            min_price_increment.0
        );
        Self::empty(min_price_increment.0)
    }

    pub(crate) fn empty(tick: i64) -> Self {
        Self {
            tick,
            bids: SideBook::new(true),
            asks: SideBook::new(false),
            orders: Slab::new(),
            index: HashMap::new(),
            refresh: RefreshTracker::default(),
            tai: TradedAtInsideTicks::default(),
            last_trade: None,
            now: 0,
            since_gc: 0,
            last_seq: None,
            gap_pending: false,
            last_gap: None,
            unknown_refs: 0,
            snapshot_clears: 0,
            gap_desync_cancels: 0,
            gap_desync_modifies: 0,
        }
    }

    pub(crate) fn to_ticks(&self, p: Price) -> i64 {
        assert!(
            p.0 % self.tick == 0,
            "fft-book: price {} not aligned to tick {}",
            p.0,
            self.tick
        );
        p.0 / self.tick
    }

    pub(crate) fn price_of(&self, t: i64) -> Price {
        Price(t * self.tick)
    }

    /// Apply one canonical event. Panics (fail loudly) on malformed events,
    /// feed-contract violations, and unexplained sequence regressions.
    pub fn apply(&mut self, ev: &CanonicalEvent) {
        if ev.is_snapshot() {
            // FFTLOG-V2 §4: the block's leading Clear is venue reset framing —
            // merge semantics supersede clear-and-rebuild; ignore it loudly.
            if ev.kind == EventKind::Clear {
                self.snapshot_clears += 1;
                return;
            }
            assert_eq!(
                ev.kind,
                EventKind::Add,
                "fft-book: snapshot-flagged event must be Add or Clear, got {:?}: {ev:?}",
                ev.kind
            );
            self.do_snapshot_add(ev);
            return;
        }
        let ts = ev.ts.0;
        if ts > self.now {
            self.now = ts;
        }
        self.track_seq(ev);
        match ev.kind {
            EventKind::Add => self.do_add(ev, ts),
            EventKind::Cancel => self.do_cancel(ev, ts),
            EventKind::Modify => self.do_modify(ev, ts),
            EventKind::Trade => self.do_trade(ev),
            EventKind::Fill => self.do_fill(ev, ts),
            EventKind::Clear => self.do_clear(),
            EventKind::Status => {}
            EventKind::Gap => self.do_gap(ev),
        }
        // Tombstone lifetime is pure event-time ([`REFRESH_WINDOW_NS`]). GC must
        // run every apply so seek (restore resets `since_gc`) and forward replay
        // drop the same expired candidates — otherwise REFRESH section bytes
        // diverge while BOOK/FLOW stay bit-identical (M2 gate).
        self.refresh.gc(self.now);
        self.since_gc += 1;
        if self.since_gc >= GC_INTERVAL {
            self.since_gc = 0;
            let now = self.now;
            self.bids.gc(now);
            self.asks.gc(now);
        }
    }

    /// Sequence accounting. `seq == 0` marks unsequenced events (synthetic
    /// fixtures). Forward skips are legitimate — the source channel carries
    /// other instruments — but a regression without an interposed Gap event is
    /// an unexplained discontinuity and panics.
    fn track_seq(&mut self, ev: &CanonicalEvent) {
        if ev.kind == EventKind::Gap {
            return;
        }
        let seq = u64::from(ev.seq.0);
        if seq == 0 {
            return;
        }
        if let Some(last) = self.last_seq {
            assert!(
                seq >= last,
                "fft-book: seq regression {last} -> {seq} without an interposed Gap event \
                 ({:?} ts {})",
                ev.kind,
                ev.ts.0
            );
        }
        self.last_seq = Some(seq);
        self.gap_pending = false;
    }

    fn do_add(&mut self, ev: &CanonicalEvent, ts: u64) {
        assert!(
            ev.side == Side::Bid || ev.side == Side::Ask,
            "fft-book: Add without side: {ev:?}"
        );
        assert!(ev.size > 0, "fft-book: Add with size 0: {ev:?}");
        assert!(
            !self.index.contains_key(&ev.order_id.0),
            "fft-book: duplicate Add for live order {}: {ev:?}",
            ev.order_id.0
        );
        let price = self.to_ticks(ev.price);
        self.insert_order(ev.order_id.0, ev.side, price, ev.size, ts);
    }

    fn do_snapshot_add(&mut self, ev: &CanonicalEvent) {
        assert!(
            ev.side == Side::Bid || ev.side == Side::Ask,
            "fft-book: snapshot Add without side: {ev:?}"
        );
        assert!(ev.size > 0, "fft-book: snapshot Add with size 0: {ev:?}");
        let price = self.to_ticks(ev.price);
        let id = ev.order_id.0;

        if let Some(&slot) = self.index.get(&id) {
            let resting = &self.orders[slot as usize];
            assert_eq!(
                ev.side, resting.side,
                "fft-book: snapshot Add side mismatch for known order {id}: snapshot {:?}, resting {:?}",
                ev.side, resting.side
            );
            assert_eq!(
                price, resting.price,
                "fft-book: snapshot Add price mismatch for known order {id}: snapshot {price} ticks, resting {} ticks",
                resting.price
            );
            assert_eq!(
                ev.size, resting.size,
                "fft-book: snapshot Add size mismatch for known order {id}: snapshot {}, resting {}",
                ev.size, resting.size
            );
            return;
        }

        self.refresh.on_snapshot_loaded(id);
        let order = Order {
            id,
            price,
            side: ev.side,
            size: ev.size,
            ts: ev.ts.0,
            epoch: self.refresh.gap_epoch,
            origin: OrderOrigin::Snapshot,
            prev: NIL,
            next: NIL,
        };
        let slot =
            u32::try_from(self.orders.insert(order)).expect("fft-book: order slot exceeds u32");
        self.index.insert(id, slot);
        let sb = if ev.side == Side::Bid {
            &mut self.bids
        } else {
            &mut self.asks
        };
        sb.prepare_for(price, self.now);
        link_snapshot(sb, &mut self.orders, slot);
        sb.note_add(price);
    }

    /// Shared placement path for Add and iceberg restores: routes through the
    /// refresh tracker, then links at the back of the level FIFO.
    pub(crate) fn insert_order(&mut self, id: u64, side: Side, price: i64, size: u32, ts: u64) {
        self.refresh.on_placed(id, side, price, size, ts);
        let o = Order {
            id,
            price,
            side,
            size,
            ts,
            epoch: self.refresh.gap_epoch,
            origin: OrderOrigin::Live,
            prev: NIL,
            next: NIL,
        };
        let slot = u32::try_from(self.orders.insert(o)).expect("fft-book: order slot exceeds u32");
        self.index.insert(id, slot);
        let sb = if side == Side::Bid {
            &mut self.bids
        } else {
            &mut self.asks
        };
        sb.prepare_for(price, self.now);
        link_tail(sb, &mut self.orders, slot);
        sb.level_entry(price).flow.record_added(ts, size);
        sb.note_add(price);
    }

    /// Fill is the sole execution-derived input for tape, cB/cA, five-second
    /// traded flow, and cumulative refresh depletion. It never mutates depth;
    /// the companion Cancel/Modify carries all displayed-book truth.
    fn do_fill(&mut self, ev: &CanonicalEvent, ts: u64) {
        let fill = ev.size;
        assert!(
            fill > 0,
            "fft-book: Fill with size 0 (id {})",
            ev.order_id.0
        );
        let id = ev.order_id.0;
        let order = self
            .index
            .get(&id)
            .map(|&slot| self.orders[slot as usize].clone());
        if let Some(o) = &order {
            assert!(
                ev.side == Side::None || ev.side == o.side,
                "fft-book: Fill side {:?} != resting side {:?} (id {id})",
                ev.side,
                o.side
            );
        }
        let side = match (ev.side, &order) {
            (Side::None, Some(o)) => o.side,
            (side, _) => side,
        };
        let price = self.to_ticks(ev.price);
        let aggressor = match side {
            Side::Bid => Side::Ask,
            Side::Ask => Side::Bid,
            Side::None => Side::None,
        };
        self.last_trade = Some((price, fill, aggressor));
        self.tai.on_fill(price, fill, side);
        let flow_side = match side {
            Side::Bid => Some(&mut self.bids),
            Side::Ask => Some(&mut self.asks),
            Side::None => None,
        };
        if let Some(flow_side) = flow_side {
            flow_side.prepare_for(price, self.now);
            flow_side.level_entry(price).flow.record_traded(ts, fill);
        }

        let Some(o) = order else {
            self.unknown_refs += 1;
            return;
        };
        if price != o.price {
            self.refresh.note_fill_off_display();
        }
        self.refresh.on_fill(id, o.size, fill, ts);
    }

    /// Canonical Trade is retained for sequencing/coverage but deliberately
    /// inert here: paired Fill is the single execution source, avoiding double count.
    fn do_trade(&mut self, _ev: &CanonicalEvent) {}

    /// Full book reset. Tape state (last trade, cB/cA) and session-cumulative
    /// refresh aggregates describe history and survive.
    fn do_clear(&mut self) {
        self.bids = SideBook::new(true);
        self.asks = SideBook::new(false);
        self.orders.clear();
        self.index.clear();
        self.refresh.on_clear();
    }

    /// A source sequence gap: every in-flight classification becomes
    /// Unavailable (epoch bump) and sequence accounting re-anchors on the next
    /// sequenced event.
    fn do_gap(&mut self, ev: &CanonicalEvent) {
        let (expected, observed) = ev.gap_seqs();
        self.last_gap = Some((expected, observed));
        self.gap_pending = true;
        self.last_seq = None;
        self.refresh.on_gap();
    }
}
