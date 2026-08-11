//! Shared synthetic-event builders for the fft-book test suite.

#![allow(dead_code)]

pub mod queue_oracles;

use fft_book::Book;
use fft_core::{CanonicalEvent, EventKind, OrderId, Price, Seq, Side, Ts};

/// ES tick: 0.25 in 1e-9 units.
pub const TICK: i64 = 250_000_000;
/// One second in ns.
pub const S: u64 = 1_000_000_000;
/// Base event time for fixtures (flow-window bucket 0 means "untouched", so
/// fixtures live well past t=0).
pub const T0: u64 = 100 * S;

pub fn book() -> Book {
    Book::new(Price(TICK))
}

pub fn px(ticks: i64) -> Price {
    Price(ticks * TICK)
}

pub fn ev(
    kind: EventKind,
    side: Side,
    ticks: i64,
    size: u32,
    id: u64,
    ts: u64,
    seq: u32,
) -> CanonicalEvent {
    CanonicalEvent {
        kind,
        side,
        flags: 0,
        size,
        ts: Ts(ts),
        seq: Seq(seq),
        price: px(ticks),
        order_id: OrderId(id),
    }
}

pub fn add(id: u64, side: Side, ticks: i64, size: u32, ts: u64) -> CanonicalEvent {
    ev(EventKind::Add, side, ticks, size, id, ts, 0)
}

pub fn cancel(id: u64, side: Side, ticks: i64, size: u32, ts: u64) -> CanonicalEvent {
    ev(EventKind::Cancel, side, ticks, size, id, ts, 0)
}

pub fn modify(id: u64, side: Side, ticks: i64, size: u32, ts: u64) -> CanonicalEvent {
    ev(EventKind::Modify, side, ticks, size, id, ts, 0)
}

pub fn fill(id: u64, side: Side, ticks: i64, size: u32, ts: u64) -> CanonicalEvent {
    ev(EventKind::Fill, side, ticks, size, id, ts, 0)
}

pub fn trade(aggressor: Side, ticks: i64, size: u32, ts: u64) -> CanonicalEvent {
    ev(EventKind::Trade, aggressor, ticks, size, 0, ts, 0)
}

pub fn clear(ts: u64) -> CanonicalEvent {
    ev(EventKind::Clear, Side::None, 0, 0, 0, ts, 0)
}

pub fn gap(ts: u64, expected: u64, observed: u64) -> CanonicalEvent {
    CanonicalEvent::gap(Ts(ts), expected, observed)
}
