//! 5 s flow window (stacks / pulls / traded-at-touch) and Grady cB/cA counters
//! through the public event path.

mod common;

use common::*;
use fft_core::Side;

#[test]
fn flow_counters_accumulate_and_expire() {
    let mut b = book();
    b.apply(&add(1, Side::Bid, 100, 5, T0));
    b.apply(&add(2, Side::Bid, 100, 7, T0 + S));
    b.apply(&modify(2, Side::Bid, 100, 3, T0 + 2 * S)); // pull of 4
    b.apply(&fill(1, Side::Bid, 100, 2, T0 + 3 * S));
    let v = b.level(Side::Bid, px(100));
    assert_eq!((v.added_5s, v.cancelled_5s, v.traded_5s), (12, 4, 2));
    assert_eq!((v.total_size, v.order_count), (6, 2));

    // Advance event time past the window with unrelated activity.
    b.apply(&add(3, Side::Ask, 105, 1, T0 + 9 * S));
    let v = b.level(Side::Bid, px(100));
    assert_eq!((v.added_5s, v.cancelled_5s, v.traded_5s), (0, 0, 0));
    assert_eq!((v.total_size, v.order_count), (6, 2));
    b.check_invariants();
}

#[test]
fn flow_survives_on_emptied_level_until_stale() {
    let mut b = book();
    b.apply(&add(1, Side::Bid, 100, 5, T0));
    b.apply(&cancel(1, T0 + 1));
    // Level empty, but the pull is visible for 5 s.
    let v = b.level(Side::Bid, px(100));
    assert_eq!((v.order_count, v.cancelled_5s), (0, 5));
    assert_eq!(b.populated_levels(), 0);
    b.check_invariants();
}

#[test]
fn flow_survives_dense_window_recenter_until_expiry() {
    let mut b = book();
    b.apply(&add(1, Side::Bid, 100, 5, T0));
    b.apply(&cancel(1, T0 + 1));

    // With no resting best, this distant add recentres the dense window around
    // 1_000 and evicts the just-emptied level at 100 into the far map.
    b.apply(&add(2, Side::Bid, 1_000, 1, T0 + 2));
    let fresh = b.level(Side::Bid, px(100));
    assert_eq!(
        (fresh.order_count, fresh.added_5s, fresh.cancelled_5s),
        (0, 5, 5)
    );

    b.apply(&ev(
        fft_core::EventKind::Status,
        Side::None,
        0,
        0,
        0,
        T0 + 6 * S,
        0,
    ));
    let expired = b.level(Side::Bid, px(100));
    assert_eq!(
        (expired.order_count, expired.added_5s, expired.cancelled_5s,),
        (0, 0, 0)
    );
    b.check_invariants();
}

#[test]
fn inside_traded_counters_reset_on_price_change() {
    let mut b = book();
    b.apply(&trade(Side::Ask, 100, 5, T0)); // sell into the bid
    b.apply(&trade(Side::Ask, 100, 3, T0 + 1));
    b.apply(&trade(Side::Bid, 101, 4, T0 + 2)); // buy into the ask
    let t = b.traded_at_inside();
    assert_eq!((t.bid_price, t.bid_vol), (Some(px(100)), 8));
    assert_eq!((t.ask_price, t.ask_vol), (Some(px(101)), 4));

    b.apply(&trade(Side::Ask, 99, 2, T0 + 3)); // bid price moved: cB resets
    let t = b.traded_at_inside();
    assert_eq!((t.bid_price, t.bid_vol), (Some(px(99)), 2));
    assert_eq!((t.ask_price, t.ask_vol), (Some(px(101)), 4));

    let lt = b.last_trade().unwrap();
    assert_eq!((lt.price, lt.size, lt.aggressor), (px(99), 2, Side::Ask));
}
