//! One side of the book: sliding dense window of levels + `BTreeMap` far map.

use crate::level::{Level, NIL, Order, OrderOrigin};
use slab::Slab;
use std::collections::BTreeMap;

pub(crate) const WINDOW_TICKS: usize = 512;
pub(crate) const WINDOW_MARGIN: usize = 96;

#[derive(Debug)]
pub(crate) struct SideBook {
    pub is_bid: bool,
    /// Dense levels for `base_tick .. base_tick + WINDOW_TICKS`.
    pub window: Vec<Level>,
    pub base_tick: i64,
    /// Levels outside the window (only kept while populated or flow-fresh).
    pub far: BTreeMap<i64, Level>,
    pub best: Option<i64>,
    /// Count of levels with at least one resting order.
    pub populated: usize,
    /// Inclusive occupancy bounds of touched window slots (`lo > hi` = none).
    occ_lo: usize,
    occ_hi: usize,
    pub initialised: bool,
}

impl SideBook {
    pub fn new(is_bid: bool) -> Self {
        Self {
            is_bid,
            window: vec![Level::default(); WINDOW_TICKS],
            base_tick: 0,
            far: BTreeMap::new(),
            best: None,
            populated: 0,
            occ_lo: WINDOW_TICKS,
            occ_hi: 0,
            initialised: false,
        }
    }

    /// True iff `a` is a better (more aggressive) price than `b` on this side.
    pub fn better(&self, a: i64, b: i64) -> bool {
        if self.is_bid { a > b } else { a < b }
    }

    pub fn hi_bound(&self) -> i64 {
        self.base_tick + self.window.len() as i64
    }

    fn idx(&self, price: i64) -> Option<usize> {
        let off = price - self.base_tick;
        if off >= 0 && (off as usize) < self.window.len() {
            Some(off as usize)
        } else {
            None
        }
    }

    fn note_occupied(&mut self, i: usize) {
        if i < self.occ_lo {
            self.occ_lo = i;
        }
        if i > self.occ_hi {
            self.occ_hi = i;
        }
    }

    pub fn level_ref(&self, price: i64) -> Option<&Level> {
        if let Some(i) = self.idx(price) {
            return Some(&self.window[i]);
        }
        self.far.get(&price)
    }

    pub fn level_mut(&mut self, price: i64) -> Option<&mut Level> {
        if let Some(i) = self.idx(price) {
            self.note_occupied(i);
            return Some(&mut self.window[i]);
        }
        self.far.get_mut(&price)
    }

    pub fn level_entry(&mut self, price: i64) -> &mut Level {
        if let Some(i) = self.idx(price) {
            self.note_occupied(i);
            return &mut self.window[i];
        }
        self.far.entry(price).or_default()
    }

    /// Ensure the window is placed for activity at `price`, recentring on the
    /// current best when it drifts within `WINDOW_MARGIN` of an edge.
    pub fn prepare_for(&mut self, price: i64, now: u64) {
        let len = self.window.len() as i64;
        if !self.initialised {
            self.initialised = true;
            self.base_tick = price - len / 2;
            return;
        }
        let anchor = self.best.unwrap_or(price);
        let off = anchor - self.base_tick;
        if off >= WINDOW_MARGIN as i64 && off < len - WINDOW_MARGIN as i64 {
            return;
        }
        self.recentre(anchor - len / 2, now);
    }

    fn evict(&mut self, i: usize, old_base: i64, now: u64) {
        let l = std::mem::take(&mut self.window[i]);
        if l.order_count > 0 || (!l.flow.untouched() && !l.flow.stale(now)) {
            self.far.insert(old_base + i as i64, l);
        }
    }

    fn recentre(&mut self, new_base: i64, now: u64) {
        let shift = new_base - self.base_tick;
        if shift == 0 {
            return;
        }
        let len = self.window.len();
        let old_base = self.base_tick;
        let s = shift.unsigned_abs() as usize;

        if s >= len {
            for i in 0..len {
                self.evict(i, old_base, now);
            }
        } else if shift > 0 {
            for i in 0..s {
                self.evict(i, old_base, now);
            }
            self.window.rotate_left(s);
            for l in &mut self.window[(len - s)..] {
                *l = Level::default();
            }
        } else {
            for i in (len - s)..len {
                self.evict(i, old_base, now);
            }
            self.window.rotate_right(s);
            for l in &mut self.window[..s] {
                *l = Level::default();
            }
        }
        self.base_tick = new_base;

        let hi = new_base + len as i64;
        loop {
            let key = {
                let mut it = self.far.range(new_base..);
                match it.next() {
                    Some((&p, _)) if p < hi => p,
                    _ => break,
                }
            };
            let l = self.far.remove(&key).unwrap();
            self.window[(key - new_base) as usize] = l;
        }
        self.recompute_occupied();
    }

    fn recompute_occupied(&mut self) {
        let mut lo = self.window.len();
        let mut hi = 0usize;
        for (i, l) in self.window.iter().enumerate() {
            if l.order_count > 0 || !l.flow.untouched() {
                if i < lo {
                    lo = i;
                }
                hi = i;
            }
        }
        self.occ_lo = lo;
        self.occ_hi = hi;
    }

    pub fn gc(&mut self, now: u64) {
        if self.occ_lo <= self.occ_hi {
            let hi = self.occ_hi.min(self.window.len() - 1);
            let mut lo = self.window.len();
            let mut new_hi = 0usize;
            for i in self.occ_lo..=hi {
                if self.window[i].dead(now) {
                    self.window[i] = Level::default();
                } else {
                    if i < lo {
                        lo = i;
                    }
                    new_hi = i;
                }
            }
            self.occ_lo = lo;
            self.occ_hi = new_hi;
        }
        self.far.retain(|_, l| !l.dead(now));
    }

    pub fn note_add(&mut self, price: i64) {
        match self.best {
            None => self.best = Some(price),
            Some(b) if self.better(price, b) => self.best = Some(price),
            _ => {}
        }
    }

    pub fn note_remove(&mut self, price: i64) {
        if self.best != Some(price) {
            return;
        }
        if let Some(l) = self.level_ref(price)
            && l.order_count > 0
        {
            return;
        }
        self.best = self.next_best_from(price);
    }

    /// Visit every tracked level best-first (far beyond the window on the best
    /// side, then the occupied window span, then far on the worse side).
    /// `f` returns false to stop early.
    fn traverse(&self, mut f: impl FnMut(i64, &Level) -> bool) {
        let hi = self.hi_bound();
        if self.is_bid {
            for (&p, l) in self.far.range(hi..).rev() {
                if !f(p, l) {
                    return;
                }
            }
            if self.occ_lo <= self.occ_hi {
                let top = self.occ_hi.min(self.window.len() - 1);
                for i in (self.occ_lo..=top).rev() {
                    if !f(self.base_tick + i as i64, &self.window[i]) {
                        return;
                    }
                }
            }
            for (&p, l) in self.far.range(..self.base_tick).rev() {
                if !f(p, l) {
                    return;
                }
            }
        } else {
            for (&p, l) in self.far.range(..self.base_tick) {
                if !f(p, l) {
                    return;
                }
            }
            if self.occ_lo <= self.occ_hi {
                let top = self.occ_hi.min(self.window.len() - 1);
                for i in self.occ_lo..=top {
                    if !f(self.base_tick + i as i64, &self.window[i]) {
                        return;
                    }
                }
            }
            for (&p, l) in self.far.range(hi..) {
                if !f(p, l) {
                    return;
                }
            }
        }
    }

    /// Visit populated levels best-first. `f` returns false to stop.
    pub fn try_for_each_populated(&self, mut f: impl FnMut(i64, &Level) -> bool) {
        self.traverse(|p, l| if l.order_count > 0 { f(p, l) } else { true });
    }

    /// Visit every level that is not dead at `now`, best-first (= price order:
    /// bids descending, asks ascending). Serialization traversal.
    pub fn for_each_alive(&self, now: u64, mut f: impl FnMut(i64, &Level)) {
        self.traverse(|p, l| {
            if !l.dead(now) {
                f(p, l);
            }
            true
        });
    }

    /// Best populated price strictly worse than `price`.
    pub fn next_best_from(&self, price: i64) -> Option<i64> {
        let mut out = None;
        self.traverse(|p, l| {
            if self.better(price, p) && l.order_count > 0 {
                out = Some(p);
                false
            } else {
                true
            }
        });
        out
    }

    pub fn scan_best(&self) -> Option<i64> {
        let mut out = None;
        self.try_for_each_populated(|p, _| {
            out = Some(p);
            false
        });
        out
    }
}

/// Append the order in `slot` to the tail of its price level FIFO.
/// `flow_added` folds the caller's added-flow record into the single level
/// lookup: `Some((ts, qty))` ≡ `level.flow.record_added(ts, qty)` after link.
pub(crate) fn link_tail(
    sb: &mut SideBook,
    orders: &mut Slab<Order>,
    slot: u32,
    flow_added: Option<(u64, u32)>,
) {
    let price = orders[slot as usize].price;
    let size = orders[slot as usize].size;
    let level = sb.level_entry(price);
    let tail = level.tail;
    level.tail = slot;
    if tail == NIL {
        level.head = slot;
    }
    level.total_size += u64::from(size);
    level.order_count += 1;
    if let Some((ts, qty)) = flow_added {
        level.flow.record_added(ts, qty);
    }
    let became_populated = level.order_count == 1;
    if tail != NIL {
        orders[tail as usize].next = slot;
    }
    orders[slot as usize].prev = tail;
    orders[slot as usize].next = NIL;
    if became_populated {
        sb.populated += 1;
    }
}

/// Append a snapshot-loaded order to the snapshot prefix, ahead of every
/// live-origin order while preserving snapshot block order.
pub(crate) fn link_snapshot(sb: &mut SideBook, orders: &mut Slab<Order>, slot: u32) {
    debug_assert_eq!(orders[slot as usize].origin, OrderOrigin::Snapshot);
    let price = orders[slot as usize].price;
    let size = orders[slot as usize].size;
    let level = sb.level_entry(price);
    let prefix_tail = level.snapshot_tail;
    let next = if prefix_tail == NIL {
        level.head
    } else {
        orders[prefix_tail as usize].next
    };

    if prefix_tail == NIL {
        level.head = slot;
    } else {
        orders[prefix_tail as usize].next = slot;
    }
    if next == NIL {
        level.tail = slot;
    } else {
        orders[next as usize].prev = slot;
    }
    orders[slot as usize].prev = prefix_tail;
    orders[slot as usize].next = next;
    level.snapshot_tail = slot;
    level.total_size += u64::from(size);
    level.order_count += 1;
    if level.order_count == 1 {
        sb.populated += 1;
    }
}

/// Detach the order in `slot` from its price level FIFO. `cancelled_ts` folds
/// the caller's pulled-flow record into the single level lookup:
/// `Some(ts)` ≡ `level.flow.record_cancelled(ts, order.size)` after unlink.
pub(crate) fn unlink(
    sb: &mut SideBook,
    orders: &mut Slab<Order>,
    slot: u32,
    cancelled_ts: Option<u64>,
) {
    let o = &orders[slot as usize];
    let (prev, next, price, size) = (o.prev, o.next, o.price, o.size);
    if prev != NIL {
        orders[prev as usize].next = next;
    }
    if next != NIL {
        orders[next as usize].prev = prev;
    }
    let level = sb
        .level_mut(price)
        .expect("fft-book invariant: level missing for linked order");
    if level.head == slot {
        level.head = next;
    }
    if level.tail == slot {
        level.tail = prev;
    }
    if level.snapshot_tail == slot {
        level.snapshot_tail = prev;
    }
    level.total_size -= u64::from(size);
    level.order_count -= 1;
    if let Some(ts) = cancelled_ts {
        level.flow.record_cancelled(ts, size);
    }
    let became_empty = level.order_count == 0;
    orders[slot as usize].prev = NIL;
    orders[slot as usize].next = NIL;
    if became_empty {
        sb.populated -= 1;
    }
}
