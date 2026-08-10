//! Databento snapshot rows load state without pretending to be observed flow.

mod common;

use common::*;
use fft_book::{Book, PriceRefreshAgg, RefreshState};
use fft_core::{CanonicalEvent, EventKind, OrderId, Side};

const SNAPSHOT_FLAG: u16 = 1 << 5;

fn snapshot(id: u64, side: Side, ticks: i64, size: u32, ts: u64, seq: u32) -> CanonicalEvent {
    let mut event = ev(EventKind::Add, side, ticks, size, id, ts, seq);
    event.flags = SNAPSHOT_FLAG;
    event
}

fn fifo(book: &Book, side: Side, ticks: i64) -> Vec<u64> {
    let mut ids = Vec::new();
    book.for_each_order_at(side, px(ticks), |id, _| ids.push(id.0));
    ids
}

#[test]
fn snapshot_seq_regression_bypasses_sequence_accounting() {
    let mut book = book();
    book.apply(&ev(EventKind::Add, Side::Bid, 100, 5, 1, T0, 50));
    book.apply(&snapshot(2, Side::Bid, 100, 6, T0 - 10, 7));
    assert_eq!(book.last_seq(), Some(50));
    assert!(!book.gap_pending());
    assert_eq!(fifo(&book, Side::Bid, 100), [2, 1]);

    book.apply(&gap(T0 + 1, 51, 60));
    book.apply(&snapshot(3, Side::Bid, 100, 7, T0 - 9, 6));
    assert_eq!(book.last_seq(), None);
    assert!(book.gap_pending());
    assert_eq!(book.last_gap(), Some((51, 60)));
}

#[test]
#[should_panic(expected = "seq regression")]
fn equivalent_live_seq_regression_panics() {
    let mut book = book();
    book.apply(&ev(EventKind::Add, Side::Bid, 100, 5, 1, T0, 50));
    book.apply(&ev(EventKind::Add, Side::Bid, 100, 6, 2, T0 + 1, 7));
}

#[test]
fn snapshot_fifo_prefix_and_ranks_survive_priority_loss_and_restore() {
    let mut book = book();
    book.apply(&add(1, Side::Bid, 100, 10, T0));
    book.apply(&add(2, Side::Bid, 100, 20, T0 + 1));
    book.apply(&snapshot(3, Side::Bid, 100, 30, T0 - 2, 5));
    book.apply(&snapshot(4, Side::Bid, 100, 40, T0 - 1, 4));
    assert_eq!(fifo(&book, Side::Bid, 100), [3, 4, 1, 2]);
    for (id, rank, ahead) in [(3, 1, 0), (4, 2, 30), (1, 3, 70), (2, 4, 80)] {
        let q = book.queue_position(OrderId(id)).unwrap();
        assert_eq!((q.rank, q.contracts_ahead), (rank, ahead));
    }

    book.apply(&modify(3, Side::Bid, 100, 31, T0 + 2));
    book.apply(&snapshot(5, Side::Bid, 100, 50, T0 - 3, 3));
    assert_eq!(fifo(&book, Side::Bid, 100), [4, 5, 1, 2, 3]);

    let restored = Book::restore(
        &book.serialize_book(),
        &book.serialize_flow(),
        &book.serialize_refresh(),
    )
    .unwrap();
    assert_eq!(fifo(&restored, Side::Bid, 100), [4, 5, 1, 2, 3]);
    let mut restored = restored;
    restored.apply(&snapshot(6, Side::Bid, 100, 60, T0 - 4, 2));
    assert_eq!(fifo(&restored, Side::Bid, 100), [4, 5, 6, 1, 2, 3]);

    restored.apply(&add(7, Side::Bid, 99, 7, T0 + 3));
    restored.apply(&modify(4, Side::Bid, 99, 40, T0 + 4));
    restored.apply(&snapshot(8, Side::Bid, 99, 80, T0 - 5, 1));
    assert_eq!(fifo(&restored, Side::Bid, 99), [8, 7, 4]);
    restored.check_invariants();
}

fn assert_panics_with(event: CanonicalEvent, expected: &str) {
    let result = std::panic::catch_unwind(|| {
        let mut book = book();
        book.apply(&add(1, Side::Bid, 100, 5, T0));
        book.apply(&event);
    });
    let payload = result.expect_err("event should panic");
    let message = payload
        .downcast_ref::<String>()
        .map(String::as_str)
        .or_else(|| payload.downcast_ref::<&str>().copied())
        .unwrap_or("non-string panic");
    assert!(
        message.contains(expected),
        "panic {message:?} did not contain {expected:?}"
    );
}

#[test]
fn known_snapshot_mismatches_panic_with_context() {
    assert_panics_with(snapshot(1, Side::Ask, 100, 5, T0 + 1, 1), "side mismatch");
    assert_panics_with(snapshot(1, Side::Bid, 101, 5, T0 + 1, 1), "price mismatch");
    assert_panics_with(snapshot(1, Side::Bid, 100, 6, T0 + 1, 1), "size mismatch");
}

#[test]
fn malformed_snapshot_adds_panic_during_validation() {
    assert_panics_with(snapshot(2, Side::None, 100, 5, T0 + 1, 1), "without side");
    assert_panics_with(snapshot(2, Side::Bid, 100, 0, T0 + 1, 1), "size 0");
    let mut off_tick = snapshot(2, Side::Bid, 100, 5, T0 + 1, 1);
    off_tick.price.0 += 1;
    assert_panics_with(off_tick, "not aligned to tick");
}

#[test]
fn matching_known_snapshot_is_a_noop() {
    let mut book = book();
    book.apply(&ev(EventKind::Add, Side::Bid, 100, 5, 1, T0, 50));
    let before = (
        book.serialize_book(),
        book.serialize_flow(),
        book.serialize_refresh(),
    );
    book.apply(&snapshot(1, Side::Bid, 100, 5, T0 - 1, 2));
    assert_eq!(
        (
            book.serialize_book(),
            book.serialize_flow(),
            book.serialize_refresh(),
        ),
        before
    );
}

#[test]
#[should_panic(expected = "snapshot-flagged event must be Add")]
fn non_add_snapshot_panics_before_sequence_accounting() {
    let mut book = book();
    book.apply(&ev(EventKind::Add, Side::Bid, 100, 5, 1, T0, 50));
    let mut event = ev(EventKind::Trade, Side::Ask, 100, 1, 0, T0 + 1, 1);
    event.flags = SNAPSHOT_FLAG;
    book.apply(&event);
}

#[test]
fn snapshot_load_does_not_create_flow_tape_or_refresh_evidence() {
    let mut book = book();
    book.apply(&add(1, Side::Bid, 100, 5, T0));
    book.apply(&trade(Side::Ask, 100, 2, T0 + 1));
    book.apply(&fill(1, Side::Bid, 100, 2, T0 + 1));
    let flow_before = book.level(Side::Bid, px(100));
    let tape_before = book.last_trade();
    let inside_before = book.traded_at_inside();

    book.apply(&snapshot(2, Side::Bid, 100, 7, T0 - 1, 1));
    let flow_after = book.level(Side::Bid, px(100));
    assert_eq!(flow_after.added_5s, flow_before.added_5s);
    assert_eq!(flow_after.cancelled_5s, flow_before.cancelled_5s);
    assert_eq!(flow_after.traded_5s, flow_before.traded_5s);
    assert_eq!(book.last_trade(), tape_before);
    assert_eq!(book.traded_at_inside(), inside_before);
    assert_eq!(book.refresh_state(OrderId(2)), RefreshState::Unavailable);
    assert_eq!(
        book.refresh_at(Side::Bid, px(100)),
        PriceRefreshAgg::default()
    );

    // An unrelated pending candidate remains classifiable after the load.
    book.apply(&add(3, Side::Bid, 99, 4, T0 + 2));
    book.apply(&fill(3, Side::Bid, 99, 4, T0 + 3));
    book.apply(&snapshot(4, Side::Bid, 99, 6, T0 - 2, 2));
    book.apply(&modify(3, Side::Bid, 99, 8, T0 + 4));
    assert_eq!(
        book.refresh_state(OrderId(3)),
        RefreshState::Known {
            native: true,
            reloads: 1,
            hidden_volume: 8,
        }
    );
    assert_eq!(book.refresh_at(Side::Bid, px(99)).refresh_count, 1);
    book.check_invariants();
}

#[test]
fn same_id_tombstone_is_discarded_without_classification() {
    let mut book = book();
    book.apply(&add(7, Side::Ask, 105, 4, T0));
    book.apply(&fill(7, Side::Ask, 105, 4, T0 + 1));
    book.apply(&cancel(7, Side::Ask, 105, 4, T0 + 2));
    book.apply(&snapshot(7, Side::Ask, 105, 6, T0 - 1, 1));
    assert_eq!(book.refresh_state(OrderId(7)), RefreshState::Unavailable);
    assert_eq!(
        book.refresh_at(Side::Ask, px(105)),
        PriceRefreshAgg::default()
    );
    book.check_invariants();
}

#[test]
fn snapshot_order_fill_progress_survives_restore() {
    let mut book = book();
    book.apply(&snapshot(7, Side::Ask, 105, 4, T0 - 1, 1));
    book.apply(&fill(7, Side::Ask, 105, 4, T0 + 1));

    let restored = Book::restore(
        &book.serialize_book(),
        &book.serialize_flow(),
        &book.serialize_refresh(),
    )
    .unwrap();
    assert_eq!(restored.queue_position(OrderId(7)).unwrap().size, 4);
    assert_eq!(
        restored.refresh_state(OrderId(7)),
        RefreshState::Unavailable
    );
    assert_eq!(restored.serialize_refresh(), book.serialize_refresh());
    restored.check_invariants();
}
