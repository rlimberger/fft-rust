//! Native-refresh (iceberg) state machine — PRD §2.4, §4 claim 4.
//!
//! CME signature: the SAME order id restored to positive displayed size after
//! cumulative non-mutating Fill quantity reaches its displayed size. The
//! companion Cancel/Modify is book truth; a removed order leaves a tombstone.
//! A same-id/same-side/same-price placement within [`REFRESH_WINDOW_NS`] is a
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

/// A fully-filled, companion-removed order awaiting a same-id restore, a
/// second terminal Cancel, or window expiry.
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

/// Fill evidence for the current displayed tranche of a resting order.
#[derive(Debug, Clone, Copy)]
pub(crate) struct FillProgress {
    pub qty: u64,
    /// First event time at which cumulative quantity reached displayed size.
    pub depleted_ts: Option<u64>,
    pub epoch: u32,
}

#[derive(Debug, Default)]
pub(crate) struct RefreshTracker {
    pub live: HashMap<u64, LiveRefresh>,
    pub tombstones: HashMap<u64, Tombstone>,
    pub fills: HashMap<u64, FillProgress>,
    /// Session-cumulative aggregates keyed by (side wire value, price ticks).
    pub per_price: BTreeMap<(u8, i64), PriceRefreshAgg>,
    /// Incremented on every sequence gap. Orders placed under an older epoch
    /// read Unavailable.
    pub gap_epoch: u32,
    /// Fill executions whose price differed from their order's displayed price.
    pub fills_off_display: u64,
}

impl RefreshTracker {
    /// Snapshot loading cannot prove a new order life or a native refresh.
    /// Preserve any observed lower-bound history, discard candidacy, and keep
    /// classification unavailable until later clean observed activity proves it.
    pub fn on_snapshot_loaded(&mut self, id: u64) {
        self.fills.remove(&id);
        let (reloads, hidden) = self
            .tombstones
            .remove(&id)
            .map(|t| (t.reloads, t.hidden))
            .unwrap_or((0, 0));
        self.live.insert(
            id,
            LiveRefresh {
                reloads,
                hidden,
                unavailable: true,
            },
        );
    }

    /// An order id is being (re)placed in the book. Consumes a matching
    /// tombstone and classifies the restore.
    pub fn on_placed(&mut self, id: u64, side: Side, price: i64, size: u32, ts: u64) {
        self.fills.remove(&id);
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

    /// Accumulate Fill evidence without changing displayed book state.
    pub fn on_fill(&mut self, id: u64, displayed_size: u32, qty: u32, ts: u64) {
        let progress = self.fills.entry(id).or_insert(FillProgress {
            qty: 0,
            depleted_ts: None,
            epoch: self.gap_epoch,
        });
        progress.qty = progress
            .qty
            .checked_add(u64::from(qty))
            .expect("fft-book: cumulative Fill quantity overflow");
        if progress.depleted_ts.is_none() && progress.qty >= u64::from(displayed_size) {
            progress.depleted_ts = Some(ts);
        }
    }

    pub fn note_fill_off_display(&mut self) {
        self.fills_off_display = self
            .fills_off_display
            .checked_add(1)
            .expect("fft-book: off-display Fill counter overflow");
    }

    pub fn is_depleted(&self, id: u64) -> bool {
        self.fills
            .get(&id)
            .is_some_and(|progress| progress.depleted_ts.is_some())
    }

    /// A non-depleting book change starts a new displayed-tranche accounting cycle.
    pub fn on_book_change(&mut self, id: u64) {
        self.fills.remove(&id);
    }

    /// The companion book event removed the live order. Full Fill evidence arms
    /// a tombstone; partial evidence is consumed as an ordinary order death.
    pub fn on_cancel_live(&mut self, id: u64, side: Side, price: i64) {
        let depletion = self
            .fills
            .remove(&id)
            .and_then(|progress| progress.depleted_ts.map(|ts| (ts, progress.epoch)));
        let (reloads, hidden) = self
            .live
            .remove(&id)
            .map(|e| (e.reloads, e.hidden))
            .unwrap_or((0, 0));
        if let Some((depleted_ts, epoch)) = depletion {
            self.tombstones.insert(
                id,
                Tombstone {
                    side,
                    price,
                    depleted_ts,
                    epoch,
                    reloads,
                    hidden,
                },
            );
        }
    }

    /// Side recorded for a pending tombstone, if any (used to route a
    /// restore-by-Modify whose event carries no side).
    pub fn tombstone_side(&self, id: u64) -> Option<Side> {
        self.tombstones.get(&id).map(|t| t.side)
    }

    /// Explicit cancel of a depleted id: terminal, never a refresh.
    /// Returns whether a tombstone existed.
    pub fn cancel_tombstone(&mut self, id: u64) -> bool {
        self.tombstones.remove(&id).is_some()
    }

    pub fn on_gap(&mut self) {
        self.gap_epoch = self
            .gap_epoch
            .checked_add(1)
            .expect("fft-book: gap epoch overflow");
        self.fills
            .retain(|_, progress| progress.depleted_ts.is_some());
    }

    /// Book clear: all order-keyed state dies with the book; per-price session
    /// aggregates survive (they describe observed history, not current depth).
    pub fn on_clear(&mut self) {
        self.live.clear();
        self.tombstones.clear();
        self.fills.clear();
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
