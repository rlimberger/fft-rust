//! Native-refresh (iceberg) state machine — PRD §2.4, §4 claim 4.
//!
//! CME signature: the SAME order id restored to positive displayed size after
//! its displayed size fully trades. Fully-filled orders leave a tombstone; a
//! same-id/same-side/same-price placement within [`REFRESH_WINDOW_NS`] is a
//! classified refresh **iff** no sequence gap separates depletion from restore
//! (`epoch` check). Any gap makes reads Unavailable, never a false boolean.

use crate::{PriceRefreshAgg, REFRESH_WINDOW_NS, RefreshState};
use fft_core::Side;
use std::collections::{BTreeMap, HashMap};

/// Refresh history of a live order. Only orders with observed reloads or a
/// broken (gap-spanning) restore have an entry; plain orders have none.
#[derive(Debug, Clone, Copy)]
pub(crate) struct LiveRefresh {
    pub reloads: u32,
    pub hidden: u64,
    /// Set when this order life was restored across a gap: classification for
    /// it reads Unavailable until a clean depletion→restore cycle re-proves it.
    pub unavailable: bool,
}

/// A fully-displayed-filled order awaiting either a same-id restore (native
/// refresh), an explicit cancel (plain full fill), or window expiry.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Tombstone {
    pub side: Side,
    /// Price in ticks at depletion.
    pub price: i64,
    pub depleted_ts: u64,
    /// Gap epoch at depletion; a restore under a later epoch is ambiguous.
    pub epoch: u32,
    pub reloads: u32,
    pub hidden: u64,
}

#[derive(Debug, Default)]
pub(crate) struct RefreshTracker {
    pub live: HashMap<u64, LiveRefresh>,
    pub tombstones: HashMap<u64, Tombstone>,
    /// Session-cumulative aggregates keyed by (side wire value, price ticks).
    pub per_price: BTreeMap<(u8, i64), PriceRefreshAgg>,
    /// Incremented on every sequence gap. Orders placed under an older epoch
    /// read Unavailable.
    pub gap_epoch: u32,
}

impl RefreshTracker {
    /// An order id is being (re)placed in the book. Consumes a matching
    /// tombstone and classifies the restore.
    pub fn on_placed(&mut self, id: u64, side: Side, price: i64, size: u32, ts: u64) {
        let Some(t) = self.tombstones.remove(&id) else {
            return;
        };
        let same_slot = t.side == side && t.price == price;
        let in_window = ts.saturating_sub(t.depleted_ts) <= REFRESH_WINDOW_NS;
        if !(same_slot && in_window) {
            // Different side/price or far outside the venue's refresh timing:
            // a new order life, not a refresh. History is discarded.
            return;
        }
        if t.epoch == self.gap_epoch {
            // Clean depletion→restore cycle: deterministic native refresh.
            // This also re-proves nativeness after an earlier gap.
            self.live.insert(
                id,
                LiveRefresh {
                    reloads: t.reloads + 1,
                    hidden: t.hidden + u64::from(size),
                    unavailable: false,
                },
            );
            let agg = self.per_price.entry((side as u8, price)).or_default();
            agg.refresh_count += 1;
            agg.hidden_volume += u64::from(size);
        } else {
            // A gap fell between depletion and restore: events may be missing,
            // so this cycle proves nothing. Counts freeze as lower bounds.
            self.live.insert(
                id,
                LiveRefresh {
                    reloads: t.reloads,
                    hidden: t.hidden,
                    unavailable: true,
                },
            );
        }
    }

    /// Displayed size of `id` fully traded and the order left the book.
    pub fn on_depleted(&mut self, id: u64, side: Side, price: i64, ts: u64) {
        let (reloads, hidden) = self
            .live
            .remove(&id)
            .map(|e| (e.reloads, e.hidden))
            .unwrap_or((0, 0));
        self.tombstones.insert(
            id,
            Tombstone {
                side,
                price,
                depleted_ts: ts,
                epoch: self.gap_epoch,
                reloads,
                hidden,
            },
        );
    }

    /// Side recorded for a pending tombstone, if any (used to route a
    /// restore-by-Modify whose event carries no side).
    pub fn tombstone_side(&self, id: u64) -> Option<Side> {
        self.tombstones.get(&id).map(|t| t.side)
    }

    /// Live order cancelled: its refresh tracking ends with it.
    pub fn on_cancel_live(&mut self, id: u64) {
        self.live.remove(&id);
    }

    /// Explicit cancel of a depleted id: terminal, never a refresh.
    /// Returns whether a tombstone existed.
    pub fn cancel_tombstone(&mut self, id: u64) -> bool {
        self.tombstones.remove(&id).is_some()
    }

    pub fn on_gap(&mut self) {
        self.gap_epoch += 1;
    }

    /// Book clear: all order-keyed state dies with the book; per-price session
    /// aggregates survive (they describe observed history, not current depth).
    pub fn on_clear(&mut self) {
        self.live.clear();
        self.tombstones.clear();
    }

    /// Drop tombstones past the refresh window — their ids can no longer
    /// classify, so keeping them would only grow memory.
    pub fn gc(&mut self, now: u64) {
        self.tombstones
            .retain(|_, t| now.saturating_sub(t.depleted_ts) <= REFRESH_WINDOW_NS);
    }

    /// Classification read for a live order placed under `order_epoch`.
    pub fn state_for(&self, id: u64, order_epoch: u32) -> RefreshState {
        if order_epoch < self.gap_epoch {
            return RefreshState::Unavailable;
        }
        match self.live.get(&id) {
            Some(e) if e.unavailable => RefreshState::Unavailable,
            Some(e) => RefreshState::Known {
                native: e.reloads > 0,
                reloads: e.reloads,
                hidden_volume: e.hidden,
            },
            None => RefreshState::Known {
                native: false,
                reloads: 0,
                hidden_volume: 0,
            },
        }
    }

    pub fn agg_at(&self, side: Side, price: i64) -> PriceRefreshAgg {
        self.per_price
            .get(&(side as u8, price))
            .copied()
            .unwrap_or_default()
    }
}
