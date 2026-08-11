//! Differential test: the slab/intrusive-list book against a naive Vec-based
//! shadow model over randomized event streams. After every event the book's
//! invariants must hold and sizes, FIFO ranks, and contracts-ahead must agree
//! exactly. Bids and asks draw from disjoint price bands so random streams
//! never lock or cross (overlap is wire-legal but excluded here so FIFO ranks
//! stay unambiguous across disjoint bands).

mod common;

use common::*;
use fft_book::Book;
use fft_core::{OrderId, Side};
use proptest::prelude::*;

const CENTER: i64 = 1000;
const BAND: i64 = 40;

#[derive(Debug, Clone, Copy)]
enum Op {
    Add { bid: bool, off: i64, size: u32 },
    Cancel { sel: usize },
    ModifySize { sel: usize, delta: i32 },
    ModifyPrice { sel: usize, off: i64 },
    Fill { sel: usize, qty: u32 },
    Trade { bid: bool, off: i64, size: u32 },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct SOrder {
    id: u64,
    side: Side,
    ticks: i64,
    size: u32,
}

/// Naive reference: one Vec in priority order (per side+price, earlier index =
/// better queue position). CME semantics spelled out with brute force.
#[derive(Default)]
struct Shadow(Vec<SOrder>);

impl Shadow {
    fn pos(&self, id: u64) -> usize {
        self.0
            .iter()
            .position(|o| o.id == id)
            .expect("shadow order")
    }

    fn add(&mut self, id: u64, side: Side, ticks: i64, size: u32) {
        self.0.push(SOrder {
            id,
            side,
            ticks,
            size,
        });
    }

    fn cancel(&mut self, id: u64) {
        let i = self.pos(id);
        self.0.remove(i);
    }

    fn modify(&mut self, id: u64, ticks: i64, size: u32) {
        let i = self.pos(id);
        let o = self.0[i];
        if size == 0 {
            self.0.remove(i);
        } else if ticks == o.ticks && size <= o.size {
            self.0[i].size = size;
        } else {
            self.0.remove(i);
            self.0.push(SOrder {
                id,
                side: o.side,
                ticks,
                size,
            });
        }
    }

    fn ahead(&self, id: u64) -> (u32, u64) {
        let i = self.pos(id);
        let o = self.0[i];
        let mut n = 0u32;
        let mut c = 0u64;
        for e in &self.0[..i] {
            if e.side == o.side && e.ticks == o.ticks {
                n += 1;
                c += u64::from(e.size);
            }
        }
        (n, c)
    }

    /// Levels of one side, best-first: (ticks, total, count).
    fn levels(&self, side: Side) -> Vec<(i64, u64, u32)> {
        let mut out: Vec<(i64, u64, u32)> = Vec::new();
        for o in self.0.iter().filter(|o| o.side == side) {
            match out.iter_mut().find(|(t, _, _)| *t == o.ticks) {
                Some((_, total, count)) => {
                    *total += u64::from(o.size);
                    *count += 1;
                }
                None => out.push((o.ticks, u64::from(o.size), 1)),
            }
        }
        if side == Side::Bid {
            out.sort_by_key(|(t, _, _)| std::cmp::Reverse(*t));
        } else {
            out.sort_by_key(|(t, _, _)| *t);
        }
        out
    }

    fn best(&self, side: Side) -> Option<i64> {
        self.levels(side).first().map(|(t, _, _)| *t)
    }
}

fn compare(b: &Book, sh: &Shadow) {
    b.check_invariants();
    assert_eq!(b.live_order_count(), sh.0.len());
    for o in &sh.0 {
        let q = b
            .queue_position(OrderId(o.id))
            .expect("book missing shadow order");
        let (n, c) = sh.ahead(o.id);
        assert_eq!(q.side, o.side, "side of order {}", o.id);
        assert_eq!(q.price, px(o.ticks), "price of order {}", o.id);
        assert_eq!(q.size, o.size, "size of order {}", o.id);
        assert_eq!(q.orders_ahead, n, "orders ahead of {}", o.id);
        assert_eq!(q.contracts_ahead, c, "contracts ahead of {}", o.id);
        assert_eq!(q.rank, n + 1, "rank of {}", o.id);
    }
    for side in [Side::Bid, Side::Ask] {
        let mut got: Vec<(i64, u64, u32)> = Vec::new();
        b.for_each_level(side, |p, v| {
            got.push((p.0 / TICK, v.total_size, v.order_count));
        });
        assert_eq!(got, sh.levels(side), "{side:?} levels");
        // Per-level FIFO traversal must match the shadow order exactly.
        for &(t, _, _) in &got {
            let mut ids: Vec<u64> = Vec::new();
            b.for_each_order_at(side, px(t), |id, _| ids.push(id.0));
            let want: Vec<u64> =
                sh.0.iter()
                    .filter(|o| o.side == side && o.ticks == t)
                    .map(|o| o.id)
                    .collect();
            assert_eq!(ids, want, "FIFO at {side:?} {t}");
        }
    }
    assert_eq!(b.best_bid(), sh.best(Side::Bid).map(px));
    assert_eq!(b.best_ask(), sh.best(Side::Ask).map(px));
}

fn side_ticks(bid: bool, off: i64) -> i64 {
    if bid {
        CENTER - 1 - off
    } else {
        CENTER + 1 + off
    }
}

/// Apply one op to book and shadow; `sel` indexes the shadow's live orders.
fn step(b: &mut Book, sh: &mut Shadow, next_id: &mut u64, ts: u64, op: Op) {
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
            if sh.0.is_empty() {
                return;
            }
            let o = sh.0[sel % sh.0.len()];
            b.apply(&cancel(o.id, o.side, o.ticks, o.size, ts));
            sh.cancel(o.id);
        }
        Op::ModifySize { sel, delta } => {
            if sh.0.is_empty() {
                return;
            }
            let o = sh.0[sel % sh.0.len()];
            let new_size = o.size.saturating_add_signed(delta);
            b.apply(&modify(o.id, o.side, o.ticks, new_size, ts));
            sh.modify(o.id, o.ticks, new_size);
        }
        Op::ModifyPrice { sel, off } => {
            if sh.0.is_empty() {
                return;
            }
            let o = sh.0[sel % sh.0.len()];
            let t = side_ticks(o.side == Side::Bid, off);
            b.apply(&modify(o.id, o.side, t, o.size, ts));
            sh.modify(o.id, t, o.size);
        }
        Op::Fill { sel, qty } => {
            if sh.0.is_empty() {
                return;
            }
            let o = sh.0[sel % sh.0.len()];
            if o.size == 1 {
                return;
            }
            let q = 1 + qty % (o.size - 1);
            b.apply(&fill(o.id, o.side, o.ticks, q, ts));
            compare(b, sh);
            // The companion Modify is book truth and starts a fresh Fill cycle.
            b.apply(&modify(o.id, o.side, o.ticks, o.size, ts));
        }
        Op::Trade { bid, off, size } => {
            let side = if bid { Side::Bid } else { Side::Ask };
            b.apply(&trade(side, side_ticks(bid, off), size, ts));
            // Tape state only; the shadow tracks resting orders.
        }
    }
}

fn run(ops: &[Op]) {
    let mut b = book();
    let mut sh = Shadow::default();
    let mut next_id = 1u64;
    let mut ts = T0;
    for &op in ops {
        ts += 1_000_000;
        step(&mut b, &mut sh, &mut next_id, ts, op);
        compare(&b, &sh);
    }
}

fn op_strategy() -> impl Strategy<Value = Op> {
    prop_oneof![
        4 => (any::<bool>(), 0..BAND, 1..100u32)
            .prop_map(|(bid, off, size)| Op::Add { bid, off, size }),
        2 => (0..usize::MAX).prop_map(|sel| Op::Cancel { sel }),
        2 => ((0..usize::MAX), -60..60i32)
            .prop_map(|(sel, delta)| Op::ModifySize { sel, delta }),
        1 => ((0..usize::MAX), 0..BAND)
            .prop_map(|(sel, off)| Op::ModifyPrice { sel, off }),
        2 => ((0..usize::MAX), 0..200u32).prop_map(|(sel, qty)| Op::Fill { sel, qty }),
        1 => (any::<bool>(), 0..BAND, 1..50u32)
            .prop_map(|(bid, off, size)| Op::Trade { bid, off, size }),
    ]
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(64))]

    #[test]
    fn differential_random_streams(ops in prop::collection::vec(op_strategy(), 1..120)) {
        run(&ops);
    }
}

/// Deterministic long run (xorshift) at a scale proptest cannot afford:
/// invariants after every event, full differential compare every 100.
#[test]
fn differential_long_run() {
    let mut state = 0x9E37_79B9_7F4A_7C15u64;
    let mut rng = move || {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        state
    };
    let mut b = book();
    let mut sh = Shadow::default();
    let mut next_id = 1u64;
    let mut ts = T0;
    for i in 0..20_000u32 {
        ts += (rng() % 2_000_000) + 1;
        let sel = rng() as usize;
        let off = (rng() % BAND as u64) as i64;
        let bid = rng().is_multiple_of(2);
        let op = match rng() % 12 {
            0..=3 => Op::Add {
                bid,
                off,
                size: 1 + (rng() % 99) as u32,
            },
            4 | 5 => Op::Cancel { sel },
            6 | 7 => Op::ModifySize {
                sel,
                delta: (rng() % 120) as i32 - 60,
            },
            8 => Op::ModifyPrice { sel, off },
            9 | 10 => Op::Fill {
                sel,
                qty: (rng() % 200) as u32,
            },
            _ => Op::Trade {
                bid,
                off,
                size: 1 + (rng() % 49) as u32,
            },
        };
        step(&mut b, &mut sh, &mut next_id, ts, op);
        b.check_invariants();
        if i % 100 == 0 {
            compare(&b, &sh);
        }
    }
    compare(&b, &sh);
    assert!(
        b.live_order_count() > 0,
        "long run should keep a populated book"
    );
}
