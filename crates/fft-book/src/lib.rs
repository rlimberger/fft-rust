//! Deterministic L3 (market-by-order) book for one instrument, consuming
//! [`fft_core::CanonicalEvent`].
//!
//! Ported from the legacy keeper (verified over 82 M events, zero invariant
//! failures): sliding 512-tick dense window per side over a `BTreeMap` far map,
//! slab-allocated orders on intrusive per-level FIFO lists, exact CME modify
//! semantics (size-down in place keeps queue position; size-up or price change
//! goes to the back of the target level), a 5 s per-level flow window
//! (stacks / pulls / traded-at-touch), and [`Book::check_invariants`].
//!
//! New over legacy:
//! - First-class BOOK/FLOW/REFRESH checkpoint sections (`docs/FFTLOG-V2.md`
//!   §5): sides in price order, orders strictly head-to-tail FIFO, never a
//!   replay of synthetic events.
//! - Exact queue-position queries for any resting order (PRD §4 claim 3).
//! - The native-refresh (iceberg) state machine on the CME same-`order_id`
//!   signature: per-order reload count, cumulative hidden volume, per-price
//!   aggregates, cumulative non-mutating Fill depletion, and gap-aware
//!   *unavailable* classification (PRD §2.4, claim 4).
//! - Source-sequence and gap accounting (unexplained regressions panic).

#![forbid(unsafe_code)]

mod book;
mod flow;
mod level;
mod mutate;
mod query;
mod refresh;
mod serialize;
mod side;

pub use book::Book;
pub use level::LevelView;
pub use serialize::RestoreError;

use fft_core::{Price, Side};

/// fftlog v2 checkpoint section id of the BOOK section (`docs/FFTLOG-V2.md` §5).
pub const BOOK_SECTION_ID: u16 = 1;

/// fftlog v2 checkpoint section id of the FLOW section.
pub const FLOW_SECTION_ID: u16 = 2;

/// fftlog v2 checkpoint section id of the REFRESH section.
pub const REFRESH_SECTION_ID: u16 = 5;

/// Self-identifying BOOK payload layout version.
pub const BOOK_SECTION_VERSION: u16 = 3;

/// Self-identifying FLOW payload layout version.
pub const FLOW_SECTION_VERSION: u16 = 1;

/// Self-identifying REFRESH payload layout version.
pub const REFRESH_SECTION_VERSION: u16 = 2;

/// Native-refresh acceptance window, event-time ns: a fully-filled order id
/// restored to positive displayed size more than this long after depletion is a
/// new order life, never a refresh. CME re-displays an iceberg tranche within
/// the same match event, so 1 ms is orders of magnitude above the real signature
/// while bounding tombstone memory (expired candidates are GC'd).
pub const REFRESH_WINDOW_NS: u64 = 1_000_000;

/// Most recent execution print ([`fft_core::EventKind::Fill`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LastTrade {
    pub price: Price,
    pub size: u32,
    /// Aggressor side; [`Side::None`] when the source did not attribute one.
    pub aggressor: Side,
}

/// Bid/ask price relation at the touch. Diagnostic only — locked and crossed
/// books are wire-legal (intra-event-group publication and post-close windows).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Overlap {
    /// Best bid equals best ask.
    Locked(Price),
    /// Best bid strictly above best ask.
    Crossed { bid: Price, ask: Price },
}

/// Grady "current traded" counters at the touch: sells into the bid (cB) and
/// buys into the ask (cA), each resetting when its trade price changes.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct InsideTraded {
    pub bid_price: Option<Price>,
    pub bid_vol: u64,
    pub ask_price: Option<Price>,
    pub ask_vol: u64,
}

/// Exact queue standing of one resting order (PRD §4 claim 3).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct QueuePosition {
    pub side: Side,
    pub price: Price,
    /// Remaining displayed size of the order itself.
    pub size: u32,
    /// Orders resting ahead of it at its price level.
    pub orders_ahead: u32,
    /// Contracts resting ahead of it at its price level.
    pub contracts_ahead: u64,
    /// 1-based FIFO rank at the level (head of queue = 1); `orders_ahead + 1`.
    pub rank: u32,
}

/// Native-refresh classification of one order (PRD §4 claim 4).
///
/// `Known { native: false }` is only ever returned for an order whose entire
/// observed life is gap-free; across a sequence gap reads are [`Unavailable`],
/// never a false boolean, until the order is re-observed unambiguously (a fresh
/// add of that id, or a complete post-gap depletion→restore cycle).
///
/// [`Unavailable`]: RefreshState::Unavailable
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RefreshState {
    /// The order id is not currently resting in the book.
    NotResting,
    /// A sequence gap broke the observation chain for this order.
    Unavailable,
    Known {
        /// True iff a native refresh (same id restored after full displayed
        /// fill) has been observed for this order.
        native: bool,
        /// Observed reload count (lower bound if the order ever spanned a gap).
        reloads: u32,
        /// Cumulative re-displayed (hidden) volume across observed reloads.
        hidden_volume: u64,
    },
}

/// Session-cumulative native-refresh aggregate at one price on one side.
/// Counts are observed lower bounds (gaps can hide refreshes, never add them).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct PriceRefreshAgg {
    /// Classified native refreshes observed at this price.
    pub refresh_count: u32,
    /// Total re-displayed (hidden) volume revealed by those refreshes.
    pub hidden_volume: u64,
}
