//! Read-side surface: level views, queue-position and refresh queries, gap
//! state accessors, and the invariant checker.

use crate::book::Book;
use crate::level::{Level, NIL, OrderOrigin, view_of};
use crate::{
    InsideTraded, LastTrade, LevelView, Overlap, PriceRefreshAgg, QueuePosition, RefreshState,
};
use fft_core::{OrderId, Price, Side, Ts};
use std::collections::HashMap;

impl Book {
    pub fn tick_size(&self) -> Price {
        Price(self.tick)
    }

    pub fn best_bid(&self) -> Option<Price> {
        self.bids.best.map(|t| self.price_of(t))
    }

    pub fn best_ask(&self) -> Option<Price> {
        self.asks.best.map(|t| self.price_of(t))
    }

    /// Bid/ask price relation from the two cached bests. `None` when the book
    /// is normal (`bb < ba`) or one-sided; locked/crossed are wire-legal
    /// transient (and post-close) states — diagnostic only, never an invariant.
    pub fn book_overlap(&self) -> Option<Overlap> {
        match (self.bids.best, self.asks.best) {
            (Some(bb), Some(ba)) if bb == ba => Some(Overlap::Locked(self.price_of(bb))),
            (Some(bb), Some(ba)) if bb > ba => Some(Overlap::Crossed {
                bid: self.price_of(bb),
                ask: self.price_of(ba),
            }),
            _ => None,
        }
    }

    pub fn last_trade(&self) -> Option<LastTrade> {
        self.last_trade.map(|(t, size, aggressor)| LastTrade {
            price: self.price_of(t),
            size,
            aggressor,
        })
    }

    /// Grady cB/cA counters, fed solely by Fill events.
    pub fn traded_at_inside(&self) -> InsideTraded {
        InsideTraded {
            bid_price: self.tai.bid_price.map(|t| self.price_of(t)),
            bid_vol: self.tai.bid_vol,
            ask_price: self.tai.ask_price.map(|t| self.price_of(t)),
            ask_vol: self.tai.ask_vol,
        }
    }

    fn side_book(&self, side: Side) -> &crate::side::SideBook {
        match side {
            Side::Bid => &self.bids,
            Side::Ask => &self.asks,
            Side::None => panic!("fft-book: query with Side::None"),
        }
    }

    /// View of one price level (zeros if untracked).
    pub fn level(&self, side: Side, price: Price) -> LevelView {
        let t = self.to_ticks(price);
        match self.side_book(side).level_ref(t) {
            Some(l) => view_of(l, self.now),
            None => LevelView::default(),
        }
    }

    /// Visit populated levels best-first.
    pub fn for_each_level(&self, side: Side, mut f: impl FnMut(Price, LevelView)) {
        let now = self.now;
        self.side_book(side).try_for_each_populated(|t, l| {
            f(self.price_of(t), view_of(l, now));
            true
        });
    }

    /// Visit the resting orders of one level strictly head-to-tail FIFO.
    pub fn for_each_order_at(&self, side: Side, price: Price, mut f: impl FnMut(OrderId, u32)) {
        let t = self.to_ticks(price);
        let Some(level) = self.side_book(side).level_ref(t) else {
            return;
        };
        let mut cur = level.head;
        while cur != NIL {
            let o = &self.orders[cur as usize];
            f(OrderId(o.id), o.size);
            cur = o.next;
        }
    }

    /// Exact queue standing of a resting order: contracts ahead, orders ahead,
    /// FIFO rank at its price level (PRD §4 claim 3).
    pub fn queue_position(&self, id: OrderId) -> Option<QueuePosition> {
        let &slot = self.index.get(&id.0)?;
        let target = &self.orders[slot as usize];
        let level = self
            .side_book(target.side)
            .level_ref(target.price)
            .expect("fft-book invariant: level missing for live order");
        let mut orders_ahead = 0u32;
        let mut contracts_ahead = 0u64;
        let mut cur = level.head;
        while cur != NIL && cur != slot {
            let o = &self.orders[cur as usize];
            orders_ahead += 1;
            contracts_ahead += u64::from(o.size);
            cur = o.next;
        }
        assert_eq!(
            cur, slot,
            "fft-book invariant: order {} not on its level list",
            id.0
        );
        Some(QueuePosition {
            side: target.side,
            price: self.price_of(target.price),
            size: target.size,
            orders_ahead,
            contracts_ahead,
            rank: orders_ahead + 1,
        })
    }

    /// Native-refresh classification of a resting order (PRD §4 claim 4).
    pub fn refresh_state(&self, id: OrderId) -> RefreshState {
        let Some(&slot) = self.index.get(&id.0) else {
            return RefreshState::NotResting;
        };
        self.refresh
            .state_for(id.0, self.orders[slot as usize].epoch)
    }

    /// Session-cumulative refresh aggregate at one price on one side.
    pub fn refresh_at(&self, side: Side, price: Price) -> PriceRefreshAgg {
        assert!(
            side == Side::Bid || side == Side::Ask,
            "fft-book: refresh_at with Side::None"
        );
        self.refresh.agg_at(side, self.to_ticks(price))
    }

    pub fn live_order_count(&self) -> usize {
        self.index.len()
    }

    pub fn populated_levels(&self) -> usize {
        self.bids.populated + self.asks.populated
    }

    pub fn last_event_ts(&self) -> Ts {
        Ts(self.now)
    }

    /// Last applied source sequence (`None` until sequenced, and after a gap
    /// until re-anchored).
    pub fn last_seq(&self) -> Option<u64> {
        self.last_seq
    }

    /// A Gap event was applied and no sequenced event has followed yet.
    pub fn gap_pending(&self) -> bool {
        self.gap_pending
    }

    /// Number of sequence gaps applied so far.
    pub fn gaps_seen(&self) -> u32 {
        self.refresh.gap_epoch
    }

    /// `(expected, observed)` of the most recent Gap event.
    pub fn last_gap(&self) -> Option<(u64, u64)> {
        self.last_gap
    }

    /// Cancel/Modify/Fill events that referenced unknown order ids, including
    /// sideless Fills that could not be attributed to book flow.
    pub fn unknown_ref_events(&self) -> u64 {
        self.unknown_refs
    }

    /// Fill executions whose price differed from the order's displayed price.
    pub fn fills_off_display(&self) -> u64 {
        self.refresh.fills_off_display
    }

    /// Snapshot-flagged Clear records ignored as block framing (FFTLOG-V2 §4).
    pub fn snapshot_clears(&self) -> u64 {
        self.snapshot_clears
    }

    /// Add events that replaced a gap-tainted retained order with the same id
    /// (venue values re-applied via remove+reinsert).
    pub fn gap_desync_adds(&self) -> u64 {
        self.gap_desync_adds
    }

    /// Cancel events on gap-tainted ids whose size/price/side disagreed with
    /// retained depth (venue-wins full removal; see FFTLOG-V2 §4).
    pub fn gap_desync_cancels(&self) -> u64 {
        self.gap_desync_cancels
    }

    /// Modify events on gap-tainted ids whose side/price disagreed with
    /// retained depth (venue values re-applied via remove+reinsert).
    pub fn gap_desync_modifies(&self) -> u64 {
        self.gap_desync_modifies
    }

    /// Fill events on gap-tainted retained orders that would otherwise attach
    /// stale depletion/off-display evidence (sided venue mismatch, or sideless
    /// Fill resolved via order-id lookup). Tape/flow attribution still runs.
    pub fn gap_desync_fills(&self) -> u64 {
        self.gap_desync_fills
    }

    fn walk_level(
        &self,
        side: Side,
        price: i64,
        level: &Level,
        seen: &mut HashMap<u64, (Side, i64)>,
    ) {
        let mut total = 0u64;
        let mut count = 0u32;
        let mut cur = level.head;
        let mut prev = NIL;
        let mut snapshot_tail = NIL;
        let mut reached_live = false;
        while cur != NIL {
            let o = &self.orders[cur as usize];
            assert!(
                o.price == price,
                "fft-book invariant: order {} price {} on level {price}",
                o.id,
                o.price
            );
            assert!(
                o.side == side,
                "fft-book invariant: order {} on wrong side {side:?}",
                o.id
            );
            assert!(
                o.size > 0,
                "fft-book invariant: live order {} with size 0",
                o.id
            );
            assert!(
                o.prev == prev,
                "fft-book invariant: broken prev link at order {}",
                o.id
            );
            assert!(
                self.index.get(&o.id) == Some(&cur),
                "fft-book invariant: order {} not indexed to its slot",
                o.id
            );
            assert!(
                seen.insert(o.id, (side, price)).is_none(),
                "fft-book invariant: order {} linked twice",
                o.id
            );
            assert!(
                o.epoch <= self.refresh.gap_epoch,
                "fft-book invariant: order {} epoch {} beyond gap epoch {}",
                o.id,
                o.epoch,
                self.refresh.gap_epoch
            );
            match o.origin {
                OrderOrigin::Snapshot => {
                    assert!(
                        !reached_live,
                        "fft-book invariant: snapshot order {} follows live order at {side:?} {price}",
                        o.id
                    );
                    snapshot_tail = cur;
                }
                OrderOrigin::Live => reached_live = true,
            }
            total += u64::from(o.size);
            count += 1;
            prev = cur;
            cur = o.next;
            assert!(
                count as usize <= self.index.len(),
                "fft-book invariant: cyclic FIFO list at {side:?} {price}"
            );
        }
        assert!(
            prev == level.tail,
            "fft-book invariant: stale tail pointer at {side:?} {price}"
        );
        assert_eq!(
            snapshot_tail, level.snapshot_tail,
            "fft-book invariant: snapshot prefix tail mismatch at {side:?} {price}"
        );
        assert!(
            total == level.total_size,
            "fft-book invariant: total_size {} != walked {total} at {side:?} {price}",
            level.total_size
        );
        assert!(
            count == level.order_count,
            "fft-book invariant: order_count {} != walked {count} at {side:?} {price}",
            level.order_count
        );
    }

    /// Full structural audit. Panics with context on any violation — an
    /// invariant failure is a bug, never a log line.
    pub fn check_invariants(&self) {
        let mut seen: HashMap<u64, (Side, i64)> = HashMap::with_capacity(self.index.len());
        for side in [Side::Bid, Side::Ask] {
            let sb = self.side_book(side);
            let mut populated = 0usize;
            let mut prev_price: Option<i64> = None;
            sb.try_for_each_populated(|price, level| {
                populated += 1;
                if let Some(pp) = prev_price {
                    assert!(
                        sb.better(pp, price),
                        "fft-book invariant: levels not best-first ({side:?}: {pp} then {price})"
                    );
                }
                prev_price = Some(price);
                self.walk_level(side, price, level, &mut seen);
                true
            });
            assert!(
                populated == sb.populated,
                "fft-book invariant: populated counter {} != walked {populated} on {side:?}",
                sb.populated
            );
            assert!(
                sb.best == sb.scan_best(),
                "fft-book invariant: cached best {:?} != scanned {:?} on {side:?}",
                sb.best,
                sb.scan_best()
            );
            for &p in sb.far.keys() {
                assert!(
                    p < sb.base_tick || p >= sb.hi_bound(),
                    "fft-book invariant: far level {p} inside window [{}, {}) on {side:?}",
                    sb.base_tick,
                    sb.hi_bound()
                );
            }
        }
        assert!(
            seen.len() == self.index.len(),
            "fft-book invariant: {} linked orders != {} indexed",
            seen.len(),
            self.index.len()
        );
        // Bid/ask overlap is not structural: locked and crossed books are
        // wire-legal (intra-event-group and post-close). See `book_overlap`.
        for id in self.refresh.tombstones.keys() {
            assert!(
                !self.index.contains_key(id),
                "fft-book invariant: tombstoned order {id} still live"
            );
        }
        for id in self.refresh.live.keys() {
            assert!(
                self.index.contains_key(id),
                "fft-book invariant: refresh entry for dead order {id}"
            );
        }
        for (id, progress) in &self.refresh.fills {
            let &slot = self
                .index
                .get(id)
                .unwrap_or_else(|| panic!("fft-book invariant: Fill progress for dead order {id}"));
            let order = &self.orders[slot as usize];
            assert!(
                progress.qty > 0,
                "fft-book invariant: zero Fill progress for {id}"
            );
            assert!(
                progress.epoch <= self.refresh.gap_epoch,
                "fft-book invariant: Fill epoch {} beyond gap epoch {} for {id}",
                progress.epoch,
                self.refresh.gap_epoch
            );
            match progress.depleted_ts {
                Some(ts) => {
                    assert!(
                        progress.qty >= u64::from(order.size),
                        "fft-book invariant: depleted Fill progress below displayed size for {id}"
                    );
                    assert!(
                        ts <= self.now,
                        "fft-book invariant: depletion time beyond book time for {id}"
                    );
                }
                None => assert!(
                    progress.qty < u64::from(order.size),
                    "fft-book invariant: partial Fill progress reached displayed size for {id}"
                ),
            }
        }
    }
}
