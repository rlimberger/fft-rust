//! The seven native-refresh fixtures from the M1 gate (PRD §2.4, §4 claim 4),
//! plus the restore-by-Modify variant of the CME signature.

mod common;

use common::*;
use fft_book::{PriceRefreshAgg, RefreshState};
use fft_core::{OrderId, Side};

fn state(b: &fft_book::Book, id: u64) -> RefreshState {
    b.refresh_state(OrderId(id))
}

const NOT_NATIVE: RefreshState = RefreshState::Known {
    native: false,
    reloads: 0,
    hidden_volume: 0,
};

/// 1. Single refresh: same id restored after its displayed size fully trades.
#[test]
fn single_refresh() {
    let mut b = book();
    b.apply(&add(1, Side::Bid, 100, 5, T0));
    b.apply(&add(2, Side::Bid, 100, 7, T0 + 1));
    b.apply(&fill(1, Side::Bid, 100, 5, T0 + 2));
    assert_eq!(state(&b, 1), RefreshState::NotResting);
    b.apply(&add(1, Side::Bid, 100, 5, T0 + 3));
    assert_eq!(
        state(&b, 1),
        RefreshState::Known {
            native: true,
            reloads: 1,
            hidden_volume: 5
        }
    );
    assert_eq!(
        b.refresh_at(Side::Bid, px(100)),
        PriceRefreshAgg {
            refresh_count: 1,
            hidden_volume: 5
        }
    );
    // The reload lost priority: it rests behind order 2.
    assert_eq!(b.queue_position(OrderId(1)).unwrap().rank, 2);
    b.check_invariants();
}

/// 1b. The real CME wire form: the restore arrives as a Modify of the id the
/// full fill just removed.
#[test]
fn single_refresh_via_modify() {
    let mut b = book();
    b.apply(&add(1, Side::Ask, 105, 4, T0));
    b.apply(&fill(1, Side::Ask, 105, 4, T0 + 1));
    b.apply(&modify(1, Side::Ask, 105, 6, T0 + 2));
    assert_eq!(
        state(&b, 1),
        RefreshState::Known {
            native: true,
            reloads: 1,
            hidden_volume: 6
        }
    );
    assert_eq!(b.refresh_at(Side::Ask, px(105)).refresh_count, 1);
    b.check_invariants();
}

/// 2. Multiple reloads accumulate count and hidden volume.
#[test]
fn multiple_reloads() {
    let mut b = book();
    b.apply(&add(1, Side::Bid, 100, 5, T0));
    for k in 0..3u64 {
        let t = T0 + k * 10;
        b.apply(&fill(1, Side::Bid, 100, 5, t + 1));
        b.apply(&add(1, Side::Bid, 100, 5, t + 2));
    }
    assert_eq!(
        state(&b, 1),
        RefreshState::Known {
            native: true,
            reloads: 3,
            hidden_volume: 15
        }
    );
    assert_eq!(
        b.refresh_at(Side::Bid, px(100)),
        PriceRefreshAgg {
            refresh_count: 3,
            hidden_volume: 15
        }
    );
    b.check_invariants();
}

/// 3. Partial fill then modify: displayed size never depleted — no refresh.
#[test]
fn partial_fill_then_modify() {
    let mut b = book();
    b.apply(&add(1, Side::Bid, 100, 10, T0));
    b.apply(&fill(1, Side::Bid, 100, 4, T0 + 1));
    b.apply(&modify(1, Side::Bid, 100, 8, T0 + 2));
    assert_eq!(state(&b, 1), NOT_NATIVE);
    assert_eq!(b.refresh_at(Side::Bid, px(100)), PriceRefreshAgg::default());
    b.check_invariants();
}

/// 4. Full fill, then a DIFFERENT order id at the same price: not a refresh.
#[test]
fn full_fill_then_different_order() {
    let mut b = book();
    b.apply(&add(1, Side::Bid, 100, 5, T0));
    b.apply(&fill(1, Side::Bid, 100, 5, T0 + 1));
    b.apply(&add(9, Side::Bid, 100, 5, T0 + 2));
    assert_eq!(state(&b, 9), NOT_NATIVE);
    assert_eq!(b.refresh_at(Side::Bid, px(100)), PriceRefreshAgg::default());
    b.check_invariants();
}

/// 5. Synthetic iceberg (a new order id per reload) must never classify as
///    native — that is exactly the heuristic we refuse to ship.
#[test]
fn synthetic_iceberg_is_not_native() {
    let mut b = book();
    let mut id = 10u64;
    b.apply(&add(id, Side::Ask, 105, 5, T0));
    for k in 0..4u64 {
        let t = T0 + 10 * k;
        b.apply(&fill(id, Side::Ask, 105, 5, t + 1));
        id += 1;
        b.apply(&add(id, Side::Ask, 105, 5, t + 2));
        assert_eq!(state(&b, id), NOT_NATIVE);
    }
    assert_eq!(b.refresh_at(Side::Ask, px(105)), PriceRefreshAgg::default());
    b.check_invariants();
}

/// 6. Cancel at depletion: the venue said the order is done — a later add of
///    the same id is a new order life, not a refresh.
#[test]
fn cancel_at_depletion() {
    let mut b = book();
    b.apply(&add(1, Side::Bid, 100, 5, T0));
    b.apply(&fill(1, Side::Bid, 100, 5, T0 + 1));
    b.apply(&cancel(1, T0 + 2));
    b.apply(&add(1, Side::Bid, 100, 5, T0 + 3));
    assert_eq!(state(&b, 1), NOT_NATIVE);
    assert_eq!(b.refresh_at(Side::Bid, px(100)), PriceRefreshAgg::default());
    b.check_invariants();
}

/// 7. Gap around a candidate refresh: classification reads Unavailable — never
///    a false (or true) boolean built on missing events.
#[test]
fn gap_around_candidate_refresh() {
    let mut b = book();
    b.apply(&add(1, Side::Bid, 100, 5, T0));
    b.apply(&add(2, Side::Bid, 99, 5, T0 + 1));
    b.apply(&fill(1, Side::Bid, 100, 5, T0 + 2));
    b.apply(&gap(T0 + 3, 1000, 1007));
    b.apply(&add(1, Side::Bid, 100, 5, T0 + 4));
    // The candidate cycle spans the gap: unavailable, and no aggregate credit.
    assert_eq!(state(&b, 1), RefreshState::Unavailable);
    assert_eq!(b.refresh_at(Side::Bid, px(100)), PriceRefreshAgg::default());
    // Any order that predates the gap is unavailable too...
    assert_eq!(state(&b, 2), RefreshState::Unavailable);
    // ...while an order first observed after the gap is unambiguous.
    b.apply(&add(3, Side::Bid, 100, 5, T0 + 5));
    assert_eq!(state(&b, 3), NOT_NATIVE);
    b.check_invariants();

    // A complete post-gap depletion→restore cycle re-proves nativeness
    // unambiguously (counts remain observed lower bounds).
    b.apply(&fill(1, Side::Bid, 100, 5, T0 + 6));
    b.apply(&add(1, Side::Bid, 100, 5, T0 + 7));
    assert_eq!(
        state(&b, 1),
        RefreshState::Known {
            native: true,
            reloads: 1,
            hidden_volume: 5
        }
    );
    assert_eq!(b.refresh_at(Side::Bid, px(100)).refresh_count, 1);
    b.check_invariants();
}

/// Restores outside the acceptance window are a new order life.
#[test]
fn late_restore_is_not_a_refresh() {
    let mut b = book();
    b.apply(&add(1, Side::Bid, 100, 5, T0));
    b.apply(&fill(1, Side::Bid, 100, 5, T0 + 1));
    b.apply(&add(
        1,
        Side::Bid,
        100,
        5,
        T0 + 1 + fft_book::REFRESH_WINDOW_NS + 1,
    ));
    assert_eq!(state(&b, 1), NOT_NATIVE);
    b.check_invariants();
}
