//! Three-section checkpoint roundtrips and defensive restore behavior.

mod common;

use common::*;
use fft_book::{Book, RestoreError};
use fft_core::{EventKind, OrderId, Side};

const SNAPSHOT_FLAG: u16 = 1 << 5;

fn snapshot(id: u64, side: Side, ticks: i64, size: u32, ts: u64) -> fft_core::CanonicalEvent {
    let mut event = add(id, side, ticks, size, ts);
    event.flags = SNAPSHOT_FLAG;
    event
}

fn sections(book: &Book) -> (Vec<u8>, Vec<u8>, Vec<u8>) {
    (
        book.serialize_book(),
        book.serialize_flow(),
        book.serialize_refresh(),
    )
}

/// Exercises FIFO, snapshot-prefix origin, far levels, fresh empty flow,
/// refresh history, a pending candidate, tape state, and a sequence gap.
fn busy_book() -> Book {
    let mut book = book();
    let mut seq = 100u32;
    let mut sequenced = |kind: EventKind, side: Side, ticks: i64, size: u32, id: u64, ts: u64| {
        seq += 1;
        ev(kind, side, ticks, size, id, ts, seq)
    };
    book.apply(&sequenced(EventKind::Add, Side::Bid, 1000, 10, 1, T0));
    book.apply(&sequenced(EventKind::Add, Side::Bid, 1000, 20, 2, T0 + 1));
    book.apply(&snapshot(10, Side::Bid, 1000, 30, T0 - 1));
    book.apply(&sequenced(EventKind::Add, Side::Bid, 999, 5, 3, T0 + 2));
    book.apply(&sequenced(EventKind::Add, Side::Ask, 1001, 7, 4, T0 + 3));
    book.apply(&sequenced(EventKind::Add, Side::Ask, 1002, 9, 5, T0 + 4));
    book.apply(&sequenced(EventKind::Add, Side::Bid, 100, 3, 6, T0 + 5));
    book.apply(&sequenced(EventKind::Add, Side::Ask, 2000, 4, 7, T0 + 6));
    book.apply(&sequenced(EventKind::Add, Side::Bid, 998, 8, 8, T0 + 7));
    book.apply(&sequenced(EventKind::Cancel, Side::Bid, 998, 8, 8, T0 + 8));
    book.apply(&sequenced(EventKind::Fill, Side::Ask, 1001, 3, 4, T0 + 9));
    book.apply(&sequenced(EventKind::Trade, Side::Bid, 1001, 3, 0, T0 + 10));
    book.apply(&sequenced(EventKind::Trade, Side::Ask, 1000, 2, 0, T0 + 11));
    book.apply(&sequenced(EventKind::Fill, Side::Bid, 1000, 20, 2, T0 + 12));
    book.apply(&sequenced(
        EventKind::Modify,
        Side::Bid,
        1000,
        15,
        2,
        T0 + 13,
    ));
    book.apply(&gap(T0 + 14, 115, 220));
    book.apply(&sequenced(EventKind::Add, Side::Bid, 999, 6, 9, T0 + 15));
    book.apply(&sequenced(EventKind::Fill, Side::Bid, 999, 6, 9, T0 + 16));
    book.apply(&sequenced(EventKind::Cancel, Side::Bid, 999, 6, 9, T0 + 17));
    book.apply(&sequenced(
        EventKind::Cancel,
        Side::Bid,
        998,
        1,
        777,
        T0 + 18,
    ));
    book.apply(&sequenced(EventKind::Add, Side::Ask, 1002, 4, 11, T0 + 19));
    book.apply(&sequenced(EventKind::Fill, Side::Ask, 1002, 4, 11, T0 + 20));
    book.check_invariants();
    book
}

fn all_ids(book: &Book) -> Vec<u64> {
    let mut ids = Vec::new();
    for side in [Side::Bid, Side::Ask] {
        book.for_each_level(side, |price, _| {
            book.for_each_order_at(side, price, |id, _| ids.push(id.0));
        });
    }
    ids
}

fn assert_books_equal(left: &Book, right: &Book) {
    assert_eq!(left.best_bid(), right.best_bid());
    assert_eq!(left.best_ask(), right.best_ask());
    assert_eq!(left.last_trade(), right.last_trade());
    assert_eq!(left.traded_at_inside(), right.traded_at_inside());
    assert_eq!(left.last_event_ts(), right.last_event_ts());
    assert_eq!(left.last_seq(), right.last_seq());
    assert_eq!(left.gap_pending(), right.gap_pending());
    assert_eq!(left.gaps_seen(), right.gaps_seen());
    assert_eq!(left.last_gap(), right.last_gap());
    assert_eq!(left.unknown_ref_events(), right.unknown_ref_events());
    assert_eq!(left.fills_off_display(), right.fills_off_display());
    for side in [Side::Bid, Side::Ask] {
        let mut left_levels = Vec::new();
        left.for_each_level(side, |price, view| left_levels.push((price, view)));
        let mut right_levels = Vec::new();
        right.for_each_level(side, |price, view| right_levels.push((price, view)));
        assert_eq!(left_levels, right_levels);
        for &(price, _) in &left_levels {
            let mut left_fifo = Vec::new();
            left.for_each_order_at(side, price, |id, size| left_fifo.push((id, size)));
            let mut right_fifo = Vec::new();
            right.for_each_order_at(side, price, |id, size| right_fifo.push((id, size)));
            assert_eq!(left_fifo, right_fifo);
        }
    }
    for id in all_ids(left) {
        assert_eq!(
            left.queue_position(OrderId(id)),
            right.queue_position(OrderId(id))
        );
        assert_eq!(
            left.refresh_state(OrderId(id)),
            right.refresh_state(OrderId(id))
        );
    }
    assert_eq!(sections(left), sections(right));
}

#[test]
fn each_section_roundtrips_byte_identically() {
    let live = busy_book();
    let (book, flow, refresh) = sections(&live);
    let restored = Book::restore(&book, &flow, &refresh).unwrap();
    restored.check_invariants();
    assert_eq!(restored.serialize_book(), book);
    assert_eq!(restored.serialize_flow(), flow);
    assert_eq!(restored.serialize_refresh(), refresh);
    assert_books_equal(&live, &restored);
}

#[test]
fn restored_tail_matches_forward_with_snapshot_flow_refresh_and_gap() {
    let mut forward = busy_book();
    let (book, flow, refresh) = sections(&forward);
    let mut restored = Book::restore(&book, &flow, &refresh).unwrap();
    let tail = [
        modify(11, Side::Ask, 1002, 6, T0 + 21),
        modify(9, Side::Bid, 999, 12, T0 + 22),
        add(20, Side::Bid, 1000, 4, T0 + 23),
        fill(1, Side::Bid, 1000, 10, T0 + 24),
        modify(5, Side::Ask, 1003, 9, T0 + 25),
        cancel(3, Side::Bid, 999, 5, T0 + 26),
        trade(Side::Bid, 1001, 1, T0 + 27),
    ];
    for event in &tail {
        forward.apply(event);
        restored.apply(event);
    }
    forward.check_invariants();
    restored.check_invariants();
    assert_books_equal(&forward, &restored);
}

/// Regression for REFRESH-SEEK-DIVERGE: expired tombstones must not survive a
/// restore+tail differently than a pure forward pass. Root cause was interval
/// GC (`since_gc` resets to 0 on restore) leaving multi-second-old tombs on
/// the seek path while forward had already swept them — BOOK/FLOW matched,
/// REFRESH section bytes did not.
#[test]
fn expired_refresh_tombstones_match_across_restore_tail() {
    let mut forward = book();
    // Arm a tombstone at T0, then advance event time far past REFRESH_WINDOW_NS
    // without hitting the old 4096-event GC phase on a restored book.
    forward.apply(&add(1, Side::Bid, 100, 5, T0));
    forward.apply(&fill(1, Side::Bid, 100, 5, T0 + 1));
    forward.apply(&cancel(1, Side::Bid, 100, 5, T0 + 2));
    // Unrelated live book activity so serialize has resting depth + now.
    forward.apply(&add(2, Side::Bid, 99, 3, T0 + 3));
    forward.apply(&add(3, Side::Ask, 101, 4, T0 + 4));

    let (book_bytes, flow_bytes, refresh_bytes) = sections(&forward);
    // Inject an expired tombstone into the REFRESH payload as old checkpoints
    // could (interval GC had not yet run at write time, or a later now was
    // not re-swept). Layout: after live count, tombstone block starts.
    // Simpler: build a second book path that restores then advances time with
    // fewer than GC_INTERVAL events — the bug class.
    let mut restored = Book::restore(&book_bytes, &flow_bytes, &refresh_bytes).unwrap();

    // Advance both past the 1 ms window with a short tail (≪ 4096 events).
    let tail_ts = T0 + 3 + fft_book::REFRESH_WINDOW_NS + 10;
    let tail = [
        add(4, Side::Bid, 98, 1, tail_ts),
        cancel(4, Side::Bid, 98, 1, tail_ts + 1),
        add(5, Side::Ask, 102, 2, tail_ts + 2),
    ];
    for event in &tail {
        forward.apply(event);
        restored.apply(event);
    }
    forward.check_invariants();
    restored.check_invariants();
    assert_eq!(forward.serialize_refresh(), restored.serialize_refresh());
    assert_books_equal(&forward, &restored);

    // Late same-id restore must be a new life on both paths (window expired).
    let late = add(1, Side::Bid, 100, 5, tail_ts + 3);
    forward.apply(&late);
    restored.apply(&late);
    assert_eq!(
        forward.refresh_state(OrderId(1)),
        restored.refresh_state(OrderId(1))
    );
    assert_eq!(forward.serialize_refresh(), restored.serialize_refresh());
}

/// Serialize must omit tombstones past the acceptance window even if the
/// in-memory map still holds them (defense against residual phase lag).
#[test]
fn serialize_refresh_omits_expired_tombstones() {
    let mut b = book();
    b.apply(&add(1, Side::Bid, 100, 5, T0));
    b.apply(&fill(1, Side::Bid, 100, 5, T0 + 1));
    b.apply(&cancel(1, Side::Bid, 100, 5, T0 + 2));
    b.apply(&add(2, Side::Bid, 99, 1, T0 + 3));
    // Force clock past window with a tiny tail; every-apply GC should already
    // have dropped the tomb — re-serialize stays empty of id 1 candidacy.
    b.apply(&add(
        3,
        Side::Ask,
        101,
        1,
        T0 + 3 + fft_book::REFRESH_WINDOW_NS + 1,
    ));
    let refresh = b.serialize_refresh();
    // Round-trip restore must accept and leave no stale candidacy for id 1.
    let mut restored = Book::restore(&b.serialize_book(), &b.serialize_flow(), &refresh).unwrap();
    restored.apply(&add(
        1,
        Side::Bid,
        100,
        5,
        T0 + 3 + fft_book::REFRESH_WINDOW_NS + 2,
    ));
    assert_eq!(
        restored.refresh_state(OrderId(1)),
        fft_book::RefreshState::Known {
            native: false,
            reloads: 0,
            hidden_volume: 0,
        }
    );
}

#[test]
fn inner_versions_are_checked_independently() {
    let (book, flow, refresh) = sections(&busy_book());
    for (section, mut bad_book, mut bad_flow, mut bad_refresh) in [
        ("BOOK", book.clone(), flow.clone(), refresh.clone()),
        ("FLOW", book.clone(), flow.clone(), refresh.clone()),
        ("REFRESH", book.clone(), flow.clone(), refresh.clone()),
    ] {
        match section {
            "BOOK" => bad_book[..2].copy_from_slice(&99u16.to_le_bytes()),
            "FLOW" => bad_flow[..2].copy_from_slice(&99u16.to_le_bytes()),
            "REFRESH" => bad_refresh[..2].copy_from_slice(&99u16.to_le_bytes()),
            _ => unreachable!(),
        }
        assert_eq!(
            Book::restore(&bad_book, &bad_flow, &bad_refresh).unwrap_err(),
            RestoreError::UnsupportedVersion {
                section,
                version: 99,
            }
        );
    }
}

#[test]
fn malformed_sections_return_typed_errors_without_panicking() {
    let (book, flow, refresh) = sections(&busy_book());
    let cases = [
        (
            &book[..book.len() - 1],
            flow.as_slice(),
            refresh.as_slice(),
            RestoreError::Truncated { section: "BOOK" },
        ),
        (
            book.as_slice(),
            &flow[..flow.len() - 1],
            refresh.as_slice(),
            RestoreError::Truncated { section: "FLOW" },
        ),
        (
            book.as_slice(),
            flow.as_slice(),
            &refresh[..refresh.len() - 1],
            RestoreError::Truncated { section: "REFRESH" },
        ),
    ];
    for (book, flow, refresh, expected) in cases {
        let result = std::panic::catch_unwind(|| Book::restore(book, flow, refresh));
        assert_eq!(
            result
                .expect("restore must not unwind")
                .expect_err("malformed section must fail"),
            expected
        );
    }

    let mut corrupt_book = book.clone();
    corrupt_book[27] = 2;
    assert_eq!(
        Book::restore(&corrupt_book, &flow, &refresh).unwrap_err(),
        RestoreError::Corrupt {
            section: "BOOK",
            what: "invalid boolean",
        }
    );
    let mut corrupt_flow = flow.clone();
    corrupt_flow[2] = 2;
    assert_eq!(
        Book::restore(&book, &corrupt_flow, &refresh).unwrap_err(),
        RestoreError::Corrupt {
            section: "FLOW",
            what: "invalid boolean",
        }
    );
    let mut corrupt_refresh = refresh.clone();
    corrupt_refresh.push(0);
    assert_eq!(
        Book::restore(&book, &flow, &corrupt_refresh).unwrap_err(),
        RestoreError::Corrupt {
            section: "REFRESH",
            what: "trailing bytes",
        }
    );
}

fn book_with_depleted_fill() -> Book {
    let mut book = book();
    book.apply(&add(1, Side::Bid, 100, 5, T0));
    book.apply(&fill(1, Side::Bid, 100, 5, T0 + 1));
    book
}

#[test]
fn malformed_refresh_v2_fill_progress_is_rejected() {
    let (book, flow, refresh) = sections(&book_with_depleted_fill());

    let mut zero_qty = refresh.clone();
    zero_qty[34..42].copy_from_slice(&0u64.to_le_bytes());
    assert_eq!(
        Book::restore(&book, &flow, &zero_qty).unwrap_err(),
        RestoreError::Corrupt {
            section: "REFRESH",
            what: "zero Fill progress quantity",
        }
    );

    let mut unknown_id = refresh.clone();
    unknown_id[26..34].copy_from_slice(&2u64.to_le_bytes());
    assert_eq!(
        Book::restore(&book, &flow, &unknown_id).unwrap_err(),
        RestoreError::Corrupt {
            section: "REFRESH",
            what: "Fill progress has no resting order",
        }
    );

    let mut future_epoch = refresh.clone();
    future_epoch[51..55].copy_from_slice(&1u32.to_le_bytes());
    assert_eq!(
        Book::restore(&book, &flow, &future_epoch).unwrap_err(),
        RestoreError::Corrupt {
            section: "REFRESH",
            what: "inconsistent Fill progress ownership",
        }
    );

    let mut partial_with_time = refresh;
    partial_with_time[42] = 0;
    assert_eq!(
        Book::restore(&book, &flow, &partial_with_time).unwrap_err(),
        RestoreError::Corrupt {
            section: "REFRESH",
            what: "partial Fill has depletion time",
        }
    );
}

#[test]
fn off_display_fill_counter_roundtrips_in_refresh_v2() {
    let mut live = book();
    live.apply(&add(1, Side::Bid, 100, 5, T0));
    live.apply(&fill(1, Side::Bid, 101, 2, T0 + 1));
    assert_eq!(live.fills_off_display(), 1);

    let (book, flow, refresh) = sections(&live);
    let restored = Book::restore(&book, &flow, &refresh).unwrap();
    assert_eq!(restored.fills_off_display(), 1);
    assert_eq!(restored.serialize_refresh(), refresh);
}

#[test]
fn every_single_byte_mutation_is_caught_without_unwinding() {
    let (book, flow, refresh) = sections(&busy_book());
    for (section, original) in [("BOOK", &book), ("FLOW", &flow), ("REFRESH", &refresh)] {
        for byte in 0..original.len() {
            let mut mutated = original.clone();
            mutated[byte] ^= 0xff;
            let result = std::panic::catch_unwind(|| match section {
                "BOOK" => Book::restore(&mutated, &flow, &refresh),
                "FLOW" => Book::restore(&book, &mutated, &refresh),
                "REFRESH" => Book::restore(&book, &flow, &mutated),
                _ => unreachable!(),
            });
            assert!(
                result.is_ok(),
                "{section} byte {byte} mutation escaped typed restore"
            );
        }
    }
}

fn tombstone_book(reverse: bool) -> Book {
    let mut book = book();
    book.apply(&add(1, Side::Bid, 100, 5, T0));
    book.apply(&add(2, Side::Bid, 100, 7, T0));
    book.apply(&fill(1, Side::Bid, 100, 5, T0 + 1));
    book.apply(&fill(2, Side::Bid, 100, 7, T0 + 2));
    if reverse {
        book.apply(&cancel(1, Side::Bid, 100, 5, T0 + 3));
        book.apply(&cancel(2, Side::Bid, 100, 7, T0 + 4));
    } else {
        book.apply(&cancel(2, Side::Bid, 100, 7, T0 + 4));
        book.apply(&cancel(1, Side::Bid, 100, 5, T0 + 3));
    }
    book
}

#[test]
fn hash_insertion_order_does_not_change_checkpoint_bytes() {
    let forward = tombstone_book(false);
    let reverse = tombstone_book(true);
    assert_eq!(forward.serialize_book(), reverse.serialize_book());
    assert_eq!(forward.serialize_flow(), reverse.serialize_flow());
    assert_eq!(forward.serialize_refresh(), reverse.serialize_refresh());
}

#[test]
fn flow_omits_untouched_and_stale_levels_but_keeps_fresh_empty() {
    let empty_len = book().serialize_flow().len();
    let mut book = book();
    book.apply(&add(1, Side::Bid, 100, 5, T0));
    book.apply(&cancel(1, Side::Bid, 100, 5, T0 + 1));
    assert!(book.serialize_flow().len() > empty_len);

    book.apply(&ev(EventKind::Status, Side::None, 0, 0, 0, T0 + 6 * S, 0));
    assert_eq!(book.serialize_flow().len(), empty_len);
}

/// Locked book (bb == ba): wire-legal; structure holds; overlap diagnostic.
#[test]
fn locked_book_passes_invariants_and_roundtrips() {
    use fft_book::Overlap;

    let mut live = book();
    live.apply(&add(1, Side::Bid, 100, 5, T0));
    live.apply(&add(2, Side::Ask, 100, 3, T0 + 1));
    assert_eq!(live.best_bid(), Some(px(100)));
    assert_eq!(live.best_ask(), Some(px(100)));
    assert_eq!(live.book_overlap(), Some(Overlap::Locked(px(100))));
    live.check_invariants();

    let (book, flow, refresh) = sections(&live);
    let restored = Book::restore(&book, &flow, &refresh).unwrap();
    restored.check_invariants();
    assert_eq!(restored.book_overlap(), Some(Overlap::Locked(px(100))));
    assert_eq!(restored.serialize_book(), book);
    assert_eq!(restored.serialize_flow(), flow);
    assert_eq!(restored.serialize_refresh(), refresh);
}

/// Crossed book (bb > ba): wire-legal (intra-event-group / post-close).
#[test]
fn crossed_book_passes_invariants_and_roundtrips() {
    use fft_book::Overlap;

    let mut live = book();
    live.apply(&add(1, Side::Bid, 100, 5, T0));
    live.apply(&add(2, Side::Ask, 99, 3, T0 + 1));
    assert_eq!(
        live.book_overlap(),
        Some(Overlap::Crossed {
            bid: px(100),
            ask: px(99),
        })
    );
    live.check_invariants();

    let (book, flow, refresh) = sections(&live);
    let restored = Book::restore(&book, &flow, &refresh).unwrap();
    restored.check_invariants();
    assert_eq!(
        restored.book_overlap(),
        Some(Overlap::Crossed {
            bid: px(100),
            ask: px(99),
        })
    );
    assert_eq!(restored.serialize_book(), book);
    assert_eq!(restored.serialize_flow(), flow);
    assert_eq!(restored.serialize_refresh(), refresh);
}

#[test]
fn normal_book_has_no_overlap() {
    let mut live = book();
    live.apply(&add(1, Side::Bid, 100, 5, T0));
    live.apply(&add(2, Side::Ask, 101, 3, T0 + 1));
    assert_eq!(live.book_overlap(), None);
    live.check_invariants();
}
