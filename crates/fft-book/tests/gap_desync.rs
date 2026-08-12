//! Gap-tainted retained-ID desync: venue state may diverge from retained depth
//! across a Gap (FFTLOG-V2 §4). Pre-gap conflicts must not panic; post-gap /
//! no-gap malformed events stay loud.

mod common;

use common::*;
use fft_book::RefreshState;
use fft_core::{OrderId, Side};

#[test]
fn gap_then_duplicate_add_replaces_retained_order() {
    let mut b = book();
    b.apply(&add(1, Side::Bid, 100, 10, T0));
    b.apply(&gap(T0 + 1, 11, 20));
    b.apply(&add(1, Side::Ask, 105, 3, T0 + 2));

    let q = b
        .queue_position(OrderId(1))
        .expect("venue order should rest");
    assert_eq!(
        (q.side, q.price, q.size, q.rank),
        (Side::Ask, px(105), 3, 1)
    );
    assert_eq!(b.level(Side::Bid, px(100)).total_size, 0);
    assert_eq!(b.level(Side::Ask, px(105)).total_size, 3);
    assert_eq!(b.gap_desync_adds(), 1);
    b.check_invariants();
}

#[test]
fn gap_then_known_fill_side_mismatch_uses_venue_side() {
    let mut b = book();
    b.apply(&add(1, Side::Bid, 100, 10, T0));
    b.apply(&gap(T0 + 1, 11, 20));
    b.apply(&fill(1, Side::Ask, 101, 4, T0 + 2));

    let q = b
        .queue_position(OrderId(1))
        .expect("Fill must not mutate depth");
    assert_eq!((q.side, q.price, q.size), (Side::Bid, px(100), 10));
    assert_eq!(b.level(Side::Ask, px(101)).traded_5s, 4);
    assert_eq!(b.level(Side::Bid, px(101)).traded_5s, 0);
    assert_eq!(b.last_trade().unwrap().aggressor, Side::Bid);
    assert_eq!(b.gap_desync_fills(), 1);
    assert_eq!(b.fills_off_display(), 0);
    b.check_invariants();
}

#[test]
fn gap_then_mismatched_cancel_removes_without_panic() {
    let mut b = book();
    b.apply(&add(1, Side::Bid, 100, 10, T0));
    b.apply(&gap(T0 + 1, 11, 20));
    // Venue cancel disagrees on size and price with what we retained.
    b.apply(&cancel(1, Side::Bid, 101, 7, T0 + 2));
    assert!(b.queue_position(OrderId(1)).is_none());
    assert_eq!(b.live_order_count(), 0);
    assert_eq!(b.gap_desync_cancels(), 1);
    assert_eq!(b.gap_desync_modifies(), 0);
    assert_eq!(b.level(Side::Bid, px(100)).total_size, 0);
    b.check_invariants();
}

#[test]
fn gap_then_mismatched_modify_applies_venue_values() {
    let mut b = book();
    b.apply(&add(1, Side::Bid, 100, 10, T0));
    b.apply(&gap(T0 + 1, 11, 20));
    // Side flip is a hard assert pre-gap; post-gap venue values win via reinsert.
    b.apply(&modify(1, Side::Ask, 105, 3, T0 + 2));
    let q = b.queue_position(OrderId(1)).expect("order should rest");
    assert_eq!(q.side, Side::Ask);
    assert_eq!(q.price, px(105));
    assert_eq!(q.size, 3);
    assert_eq!(q.rank, 1);
    assert_eq!(b.level(Side::Bid, px(100)).total_size, 0);
    assert_eq!(b.level(Side::Ask, px(105)).total_size, 3);
    assert_eq!(b.gap_desync_modifies(), 1);
    assert_eq!(b.gap_desync_cancels(), 0);
    // Reinsert stamps the current gap epoch — classification can re-arm later.
    assert_eq!(b.gaps_seen(), 1);
    b.check_invariants();
}

#[test]
#[should_panic(expected = "duplicate Add")]
fn post_gap_fresh_order_duplicate_add_still_panics() {
    let mut b = book();
    b.apply(&gap(T0, 11, 20));
    b.apply(&add(1, Side::Bid, 100, 5, T0 + 1));
    b.apply(&add(1, Side::Ask, 105, 3, T0 + 2));
}

#[test]
#[should_panic(expected = "Fill side")]
fn post_gap_fresh_order_fill_side_mismatch_still_panics() {
    let mut b = book();
    b.apply(&gap(T0, 11, 20));
    b.apply(&add(1, Side::Bid, 100, 5, T0 + 1));
    b.apply(&fill(1, Side::Ask, 100, 2, T0 + 2));
}

#[test]
#[should_panic(expected = "Cancel size")]
fn post_gap_fresh_order_mismatched_cancel_still_panics() {
    let mut b = book();
    b.apply(&add(1, Side::Bid, 100, 10, T0));
    b.apply(&gap(T0 + 1, 11, 20));
    // Fresh post-gap add is not tainted.
    b.apply(&add(2, Side::Bid, 100, 5, T0 + 2));
    b.apply(&cancel(2, Side::Bid, 100, 9, T0 + 3));
}

#[test]
fn matching_cancel_on_tainted_order_uses_normal_path() {
    let mut b = book();
    b.apply(&add(1, Side::Bid, 100, 10, T0));
    b.apply(&gap(T0 + 1, 11, 20));
    b.apply(&cancel(1, Side::Bid, 100, 4, T0 + 2));
    assert_eq!(b.queue_position(OrderId(1)).unwrap().size, 6);
    assert_eq!(b.gap_desync_cancels(), 0);
    b.check_invariants();
}

#[test]
fn second_gap_retaints_survivors() {
    let mut b = book();
    b.apply(&add(1, Side::Bid, 100, 10, T0));
    b.apply(&gap(T0 + 1, 11, 20));
    // Matching touch does not clear taint (epoch stays pre-gap).
    b.apply(&cancel(1, Side::Bid, 100, 2, T0 + 2));
    assert_eq!(b.gap_desync_cancels(), 0);
    b.apply(&gap(T0 + 3, 21, 30));
    assert_eq!(b.gaps_seen(), 2);
    // Still pre-epoch-2 → desync path on size/price disagreement.
    b.apply(&cancel(1, Side::Bid, 99, 8, T0 + 4));
    assert!(b.queue_position(OrderId(1)).is_none());
    assert_eq!(b.gap_desync_cancels(), 1);
    b.check_invariants();
}

#[test]
fn second_gap_retaints_post_first_gap_adds() {
    let mut b = book();
    b.apply(&add(1, Side::Bid, 100, 10, T0));
    b.apply(&gap(T0 + 1, 11, 20));
    b.apply(&add(2, Side::Bid, 100, 5, T0 + 2));
    // Order 2 is clean under epoch 1.
    b.apply(&gap(T0 + 3, 21, 30));
    // After the second gap, order 2 is tainted under epoch 2.
    b.apply(&cancel(2, Side::Bid, 101, 5, T0 + 4));
    assert!(b.queue_position(OrderId(2)).is_none());
    assert_eq!(b.gap_desync_cancels(), 1);
    // Pre-first-gap survivor also tainted.
    b.apply(&cancel(1, Side::Bid, 99, 10, T0 + 5));
    assert!(b.queue_position(OrderId(1)).is_none());
    assert_eq!(b.gap_desync_cancels(), 2);
    b.check_invariants();
}

#[test]
fn gap_desync_zero_size_modify_price_mismatch_removes() {
    let mut b = book();
    b.apply(&add(1, Side::Bid, 100, 10, T0));
    b.apply(&gap(T0 + 1, 11, 20));
    b.apply(&modify(1, Side::Bid, 99, 0, T0 + 2));
    assert!(b.queue_position(OrderId(1)).is_none());
    assert_eq!(b.gap_desync_modifies(), 1);
    b.check_invariants();
}

#[test]
fn gap_then_sideless_fill_skips_stale_depletion() {
    let mut b = book();
    b.apply(&add(1, Side::Bid, 100, 10, T0));
    b.apply(&gap(T0 + 1, 11, 20));
    // Sideless Fill resolves resting Bid by order-id lookup (FFTLOG-V2 §4),
    // but must not attach depletion/off-display evidence to pre-gap depth.
    b.apply(&fill(1, Side::None, 101, 10, T0 + 2));

    let q = b
        .queue_position(OrderId(1))
        .expect("Fill must not mutate depth");
    assert_eq!((q.side, q.price, q.size), (Side::Bid, px(100), 10));
    assert_eq!(b.level(Side::Bid, px(101)).traded_5s, 10);
    assert_eq!(b.level(Side::Ask, px(101)).traded_5s, 0);
    assert_eq!(b.last_trade().unwrap().aggressor, Side::Ask);
    let inside = b.traded_at_inside();
    assert_eq!((inside.bid_price, inside.bid_vol), (Some(px(101)), 10));
    assert_eq!(b.gap_desync_fills(), 1);
    assert_eq!(b.fills_off_display(), 0);
    assert_eq!(b.unknown_ref_events(), 0);

    // Companion cancel + same-id restore must not credit a native refresh from
    // the skipped stale depletion evidence.
    b.apply(&cancel(1, Side::Bid, 100, 10, T0 + 3));
    b.apply(&add(1, Side::Bid, 100, 4, T0 + 4));
    assert_eq!(
        b.refresh_state(OrderId(1)),
        RefreshState::Known {
            native: false,
            reloads: 0,
            hidden_volume: 0,
        },
        "gap-tainted sideless fill must not arm a refresh tombstone"
    );
    assert_eq!(b.refresh_at(Side::Bid, px(100)), Default::default());
    b.check_invariants();
}

#[test]
fn non_tainted_sideless_fill_still_attaches_depletion() {
    let mut b = book();
    b.apply(&gap(T0, 11, 20));
    // Post-gap fresh order is not gap-tainted.
    b.apply(&add(1, Side::Bid, 100, 3, T0 + 1));
    b.apply(&fill(1, Side::None, 101, 3, T0 + 2));

    assert_eq!(b.queue_position(OrderId(1)).unwrap().size, 3);
    assert_eq!(b.level(Side::Bid, px(101)).traded_5s, 3);
    assert_eq!(b.last_trade().unwrap().aggressor, Side::Ask);
    assert_eq!(b.gap_desync_fills(), 0);
    assert_eq!(b.fills_off_display(), 1);
    assert_eq!(b.unknown_ref_events(), 0);

    b.apply(&cancel(1, Side::Bid, 100, 3, T0 + 3));
    b.apply(&add(1, Side::Bid, 100, 4, T0 + 4));
    assert_eq!(
        b.refresh_state(OrderId(1)),
        RefreshState::Known {
            native: true,
            reloads: 1,
            hidden_volume: 4,
        }
    );
    b.check_invariants();
}
