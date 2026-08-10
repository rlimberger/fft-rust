//! serialize → restore → serialize is byte-identical, and the restored book
//! equals the live book order-exactly: ids, FIFO traversal, contracts/orders
//! ahead, refresh state, flow window, sequence state. Never replayed events.

mod common;

use common::*;
use fft_book::Book;
use fft_core::{EventKind, OrderId, Side};

/// A busy book exercising every serialized feature: multi-level FIFO queues,
/// far-map levels, flow counters on emptied levels, partial fills, a classified
/// refresh, a pending (mid-cycle) tombstone, tape state, a gap, and post-gap
/// re-anchored sequencing.
fn busy_book() -> Book {
    let mut b = book();
    let mut seq = 100u32;
    let mut s = |k: EventKind, side: Side, t: i64, sz: u32, id: u64, ts: u64| {
        seq += 1;
        ev(k, side, t, sz, id, ts, seq)
    };
    b.apply(&s(EventKind::Add, Side::Bid, 1000, 10, 1, T0));
    b.apply(&s(EventKind::Add, Side::Bid, 1000, 20, 2, T0 + 1));
    b.apply(&s(EventKind::Add, Side::Bid, 999, 5, 3, T0 + 2));
    b.apply(&s(EventKind::Add, Side::Ask, 1001, 7, 4, T0 + 3));
    b.apply(&s(EventKind::Add, Side::Ask, 1002, 9, 5, T0 + 4));
    // Far-map levels: way off the 512-tick window around ~1000.
    b.apply(&s(EventKind::Add, Side::Bid, 100, 3, 6, T0 + 5));
    b.apply(&s(EventKind::Add, Side::Ask, 2000, 4, 7, T0 + 6));
    // Flow on an emptied level (order gone, pulls window alive).
    b.apply(&s(EventKind::Add, Side::Bid, 998, 8, 8, T0 + 7));
    b.apply(&s(EventKind::Cancel, Side::None, 0, 0, 8, T0 + 8));
    // Partial fill + tape.
    b.apply(&s(EventKind::Fill, Side::Ask, 1001, 3, 4, T0 + 9));
    b.apply(&s(EventKind::Trade, Side::Bid, 1001, 3, 0, T0 + 10));
    b.apply(&s(EventKind::Trade, Side::Ask, 1000, 2, 0, T0 + 11));
    // A classified native refresh on id 2.
    b.apply(&s(EventKind::Fill, Side::Bid, 1000, 20, 2, T0 + 12));
    b.apply(&s(EventKind::Modify, Side::Bid, 1000, 15, 2, T0 + 13));
    // A gap, then re-anchored post-gap activity (any next seq is legal —
    // sequencing re-anchors on the first sequenced event after a gap).
    b.apply(&gap(T0 + 14, 115, 220));
    b.apply(&s(EventKind::Add, Side::Bid, 999, 6, 9, T0 + 15));
    // A pending tombstone: id 9 depleted right before the checkpoint.
    b.apply(&s(EventKind::Fill, Side::Bid, 999, 6, 9, T0 + 16));
    // An unknown reference (counted, not fatal).
    b.apply(&s(EventKind::Cancel, Side::None, 0, 0, 777, T0 + 17));
    b.check_invariants();
    b
}

fn all_ids(b: &Book) -> Vec<u64> {
    let mut ids = Vec::new();
    for side in [Side::Bid, Side::Ask] {
        b.for_each_level(side, |p, _| {
            b.for_each_order_at(side, p, |id, _| ids.push(id.0));
        });
    }
    ids
}

fn assert_books_equal(a: &Book, b: &Book) {
    assert_eq!(a.best_bid(), b.best_bid());
    assert_eq!(a.best_ask(), b.best_ask());
    assert_eq!(a.last_trade(), b.last_trade());
    assert_eq!(a.traded_at_inside(), b.traded_at_inside());
    assert_eq!(a.live_order_count(), b.live_order_count());
    assert_eq!(a.populated_levels(), b.populated_levels());
    assert_eq!(a.last_event_ts(), b.last_event_ts());
    assert_eq!(a.last_seq(), b.last_seq());
    assert_eq!(a.gap_pending(), b.gap_pending());
    assert_eq!(a.gaps_seen(), b.gaps_seen());
    assert_eq!(a.last_gap(), b.last_gap());
    assert_eq!(a.unknown_ref_events(), b.unknown_ref_events());
    for side in [Side::Bid, Side::Ask] {
        let mut la = Vec::new();
        a.for_each_level(side, |p, v| la.push((p, v)));
        let mut lb = Vec::new();
        b.for_each_level(side, |p, v| lb.push((p, v)));
        assert_eq!(la, lb, "{side:?} level views (incl. flow window)");
        for &(p, _) in &la {
            let mut fa = Vec::new();
            a.for_each_order_at(side, p, |id, sz| fa.push((id, sz)));
            let mut fb = Vec::new();
            b.for_each_order_at(side, p, |id, sz| fb.push((id, sz)));
            assert_eq!(fa, fb, "FIFO at {side:?} {p:?}");
        }
    }
    for id in all_ids(a) {
        assert_eq!(
            a.queue_position(OrderId(id)),
            b.queue_position(OrderId(id)),
            "queue position of {id}"
        );
        assert_eq!(
            a.refresh_state(OrderId(id)),
            b.refresh_state(OrderId(id)),
            "refresh state of {id}"
        );
    }
    for t in [100i64, 998, 999, 1000, 1001, 1002, 2000] {
        for side in [Side::Bid, Side::Ask] {
            assert_eq!(a.refresh_at(side, px(t)), b.refresh_at(side, px(t)));
        }
    }
}

#[test]
fn roundtrip_is_byte_identical_and_order_exact() {
    let live = busy_book();
    let bytes = live.serialize();
    let restored = Book::restore(&bytes);
    restored.check_invariants();
    assert_eq!(
        bytes,
        restored.serialize(),
        "serialize∘restore∘serialize ≠ id"
    );
    assert_books_equal(&live, &restored);
}

/// The two books must also behave identically going forward — including the
/// mid-cycle tombstone: id 9 depleted before the checkpoint and restored after
/// it must classify as a native refresh in both.
#[test]
fn restored_book_behaves_identically() {
    let mut live = busy_book();
    let mut restored = Book::restore(&live.serialize());
    let follow_on = [
        modify(9, Side::Bid, 999, 12, T0 + 18), // tombstone restore across the checkpoint
        add(20, Side::Bid, 1000, 4, T0 + 19),
        fill(1, Side::Bid, 1000, 10, T0 + 20),
        modify(5, Side::Ask, 1003, 9, T0 + 21),
        cancel(3, T0 + 22),
        trade(Side::Bid, 1001, 1, T0 + 23),
    ];
    for e in &follow_on {
        live.apply(e);
        restored.apply(e);
    }
    live.check_invariants();
    restored.check_invariants();
    assert_books_equal(&live, &restored);
    assert_eq!(live.serialize(), restored.serialize());
    assert_eq!(
        live.refresh_state(OrderId(9)),
        fft_book::RefreshState::Known {
            native: true,
            reloads: 1,
            hidden_volume: 12
        }
    );
}

#[test]
fn empty_book_roundtrips() {
    let live = book();
    let bytes = live.serialize();
    let restored = Book::restore(&bytes);
    assert_eq!(bytes, restored.serialize());
    assert_eq!(restored.live_order_count(), 0);
    assert_eq!(restored.best_bid(), None);
}

#[test]
#[should_panic(expected = "BOOK section version")]
fn version_mismatch_panics() {
    let mut bytes = busy_book().serialize();
    bytes[0] = 99;
    let _ = Book::restore(&bytes);
}

#[test]
#[should_panic(expected = "truncated BOOK section")]
fn truncated_payload_panics() {
    let bytes = busy_book().serialize();
    let _ = Book::restore(&bytes[..bytes.len() - 3]);
}
