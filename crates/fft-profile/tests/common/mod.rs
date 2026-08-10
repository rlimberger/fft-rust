//! Shared fixtures: ES tick, the Wed 2026-07-29 fixture-week session, and
//! canonical-event constructors for scripted tapes.

#![allow(dead_code)]

use fft_core::{CanonicalEvent, EventKind, OrderId, Price, Seq, Side, Ts};

/// ES tick: 0.25 in 1e-9 price units.
pub const TICK: i64 = 250_000_000;

/// Wed 2026-07-29 as CT days since Unix epoch.
/// Hand-computed: 2024-01-01 is day 19,723 (54×365 + 13 leap days);
/// 2026-01-01 adds 366+365 → 20,454; Jul 29 is day-of-year 210 → 20,663.
pub const TRADE_DATE: u32 = 20_663;

const DAY_S: u64 = 86_400;

/// Globex open: Tue 2026-07-28 17:00 CDT = 22:00 UTC (CDT is UTC−5).
pub const SESSION_OPEN_NS: u64 = (20_662 * DAY_S + 22 * 3_600) * 1_000_000_000;
/// RTH open: Wed 2026-07-29 08:30 CDT = 13:30 UTC.
pub const RTH_OPEN_NS: u64 = (20_663 * DAY_S + 13 * 3_600 + 1_800) * 1_000_000_000;
/// Session end: Wed 2026-07-29 16:00 CDT = 21:00 UTC.
pub const SESSION_END_NS: u64 = (20_663 * DAY_S + 21 * 3_600) * 1_000_000_000;

pub fn price(ticks: i64) -> Price {
    Price(ticks * TICK)
}

/// Trade event; `aggressor` follows the Databento MBO convention
/// (`Side::Bid` = buy aggressor).
pub fn trade(ticks: i64, size: u32, aggressor: Side, ts: u64) -> CanonicalEvent {
    CanonicalEvent {
        kind: EventKind::Trade,
        side: aggressor,
        flags: 0,
        size,
        ts: Ts(ts),
        seq: Seq(0),
        price: price(ticks),
        order_id: OrderId(0),
    }
}

pub fn gap(ts: u64) -> CanonicalEvent {
    CanonicalEvent::gap(Ts(ts), 100, 107)
}
