//! Sequence/gap accounting: forward skips are legitimate (filtered channel),
//! regressions without an interposed Gap event are a loud error.

mod common;

use common::*;
use fft_core::{EventKind, Side};

#[test]
fn forward_skips_are_tolerated() {
    let mut b = book();
    b.apply(&ev(EventKind::Add, Side::Bid, 100, 5, 1, T0, 10));
    b.apply(&ev(EventKind::Add, Side::Bid, 99, 5, 2, T0 + 1, 11));
    // Other instruments on the channel consumed 12..=49.
    b.apply(&ev(EventKind::Add, Side::Ask, 105, 5, 3, T0 + 2, 50));
    assert_eq!(b.last_seq(), Some(50));
    assert!(!b.gap_pending());
    b.check_invariants();
}

#[test]
#[should_panic(expected = "seq regression")]
fn regression_without_gap_panics() {
    let mut b = book();
    b.apply(&ev(EventKind::Add, Side::Bid, 100, 5, 1, T0, 10));
    b.apply(&ev(EventKind::Add, Side::Bid, 99, 5, 2, T0 + 1, 5));
}

#[test]
fn gap_event_reanchors_sequencing() {
    let mut b = book();
    b.apply(&ev(EventKind::Add, Side::Bid, 100, 5, 1, T0, 10));
    b.apply(&gap(T0 + 1, 11, 3));
    assert!(b.gap_pending());
    assert_eq!(b.last_seq(), None);
    assert_eq!(b.last_gap(), Some((11, 3)));
    assert_eq!(b.gaps_seen(), 1);
    // Re-anchor below the old sequence: explained by the Gap, no panic.
    b.apply(&ev(EventKind::Add, Side::Bid, 99, 5, 2, T0 + 2, 3));
    assert_eq!(b.last_seq(), Some(3));
    assert!(!b.gap_pending());
    b.check_invariants();
}

#[test]
fn unsequenced_events_skip_accounting() {
    let mut b = book();
    b.apply(&ev(EventKind::Add, Side::Bid, 100, 5, 1, T0, 7));
    b.apply(&add(2, Side::Bid, 99, 5, T0 + 1)); // seq 0
    assert_eq!(b.last_seq(), Some(7));
    b.check_invariants();
}

#[test]
fn unknown_refs_are_counted_not_fatal() {
    let mut b = book();
    b.apply(&add(1, Side::Bid, 100, 5, T0));
    b.apply(&cancel(42, T0 + 1));
    b.apply(&modify(43, Side::Bid, 100, 5, T0 + 2));
    b.apply(&fill(44, Side::Bid, 100, 5, T0 + 3));
    assert_eq!(b.unknown_ref_events(), 3);
    assert_eq!(b.live_order_count(), 1);
    b.check_invariants();
}

#[test]
#[should_panic(expected = "duplicate Add")]
fn duplicate_add_panics() {
    let mut b = book();
    b.apply(&add(1, Side::Bid, 100, 5, T0));
    b.apply(&add(1, Side::Bid, 101, 5, T0 + 1));
}

#[test]
#[should_panic(expected = "overfill")]
fn overfill_panics() {
    let mut b = book();
    b.apply(&add(1, Side::Bid, 100, 5, T0));
    b.apply(&fill(1, Side::Bid, 100, 6, T0 + 1));
}

#[test]
#[should_panic(expected = "not aligned to tick")]
fn off_tick_price_panics() {
    let mut b = book();
    let mut e = add(1, Side::Bid, 100, 5, T0);
    e.price = fft_core::Price(e.price.0 + 1);
    b.apply(&e);
}

#[test]
fn clear_resets_depth_keeps_tape() {
    let mut b = book();
    b.apply(&add(1, Side::Bid, 100, 5, T0));
    b.apply(&add(2, Side::Ask, 105, 5, T0 + 1));
    b.apply(&trade(Side::Bid, 105, 2, T0 + 2));
    b.apply(&clear(T0 + 3));
    assert_eq!(b.live_order_count(), 0);
    assert_eq!(b.populated_levels(), 0);
    assert_eq!(b.best_bid(), None);
    assert!(b.last_trade().is_some());
    b.apply(&add(3, Side::Bid, 100, 5, T0 + 4));
    assert_eq!(b.best_bid(), Some(px(100)));
    b.check_invariants();
}
