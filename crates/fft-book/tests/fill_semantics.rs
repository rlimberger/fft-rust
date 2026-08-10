//! Frozen DBN Fill contract and the real opening-auction regression trace.

mod common;

use common::*;
use fft_book::RefreshState;
use fft_core::{EventKind, OrderId, Side};

const LAST: u16 = 1 << 7;
const AUCTION_TS: u64 = 1_785_362_400_601_000_000;
const ORDER_ID: u64 = 6_417_401_353_735;
const DISPLAY_TICKS: i64 = 29_871; // 7467.75
const EXECUTION_TICKS: i64 = 29_872; // 7468.00
const SIDELESS_AUCTION_TS: u64 = 1_785_276_000_000_000_000;
const SIDELESS_ORDER_ID: u64 = 6_417_401_353_278;
const SIDELESS_EXECUTION_TICKS: i64 = 29_856; // 7464.00

#[test]
fn opening_auction_fill_is_non_mutating_and_off_display() {
    let mut add = ev(
        EventKind::Add,
        Side::Bid,
        DISPLAY_TICKS,
        3,
        ORDER_ID,
        AUCTION_TS,
        28_635_175,
    );
    add.flags = LAST;
    let trade = ev(
        EventKind::Trade,
        Side::Bid,
        EXECUTION_TICKS,
        2,
        0,
        AUCTION_TS,
        28_635_182,
    );
    let fill = ev(
        EventKind::Fill,
        Side::Bid,
        EXECUTION_TICKS,
        2,
        ORDER_ID,
        AUCTION_TS,
        28_635_182,
    );
    let cancel = ev(
        EventKind::Cancel,
        Side::Bid,
        DISPLAY_TICKS,
        3,
        ORDER_ID,
        AUCTION_TS,
        28_635_183,
    );

    let mut book = book();
    book.apply(&add);
    book.apply(&trade);
    assert_eq!(
        book.last_trade(),
        None,
        "paired Trade is derived-state inert"
    );

    book.apply(&fill);
    let resting = book.queue_position(OrderId(ORDER_ID)).unwrap();
    assert_eq!((resting.price, resting.size), (px(DISPLAY_TICKS), 3));
    assert_eq!(book.level(Side::Bid, px(DISPLAY_TICKS)).total_size, 3);
    assert_eq!(book.level(Side::Bid, px(EXECUTION_TICKS)).traded_5s, 2);
    assert_eq!(book.fills_off_display(), 1);
    let inside = book.traded_at_inside();
    assert_eq!(
        (inside.bid_price, inside.bid_vol),
        (Some(px(EXECUTION_TICKS)), 2)
    );
    assert_eq!((inside.ask_price, inside.ask_vol), (None, 0));
    let last = book.last_trade().unwrap();
    assert_eq!(
        (last.price, last.size, last.aggressor),
        (px(EXECUTION_TICKS), 2, Side::Ask)
    );

    book.apply(&cancel);
    assert!(book.queue_position(OrderId(ORDER_ID)).is_none());
    assert_eq!(book.level(Side::Bid, px(DISPLAY_TICKS)).total_size, 0);
    assert_eq!(book.level(Side::Bid, px(DISPLAY_TICKS)).cancelled_5s, 3);
    assert_eq!(book.unknown_ref_events(), 0);
    book.check_invariants();
}

#[test]
#[should_panic(expected = "Cancel size 6 > resting 5")]
fn cancel_quantity_above_displayed_size_panics() {
    let mut book = book();
    book.apply(&add(1, Side::Bid, 100, 5, T0));
    book.apply(&cancel(1, Side::Bid, 100, 6, T0 + 1));
}

#[test]
fn opening_auction_unknown_sideless_fill_records_unattributed_tape() {
    let fill = ev(
        EventKind::Fill,
        Side::None,
        SIDELESS_EXECUTION_TICKS,
        1,
        SIDELESS_ORDER_ID,
        SIDELESS_AUCTION_TS,
        28_634_417,
    );
    let mut book = book();
    book.apply(&fill);

    let last = book.last_trade().unwrap();
    assert_eq!(
        (last.price, last.size, last.aggressor),
        (px(SIDELESS_EXECUTION_TICKS), 1, Side::None)
    );
    assert_eq!(book.traded_at_inside(), Default::default());
    assert_eq!(
        book.level(Side::Bid, px(SIDELESS_EXECUTION_TICKS))
            .traded_5s,
        0
    );
    assert_eq!(
        book.level(Side::Ask, px(SIDELESS_EXECUTION_TICKS))
            .traded_5s,
        0
    );
    assert_eq!(book.unknown_ref_events(), 1);
    assert_eq!(book.live_order_count(), 0);
    book.check_invariants();
}

#[test]
fn known_sideless_fill_resolves_resting_side_and_depletes() {
    let mut book = book();
    book.apply(&add(1, Side::Bid, 100, 3, T0));
    book.apply(&fill(1, Side::None, 100, 3, T0 + 1));

    assert_eq!(book.queue_position(OrderId(1)).unwrap().size, 3);
    assert_eq!(book.level(Side::Bid, px(100)).traded_5s, 3);
    let inside = book.traded_at_inside();
    assert_eq!((inside.bid_price, inside.bid_vol), (Some(px(100)), 3));
    assert_eq!(book.last_trade().unwrap().aggressor, Side::Ask);
    assert_eq!(book.unknown_ref_events(), 0);

    book.apply(&cancel(1, Side::Bid, 100, 3, T0 + 2));
    book.apply(&add(1, Side::Bid, 100, 4, T0 + 3));
    assert_eq!(
        book.refresh_state(OrderId(1)),
        RefreshState::Known {
            native: true,
            reloads: 1,
            hidden_volume: 4,
        }
    );
    book.check_invariants();
}

#[test]
fn unknown_sided_fill_records_tape_flow_and_unknown_ref() {
    let mut book = book();
    book.apply(&fill(99, Side::Ask, 101, 2, T0));

    let last = book.last_trade().unwrap();
    assert_eq!(
        (last.price, last.size, last.aggressor),
        (px(101), 2, Side::Bid)
    );
    let inside = book.traded_at_inside();
    assert_eq!((inside.ask_price, inside.ask_vol), (Some(px(101)), 2));
    assert_eq!(book.level(Side::Ask, px(101)).traded_5s, 2);
    assert_eq!(book.unknown_ref_events(), 1);
    assert_eq!(book.live_order_count(), 0);
    book.check_invariants();
}

#[test]
fn sideless_trade_is_inert_and_tolerated() {
    let mut book = book();
    book.apply(&trade(Side::None, 100, 1, T0));
    assert_eq!(book.last_trade(), None);
    assert_eq!(book.traded_at_inside(), Default::default());
    assert_eq!(book.unknown_ref_events(), 0);
}
