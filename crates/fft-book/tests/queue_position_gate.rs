//! PRD §4 claim 3 acceptance gate: exact queue position.
//!
//! Independence (not "call the same walk twice"):
//! 1. **Shadow** (`common::queue_oracles::Shadow`) — Vec CME model (size-down
//!    in place; size-up / price-change → back; snapshot prefix ahead of live).
//!    Never touches slab links or `queue_position`.
//! 2. **BookFifo** — prefix sums over `serialize_book()` BOOK v3 bytes; never
//!    reuses `query.rs`.
//!
//! Book ≡ both oracles after every scenario step. Synthetic deterministic gate
//! only — not a CME venue-truth claim.

mod common;

use common::queue_oracles::{self, BookFifo, Shadow};
use common::*;
use fft_book::Book;
use fft_core::{CanonicalEvent, DATABENTO_SNAPSHOT_FLAG, OrderId, Side};

fn assert_all_queues(book: &Book, shadow: &Shadow) {
    queue_oracles::assert_all_queues(book, shadow);
}

fn assert_absent(book: &Book, id: u64) {
    queue_oracles::assert_absent(book, id);
}

/// Snapshot-flagged Add (Databento SNAPSHOT bit); seq 0 is unsequenced fixture.
fn snapshot_add(id: u64, side: Side, ticks: i64, size: u32, ts: u64) -> CanonicalEvent {
    let mut e = add(id, side, ticks, size, ts);
    e.flags = DATABENTO_SNAPSHOT_FLAG;
    e
}

fn seed_level(b: &mut Book, sh: &mut Shadow, side: Side, ticks: i64, orders: &[(u64, u32)]) {
    for &(id, sz) in orders {
        b.apply(&add(id, side, ticks, sz, T0 + id));
        sh.add(id, side, ticks, sz);
    }
}

const THREE: &[(u64, u32)] = &[(1, 10), (2, 20), (3, 30)];

// ── Hand-crafted claim-3 scenarios ──────────────────────────────────────────

#[test]
fn contracts_and_orders_ahead_include_one_lots() {
    let mut b = book();
    let mut sh = Shadow::default();
    seed_level(&mut b, &mut sh, Side::Bid, 100, &[(1, 1), (2, 1), (3, 7)]);
    seed_level(&mut b, &mut sh, Side::Ask, 101, &[(4, 1), (5, 1)]);
    assert_all_queues(&b, &sh);
    let q = |id| b.queue_position(OrderId(id)).unwrap();
    assert_eq!(
        (
            q(1).orders_ahead,
            q(1).contracts_ahead,
            q(1).rank,
            q(1).size
        ),
        (0, 0, 1, 1)
    );
    assert_eq!(
        (
            q(2).orders_ahead,
            q(2).contracts_ahead,
            q(2).rank,
            q(2).size
        ),
        (1, 1, 2, 1)
    );
    assert_eq!(
        (
            q(3).orders_ahead,
            q(3).contracts_ahead,
            q(3).rank,
            q(3).size
        ),
        (2, 2, 3, 7)
    );
    assert_eq!(
        (q(5).orders_ahead, q(5).contracts_ahead, q(5).rank),
        (1, 1, 2)
    );
}

#[test]
fn size_down_preserves_fifo_rank() {
    let mut b = book();
    let mut sh = Shadow::default();
    seed_level(&mut b, &mut sh, Side::Bid, 100, THREE);
    b.apply(&modify(2, Side::Bid, 100, 15, T0 + 10));
    sh.modify(2, 100, 15);
    assert_all_queues(&b, &sh);
    let q = b.queue_position(OrderId(2)).unwrap();
    assert_eq!(
        (q.rank, q.orders_ahead, q.contracts_ahead, q.size),
        (2, 1, 10, 15)
    );
    let q3 = b.queue_position(OrderId(3)).unwrap();
    assert_eq!((q3.rank, q3.contracts_ahead), (3, 25));
}

#[test]
fn size_up_goes_to_back() {
    let mut b = book();
    let mut sh = Shadow::default();
    seed_level(&mut b, &mut sh, Side::Bid, 100, THREE);
    b.apply(&modify(1, Side::Bid, 100, 12, T0 + 10));
    sh.modify(1, 100, 12);
    assert_all_queues(&b, &sh);
    assert_eq!(b.queue_position(OrderId(1)).unwrap().rank, 3);
    assert_eq!(b.queue_position(OrderId(2)).unwrap().rank, 1);
    assert_eq!(b.queue_position(OrderId(3)).unwrap().rank, 2);
}

#[test]
fn price_change_goes_to_back_of_target_level() {
    let mut b = book();
    let mut sh = Shadow::default();
    seed_level(&mut b, &mut sh, Side::Bid, 100, THREE);
    b.apply(&add(4, Side::Bid, 99, 5, T0 + 4));
    sh.add(4, Side::Bid, 99, 5);
    b.apply(&modify(1, Side::Bid, 99, 10, T0 + 10));
    sh.modify(1, 99, 10);
    assert_all_queues(&b, &sh);
    let q = b.queue_position(OrderId(1)).unwrap();
    assert_eq!(q.price, px(99));
    assert_eq!((q.rank, q.orders_ahead, q.contracts_ahead), (2, 1, 5));
    assert_eq!(b.queue_position(OrderId(2)).unwrap().rank, 1);
    assert_eq!(b.queue_position(OrderId(4)).unwrap().rank, 1);
}

#[test]
fn price_and_size_change_goes_to_target_tail() {
    // Single Modify with both price and size changed → back of target level.
    let mut b = book();
    let mut sh = Shadow::default();
    seed_level(&mut b, &mut sh, Side::Bid, 100, THREE);
    b.apply(&add(4, Side::Bid, 99, 5, T0 + 4));
    sh.add(4, Side::Bid, 99, 5);
    b.apply(&modify(2, Side::Bid, 99, 25, T0 + 10));
    sh.modify(2, 99, 25);
    assert_all_queues(&b, &sh);
    let q = b.queue_position(OrderId(2)).unwrap();
    assert_eq!(q.price, px(99));
    assert_eq!(
        (q.rank, q.orders_ahead, q.contracts_ahead, q.size),
        (2, 1, 5, 25)
    );
    assert_eq!(b.queue_position(OrderId(4)).unwrap().rank, 1);
    assert_eq!(b.queue_position(OrderId(1)).unwrap().rank, 1);
    assert_eq!(b.queue_position(OrderId(3)).unwrap().rank, 2);
}

#[test]
fn fill_leaves_displayed_queue_unchanged() {
    // Fill is tape/depletion only: displayed size and FIFO ranks must not move.
    let mut b = book();
    let mut sh = Shadow::default();
    seed_level(&mut b, &mut sh, Side::Bid, 100, THREE);
    let before = b.queue_position(OrderId(2)).unwrap();
    b.apply(&fill(2, Side::Bid, 100, 7, T0 + 10));
    sh.fill(2, 7);
    assert_all_queues(&b, &sh);
    let after = b.queue_position(OrderId(2)).unwrap();
    assert_eq!(after, before);
    assert_eq!((after.rank, after.size, after.contracts_ahead), (2, 20, 10));
    assert_eq!(b.queue_position(OrderId(3)).unwrap().contracts_ahead, 30);
}

/// Full Fill depletion + companion Modify: `is_depleted` reinsert at level tail
/// (mutate.rs:157-160). CME native-refresh signature territory for claim 3 ranks.
#[test]
fn fill_full_depletion_then_modify_reinserts_at_back() {
    let mut b = book();
    let mut sh = Shadow::default();
    seed_level(&mut b, &mut sh, Side::Bid, 100, THREE);
    // Displayed size still 20; Fill marks depleted without mutating depth.
    b.apply(&fill(2, Side::Bid, 100, 20, T0 + 10));
    sh.fill(2, 20);
    assert_all_queues(&b, &sh);
    assert_eq!(b.queue_position(OrderId(2)).unwrap().rank, 2);

    // Companion Modify restores displayed size → remove + insert_order at tail.
    b.apply(&modify(2, Side::Bid, 100, 25, T0 + 11));
    sh.modify(2, 100, 25);
    assert_all_queues(&b, &sh);
    let q1 = b.queue_position(OrderId(1)).unwrap();
    let q3 = b.queue_position(OrderId(3)).unwrap();
    let q2 = b.queue_position(OrderId(2)).unwrap();
    assert_eq!(
        (q1.rank, q1.orders_ahead, q1.contracts_ahead, q1.size),
        (1, 0, 0, 10)
    );
    assert_eq!(
        (q3.rank, q3.orders_ahead, q3.contracts_ahead, q3.size),
        (2, 1, 10, 30)
    );
    assert_eq!(
        (q2.rank, q2.orders_ahead, q2.contracts_ahead, q2.size),
        (3, 2, 40, 25)
    );
}

/// Partial Fill never arms `is_depleted`; companion Modify follows ordinary CME
/// priority rules (size-down keeps rank; size-up → back).
#[test]
fn fill_partial_then_modify_follows_ordinary_priority() {
    let mut b = book();
    let mut sh = Shadow::default();
    seed_level(&mut b, &mut sh, Side::Bid, 100, THREE);
    b.apply(&fill(2, Side::Bid, 100, 7, T0 + 10));
    sh.fill(2, 7);
    assert_all_queues(&b, &sh);

    // Size-down while not depleted: in-place, rank preserved.
    b.apply(&modify(2, Side::Bid, 100, 15, T0 + 11));
    sh.modify(2, 100, 15);
    assert_all_queues(&b, &sh);
    assert_eq!(
        (
            b.queue_position(OrderId(2)).unwrap().rank,
            b.queue_position(OrderId(2)).unwrap().size,
            b.queue_position(OrderId(3)).unwrap().contracts_ahead
        ),
        (2, 15, 25)
    );

    // Fresh partial Fill, then size-up → priority loss to back (not the deplete path).
    b.apply(&fill(1, Side::Bid, 100, 3, T0 + 12));
    sh.fill(1, 3);
    b.apply(&modify(1, Side::Bid, 100, 12, T0 + 13));
    sh.modify(1, 100, 12);
    assert_all_queues(&b, &sh);
    assert_eq!(b.queue_position(OrderId(2)).unwrap().rank, 1);
    assert_eq!(b.queue_position(OrderId(3)).unwrap().rank, 2);
    assert_eq!(b.queue_position(OrderId(1)).unwrap().rank, 3);
}

#[test]
fn snapshot_prefix_ranks_ahead_of_live() {
    // Snapshot-flagged Adds sit in a FIFO prefix ahead of live at the level.
    let mut b = book();
    let mut sh = Shadow::default();
    seed_level(&mut b, &mut sh, Side::Bid, 100, &[(1, 10), (2, 20)]);
    b.apply(&snapshot_add(3, Side::Bid, 100, 30, T0 - 2));
    sh.add_snapshot(3, Side::Bid, 100, 30);
    b.apply(&snapshot_add(4, Side::Bid, 100, 40, T0 - 1));
    sh.add_snapshot(4, Side::Bid, 100, 40);
    assert_all_queues(&b, &sh);
    for (id, rank, ahead) in [(3, 1u32, 0u64), (4, 2, 30), (1, 3, 70), (2, 4, 80)] {
        let q = b.queue_position(OrderId(id)).unwrap();
        assert_eq!((q.rank, q.contracts_ahead), (rank, ahead), "id {id}");
    }
}

/// Snapshot-origin size-down (same price): in-place; keeps snapshot-prefix rank
/// (mutate.rs:169-179 does not clear OrderOrigin::Snapshot).
#[test]
fn snapshot_size_down_keeps_prefix_rank() {
    let mut b = book();
    let mut sh = Shadow::default();
    b.apply(&snapshot_add(1, Side::Bid, 100, 30, T0 - 2));
    sh.add_snapshot(1, Side::Bid, 100, 30);
    b.apply(&snapshot_add(2, Side::Bid, 100, 40, T0 - 1));
    sh.add_snapshot(2, Side::Bid, 100, 40);
    b.apply(&add(3, Side::Bid, 100, 10, T0));
    sh.add(3, Side::Bid, 100, 10);
    assert_all_queues(&b, &sh);

    b.apply(&modify(1, Side::Bid, 100, 20, T0 + 10));
    sh.modify(1, 100, 20);
    assert_all_queues(&b, &sh);
    let q1 = b.queue_position(OrderId(1)).unwrap();
    let q2 = b.queue_position(OrderId(2)).unwrap();
    let q3 = b.queue_position(OrderId(3)).unwrap();
    assert_eq!((q1.rank, q1.size, q1.contracts_ahead), (1, 20, 0));
    assert_eq!((q2.rank, q2.contracts_ahead), (2, 20));
    assert_eq!((q3.rank, q3.contracts_ahead), (3, 60));
}

/// Snapshot-origin size-up / price-change: demote to Live at back of target
/// level (mutate.rs:180-192 / 197-212 set OrderOrigin::Live + link_tail).
#[test]
fn snapshot_size_up_and_price_change_demote_to_live_tail() {
    let mut b = book();
    let mut sh = Shadow::default();
    b.apply(&snapshot_add(1, Side::Bid, 100, 30, T0 - 2));
    sh.add_snapshot(1, Side::Bid, 100, 30);
    b.apply(&snapshot_add(2, Side::Bid, 100, 40, T0 - 1));
    sh.add_snapshot(2, Side::Bid, 100, 40);
    b.apply(&add(3, Side::Bid, 100, 10, T0));
    sh.add(3, Side::Bid, 100, 10);
    b.apply(&add(4, Side::Bid, 99, 5, T0 + 1));
    sh.add(4, Side::Bid, 99, 5);
    assert_all_queues(&b, &sh);

    // Size-up on snapshot id 1 → live at back of 100 (behind 2 then 3).
    b.apply(&modify(1, Side::Bid, 100, 35, T0 + 10));
    sh.modify(1, 100, 35);
    assert_all_queues(&b, &sh);
    assert_eq!(b.queue_position(OrderId(2)).unwrap().rank, 1);
    assert_eq!(b.queue_position(OrderId(3)).unwrap().rank, 2);
    assert_eq!(
        (
            b.queue_position(OrderId(1)).unwrap().rank,
            b.queue_position(OrderId(1)).unwrap().size,
            b.queue_position(OrderId(1)).unwrap().contracts_ahead
        ),
        (3, 35, 50)
    );

    // Price-change on remaining snapshot id 2 → live at back of 99 (behind 4).
    b.apply(&modify(2, Side::Bid, 99, 40, T0 + 11));
    sh.modify(2, 99, 40);
    assert_all_queues(&b, &sh);
    let q2 = b.queue_position(OrderId(2)).unwrap();
    assert_eq!(q2.price, px(99));
    assert_eq!((q2.rank, q2.orders_ahead, q2.contracts_ahead), (2, 1, 5));
    assert_eq!(b.queue_position(OrderId(4)).unwrap().rank, 1);
    assert_eq!(b.queue_position(OrderId(3)).unwrap().rank, 1);
    assert_eq!(b.queue_position(OrderId(1)).unwrap().rank, 2);
}

#[test]
fn cancel_full_and_partial_adjust_ahead() {
    let mut b = book();
    let mut sh = Shadow::default();
    seed_level(
        &mut b,
        &mut sh,
        Side::Bid,
        100,
        &[(1, 10), (2, 20), (3, 30), (4, 1)],
    );
    b.apply(&cancel(1, Side::Bid, 100, 4, T0 + 10));
    sh.cancel_qty(1, 4);
    assert_all_queues(&b, &sh);
    assert_eq!(b.queue_position(OrderId(1)).unwrap().size, 6);
    assert_eq!(
        b.queue_position(OrderId(4)).unwrap().contracts_ahead,
        6 + 20 + 30
    );
    b.apply(&cancel(2, Side::Bid, 100, 20, T0 + 11));
    sh.cancel_full(2);
    assert_all_queues(&b, &sh);
    assert_absent(&b, 2);
    assert_eq!(b.queue_position(OrderId(3)).unwrap().rank, 2);
    assert_eq!(b.queue_position(OrderId(3)).unwrap().contracts_ahead, 6);
    assert_eq!(b.queue_position(OrderId(4)).unwrap().rank, 3);
    assert_eq!(b.queue_position(OrderId(4)).unwrap().contracts_ahead, 36);
}

#[test]
fn checkpoint_serialize_restore_preserves_queue_math() {
    let mut live = book();
    let mut sh = Shadow::default();
    seed_level(
        &mut live,
        &mut sh,
        Side::Bid,
        100,
        &[(1, 1), (2, 8), (3, 1)],
    );
    live.apply(&add(4, Side::Bid, 99, 5, T0 + 3));
    sh.add(4, Side::Bid, 99, 5);
    seed_level(&mut live, &mut sh, Side::Ask, 101, &[(5, 3), (6, 1)]);
    live.apply(&modify(2, Side::Bid, 100, 4, T0 + 6));
    sh.modify(2, 100, 4);
    live.apply(&modify(1, Side::Bid, 100, 2, T0 + 7));
    sh.modify(1, 100, 2);
    live.apply(&cancel(3, Side::Bid, 100, 1, T0 + 8));
    sh.cancel_full(3);
    assert_all_queues(&live, &sh);
    let (bb, ff, rr) = (
        live.serialize_book(),
        live.serialize_flow(),
        live.serialize_refresh(),
    );
    let pre = BookFifo::parse(&bb);
    for id in sh.live_ids() {
        assert_eq!(
            live.queue_position(OrderId(id)).unwrap(),
            pre.queue(id).unwrap()
        );
    }
    let restored = Book::restore(&bb, &ff, &rr).unwrap();
    restored.check_invariants();
    assert_all_queues(&restored, &sh);
    assert_eq!(restored.serialize_book(), bb);
}

// ── Deterministic randomized sequences ──────────────────────────────────────

#[derive(Clone, Copy)]
enum Op {
    Add {
        bid: bool,
        off: i64,
        size: u32,
    },
    Cancel {
        sel: usize,
    },
    PartialCancel {
        sel: usize,
        qty: u32,
    },
    SizeDown {
        sel: usize,
        new_size: u32,
    },
    SizeUp {
        sel: usize,
        new_size: u32,
    },
    PriceMove {
        sel: usize,
        off: i64,
    },
    /// Single Modify changing both price and size (always priority loss).
    PriceSizeMove {
        sel: usize,
        off: i64,
        new_size: u32,
    },
}

const CENTER: i64 = 500;
const BAND: i64 = 20;

fn side_ticks(bid: bool, off: i64) -> i64 {
    if bid {
        CENTER - 1 - off
    } else {
        CENTER + 1 + off
    }
}

fn apply_op(b: &mut Book, sh: &mut Shadow, next_id: &mut u64, ts: u64, op: Op) {
    match op {
        Op::Add { bid, off, size } => {
            let side = if bid { Side::Bid } else { Side::Ask };
            let t = side_ticks(bid, off);
            let id = *next_id;
            *next_id += 1;
            b.apply(&add(id, side, t, size, ts));
            sh.add(id, side, t, size);
        }
        Op::Cancel { sel } => {
            if sh.orders.is_empty() {
                return;
            }
            let o = sh.orders[sel % sh.orders.len()];
            b.apply(&cancel(o.id, o.side, o.ticks, o.size, ts));
            sh.cancel_full(o.id);
        }
        Op::PartialCancel { sel, qty } => {
            if sh.orders.is_empty() {
                return;
            }
            let o = sh.orders[sel % sh.orders.len()];
            if o.size == 1 {
                return;
            }
            let q = 1 + qty % (o.size - 1);
            b.apply(&cancel(o.id, o.side, o.ticks, q, ts));
            sh.cancel_qty(o.id, q);
        }
        Op::SizeDown { sel, new_size } => {
            if sh.orders.is_empty() {
                return;
            }
            let o = sh.orders[sel % sh.orders.len()];
            let ns = 1 + new_size % o.size;
            b.apply(&modify(o.id, o.side, o.ticks, ns, ts));
            sh.modify(o.id, o.ticks, ns);
        }
        Op::SizeUp { sel, new_size } => {
            if sh.orders.is_empty() {
                return;
            }
            let o = sh.orders[sel % sh.orders.len()];
            let ns = o.size + 1 + (new_size % 50);
            b.apply(&modify(o.id, o.side, o.ticks, ns, ts));
            sh.modify(o.id, o.ticks, ns);
        }
        Op::PriceMove { sel, off } => {
            if sh.orders.is_empty() {
                return;
            }
            let o = sh.orders[sel % sh.orders.len()];
            let t = side_ticks(o.side == Side::Bid, off);
            b.apply(&modify(o.id, o.side, t, o.size, ts));
            sh.modify(o.id, t, o.size);
        }
        Op::PriceSizeMove { sel, off, new_size } => {
            if sh.orders.is_empty() {
                return;
            }
            let o = sh.orders[sel % sh.orders.len()];
            let t = side_ticks(o.side == Side::Bid, off);
            let ns = 1 + new_size % 50;
            b.apply(&modify(o.id, o.side, t, ns, ts));
            sh.modify(o.id, t, ns);
        }
    }
}

/// Fixed-seed xorshift: book ≡ shadow ≡ BOOK-bytes after **every** op.
/// Mid-stream checkpoint restore must not diverge.
/// Fill→Modify reinsert / partial-fill companions are hand-tested above
/// (`fill_full_deplete_then_modify_reinserts_at_back`,
/// `partial_fill_then_size_down_keeps_rank`,
/// `partial_fill_then_size_up_goes_to_back`); refresh classification is claim 4.
#[test]
fn randomized_deterministic_sequences_triple_agree() {
    let mut state = 0xC0FF_EE42_D00D_F00Du64;
    let mut rng = move || {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        state
    };

    for run in 0..8u32 {
        let mut b = book();
        let mut sh = Shadow::default();
        let mut next_id = 1u64;
        let mut ts = T0;
        let steps = 200 + (run as usize) * 50;
        for i in 0..steps {
            ts += 1_000_000 + (rng() % 500_000);
            let sel = rng() as usize;
            let off = (rng() % BAND as u64) as i64;
            let bid = rng().is_multiple_of(2);
            let size = if rng() % 5 == 0 {
                1
            } else {
                1 + (rng() % 40) as u32
            };
            let op = match rng() % 16 {
                0..=5 => Op::Add { bid, off, size },
                6 | 7 => Op::Cancel { sel },
                8 => Op::PartialCancel {
                    sel,
                    qty: (rng() % 20) as u32,
                },
                9 | 10 => Op::SizeDown {
                    sel,
                    new_size: (rng() % 40) as u32,
                },
                11 | 12 => Op::SizeUp {
                    sel,
                    new_size: (rng() % 40) as u32,
                },
                13 | 14 => Op::PriceMove { sel, off },
                _ => Op::PriceSizeMove {
                    sel,
                    off,
                    new_size: (rng() % 40) as u32,
                },
            };
            apply_op(&mut b, &mut sh, &mut next_id, ts, op);
            assert_all_queues(&b, &sh);

            if i == steps / 2 && !sh.orders.is_empty() {
                let (bb, ff, rr) = (
                    b.serialize_book(),
                    b.serialize_flow(),
                    b.serialize_refresh(),
                );
                let restored = Book::restore(&bb, &ff, &rr).unwrap();
                assert_all_queues(&restored, &sh);
                b = restored;
            }
        }
        assert_all_queues(&b, &sh);
        assert!(
            b.live_order_count() > 0 || steps < 50,
            "run {run}: empty book after {steps} steps is unexpected for this seed"
        );
    }
}

/// Dense same-price queue: pure contracts/orders-ahead arithmetic under churn.
#[test]
fn single_level_churn_contracts_ahead_exact() {
    let mut b = book();
    let mut sh = Shadow::default();
    let mut ts = T0;
    for (id, size) in (1u64..).zip([1u32, 1, 5, 1, 10, 1, 2, 1, 8, 1]) {
        ts += 1;
        b.apply(&add(id, Side::Ask, 200, size, ts));
        sh.add(id, Side::Ask, 200, size);
    }
    assert_all_queues(&b, &sh);

    let expected: &[(u64, u32, u64)] = &[
        (1, 0, 0),
        (2, 1, 1),
        (3, 2, 2),
        (4, 3, 7),
        (5, 4, 8),
        (6, 5, 18),
        (7, 6, 19),
        (8, 7, 21),
        (9, 8, 22),
        (10, 9, 30),
    ];
    for &(id, oa, ca) in expected {
        let q = b.queue_position(OrderId(id)).unwrap();
        assert_eq!((q.orders_ahead, q.contracts_ahead), (oa, ca), "id {id}");
    }

    ts += 1;
    b.apply(&modify(5, Side::Ask, 200, 3, ts));
    sh.modify(5, 200, 3);
    ts += 1;
    b.apply(&cancel(2, Side::Ask, 200, 1, ts));
    sh.cancel_full(2);
    ts += 1;
    b.apply(&modify(1, Side::Ask, 200, 4, ts));
    sh.modify(1, 200, 4);
    assert_all_queues(&b, &sh);

    let tail = sh.live_ids().last().copied().unwrap();
    assert_eq!(tail, 1, "size-up head must be last in FIFO");
    let q1 = b.queue_position(OrderId(1)).unwrap();
    assert_eq!(q1.rank, sh.orders.len() as u32);
    assert_eq!(q1.size, 4);
}
