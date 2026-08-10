//! Exact CME modify semantics: size-down in place keeps queue position;
//! size-up or price change loses it (back of the target level).

mod common;

use common::*;
use fft_core::{OrderId, Side};

fn qp(b: &fft_book::Book, id: u64) -> fft_book::QueuePosition {
    b.queue_position(OrderId(id)).expect("order should rest")
}

fn three_bids() -> fft_book::Book {
    let mut b = book();
    b.apply(&add(1, Side::Bid, 100, 10, T0));
    b.apply(&add(2, Side::Bid, 100, 20, T0 + 1));
    b.apply(&add(3, Side::Bid, 100, 30, T0 + 2));
    b.check_invariants();
    b
}

#[test]
fn size_down_keeps_rank() {
    let mut b = three_bids();
    b.apply(&modify(2, Side::Bid, 100, 15, T0 + 3));
    let q = qp(&b, 2);
    assert_eq!(
        (q.rank, q.orders_ahead, q.contracts_ahead, q.size),
        (2, 1, 10, 15)
    );
    let q3 = qp(&b, 3);
    assert_eq!((q3.rank, q3.contracts_ahead), (3, 25));
    assert_eq!(b.level(Side::Bid, px(100)).total_size, 55);
    // The shrink is a pull in the flow window.
    assert_eq!(b.level(Side::Bid, px(100)).cancelled_5s, 5);
    b.check_invariants();
}

#[test]
fn equal_size_same_price_keeps_rank() {
    let mut b = three_bids();
    b.apply(&modify(1, Side::Bid, 100, 10, T0 + 3));
    assert_eq!(qp(&b, 1).rank, 1);
    assert_eq!(b.level(Side::Bid, px(100)).cancelled_5s, 0);
    b.check_invariants();
}

#[test]
fn size_up_loses_rank() {
    let mut b = three_bids();
    b.apply(&modify(1, Side::Bid, 100, 12, T0 + 3));
    let q = qp(&b, 1);
    assert_eq!((q.rank, q.orders_ahead, q.contracts_ahead), (3, 2, 50));
    assert_eq!(qp(&b, 2).rank, 1);
    assert_eq!(qp(&b, 3).rank, 2);
    // Only the grown amount stacks.
    assert_eq!(b.level(Side::Bid, px(100)).added_5s, 62);
    b.check_invariants();
}

#[test]
fn price_change_moves_level_and_loses_rank() {
    let mut b = three_bids();
    b.apply(&add(4, Side::Bid, 99, 5, T0 + 3));
    b.apply(&modify(1, Side::Bid, 99, 10, T0 + 4));
    let q = qp(&b, 1);
    assert_eq!(q.price, px(99));
    assert_eq!((q.rank, q.orders_ahead, q.contracts_ahead), (2, 1, 5));
    // Old level: pulled its full size; new level: stacked its full size.
    assert_eq!(b.level(Side::Bid, px(100)).cancelled_5s, 10);
    assert_eq!(b.level(Side::Bid, px(100)).total_size, 50);
    assert_eq!(b.level(Side::Bid, px(99)).added_5s, 15);
    assert_eq!(b.level(Side::Bid, px(99)).total_size, 15);
    assert_eq!(qp(&b, 2).rank, 1);
    b.check_invariants();
}

#[test]
fn modify_to_zero_cancels() {
    let mut b = three_bids();
    b.apply(&modify(2, Side::Bid, 100, 0, T0 + 3));
    assert!(b.queue_position(OrderId(2)).is_none());
    assert_eq!(b.level(Side::Bid, px(100)).order_count, 2);
    assert_eq!(b.level(Side::Bid, px(100)).cancelled_5s, 20);
    assert_eq!(qp(&b, 3).rank, 2);
    b.check_invariants();
}

#[test]
fn fill_keeps_displayed_size_and_counts_traded() {
    let mut b = three_bids();
    b.apply(&fill(1, Side::Bid, 100, 4, T0 + 3));
    let q = qp(&b, 1);
    assert_eq!((q.rank, q.size), (1, 10));
    assert_eq!(b.level(Side::Bid, px(100)).total_size, 60);
    assert_eq!(b.level(Side::Bid, px(100)).traded_5s, 4);
    b.check_invariants();
}

#[test]
fn partial_fill_companion_cancel_applies_venue_quantity() {
    let mut b = three_bids();
    b.apply(&fill(1, Side::Bid, 100, 4, T0 + 3));
    b.apply(&cancel(1, Side::Bid, 100, 4, T0 + 4));
    let q = qp(&b, 1);
    assert_eq!((q.rank, q.size), (1, 6));
    assert_eq!(b.level(Side::Bid, px(100)).total_size, 56);
    assert_eq!(b.level(Side::Bid, px(100)).cancelled_5s, 4);
    b.check_invariants();
}
