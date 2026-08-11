//! Cancel / Modify application paths (including gap-desync venue-wins handling).

use crate::book::Book;
use crate::level::{Order, OrderOrigin};
use crate::side::{link_tail, unlink};
use fft_core::{CanonicalEvent, Side};

impl Book {
    /// Resting order placed under an earlier gap epoch than the book currently
    /// holds: venue state may have diverged across the gap (FFTLOG-V2 §4).
    #[inline]
    pub(crate) fn is_gap_tainted(&self, order: &Order) -> bool {
        order.epoch < self.refresh.gap_epoch
    }

    pub(crate) fn do_cancel(&mut self, ev: &CanonicalEvent, ts: u64) {
        let id = ev.order_id.0;
        let Some(&slot) = self.index.get(&id) else {
            // Cancel of a depleted id is the "no refresh after all" terminal.
            if !self.refresh.cancel_tombstone(id) {
                self.unknown_refs += 1;
            }
            return;
        };
        let o = self.orders[slot as usize].clone();
        let price = self.to_ticks(ev.price);
        let side_ok = ev.side == Side::None || ev.side == o.side;
        let price_ok = price == o.price;
        let size_ok = ev.size > 0 && ev.size <= o.size;
        if self.is_gap_tainted(&o) && !(side_ok && price_ok && size_ok) {
            // Across a gap the venue may have diverged from retained depth.
            // Venue said this id is gone: drop what we hold, never assert.
            assert!(ev.size > 0, "fft-book: Cancel with size 0 (id {id})");
            self.gap_desync_cancels += 1;
            self.remove_live_order(slot, &o, Some(ts));
            return;
        }
        assert!(
            side_ok,
            "fft-book: Cancel side {:?} != resting side {:?} (id {id})",
            ev.side, o.side
        );
        assert_eq!(
            price, o.price,
            "fft-book: Cancel price {price} != resting price {} (id {id})",
            o.price
        );
        assert!(ev.size > 0, "fft-book: Cancel with size 0 (id {id})");
        assert!(
            size_ok,
            "fft-book: Cancel size {} > resting {} (id {id})",
            ev.size, o.size
        );
        if ev.size < o.size {
            let sb = if o.side == Side::Bid {
                &mut self.bids
            } else {
                &mut self.asks
            };
            let level = sb
                .level_mut(o.price)
                .expect("fft-book invariant: level missing for live order");
            level.total_size -= u64::from(ev.size);
            level.flow.record_cancelled(ts, ev.size);
            self.orders[slot as usize].size -= ev.size;
            self.refresh.on_book_change(id);
            return;
        }

        self.remove_live_order(slot, &o, Some(ts));
    }

    pub(crate) fn remove_live_order(
        &mut self,
        slot: u32,
        order: &Order,
        cancelled_ts: Option<u64>,
    ) {
        let sb = if order.side == Side::Bid {
            &mut self.bids
        } else {
            &mut self.asks
        };
        self.index.remove(&order.id);
        unlink(sb, &mut self.orders, slot, cancelled_ts);
        sb.note_remove(order.price);
        self.orders.remove(slot as usize);
        self.refresh
            .on_cancel_live(order.id, order.side, order.price);
    }

    /// Exact CME modify semantics: same price + size-down mutates in place and
    /// keeps queue position; size-up or price change relinks at the back of the
    /// target level. A Modify for a depleted (tombstoned) id is the CME iceberg
    /// restore path and re-enters the book through `insert_order`.
    pub(crate) fn do_modify(&mut self, ev: &CanonicalEvent, ts: u64) {
        let id = ev.order_id.0;
        let Some(&slot) = self.index.get(&id) else {
            if self.refresh.tombstone_side(id).is_some() {
                if ev.size == 0 {
                    self.refresh.cancel_tombstone(id);
                    return;
                }
                let side = if ev.side == Side::None {
                    self.refresh.tombstone_side(id).unwrap()
                } else {
                    ev.side
                };
                let price = self.to_ticks(ev.price);
                self.insert_order(id, side, price, ev.size, ts);
            } else {
                self.unknown_refs += 1;
            }
            return;
        };
        let o = self.orders[slot as usize].clone();
        let new_price = self.to_ticks(ev.price);
        let side_ok = ev.side == Side::None || ev.side == o.side;
        let zero_price_ok = ev.size != 0 || new_price == o.price;
        if self.is_gap_tainted(&o) && !(side_ok && zero_price_ok) {
            // Venue values win after a gap: drop retained depth and, when the
            // modify carries size, re-add at the back (post-gap FIFO rank is
            // unknowable and already reads Unavailable via the epoch bump).
            self.gap_desync_modifies += 1;
            self.remove_live_order(slot, &o, Some(ts));
            if ev.size > 0 {
                let side = if ev.side == Side::None {
                    o.side
                } else {
                    ev.side
                };
                self.insert_order(id, side, new_price, ev.size, ts);
            }
            return;
        }
        assert!(
            side_ok,
            "fft-book: Modify side {:?} != resting side {:?} (id {id})",
            ev.side, o.side
        );
        if ev.size == 0 {
            assert_eq!(
                new_price, o.price,
                "fft-book: zero-size Modify price {new_price} != resting price {} (id {id})",
                o.price
            );
            self.remove_live_order(slot, &o, Some(ts));
            return;
        }
        let new_size = ev.size;
        if self.refresh.is_depleted(id) {
            self.remove_live_order(slot, &o, None);
            self.insert_order(id, o.side, new_price, new_size, ts);
            return;
        }
        let now = self.now;
        let sb = if o.side == Side::Bid {
            &mut self.bids
        } else {
            &mut self.asks
        };

        if new_price == o.price {
            if new_size <= o.size {
                let shrink = o.size - new_size;
                let l = sb
                    .level_mut(o.price)
                    .expect("fft-book invariant: level missing for live order");
                l.total_size -= u64::from(shrink);
                if shrink > 0 {
                    l.flow.record_cancelled(ts, shrink);
                }
                self.orders[slot as usize].size = new_size;
            } else {
                let grow = new_size - o.size;
                unlink(sb, &mut self.orders, slot, None);
                let ord = &mut self.orders[slot as usize];
                ord.size = new_size;
                ord.ts = ts;
                ord.origin = OrderOrigin::Live;
                link_tail(sb, &mut self.orders, slot, Some((ts, grow)));
            }
            self.refresh.on_book_change(id);
            return;
        }

        unlink(sb, &mut self.orders, slot, Some(ts));
        sb.note_remove(o.price);
        sb.prepare_for(new_price, now);
        let ord = &mut self.orders[slot as usize];
        ord.price = new_price;
        ord.size = new_size;
        ord.ts = ts;
        ord.origin = OrderOrigin::Live;
        link_tail(sb, &mut self.orders, slot, Some((ts, new_size)));
        sb.note_add(new_price);
        self.refresh.on_book_change(id);
    }
}
