//! Orders and price levels (intrusive FIFO lists over the order slab).

use crate::flow::Flow;
use fft_core::Side;

/// Null slot sentinel for the intrusive lists.
pub(crate) const NIL: u32 = u32::MAX;

/// One resting order. `prev`/`next` are slab slots forming the level FIFO.
#[derive(Debug, Clone)]
pub(crate) struct Order {
    pub id: u64,
    /// Price in ticks.
    pub price: i64,
    pub side: Side,
    pub size: u32,
    /// Event time of last priority assignment (add or priority-losing modify).
    pub ts: u64,
    /// Gap epoch at (re)placement; behind the current epoch ⇒ refresh reads
    /// are Unavailable for this order.
    pub epoch: u32,
    pub prev: u32,
    pub next: u32,
}

/// One price level: FIFO head/tail, aggregates, and its 5 s flow window.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Level {
    pub head: u32,
    pub tail: u32,
    pub total_size: u64,
    pub order_count: u32,
    pub flow: Flow,
}

impl Default for Level {
    fn default() -> Self {
        Self {
            head: NIL,
            tail: NIL,
            total_size: 0,
            order_count: 0,
            flow: Flow::default(),
        }
    }
}

impl Level {
    /// Empty and flow-stale: eligible for GC and excluded from serialization.
    pub fn dead(&self, now: u64) -> bool {
        self.order_count == 0 && self.flow.stale(now)
    }
}

/// Aggregated view of one price level plus its 5 s flow window
/// (stacks = `added_5s`, pulls = `cancelled_5s`, traded-at-touch = `traded_5s`).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct LevelView {
    pub total_size: u64,
    pub order_count: u32,
    pub added_5s: u32,
    pub cancelled_5s: u32,
    pub traded_5s: u32,
}

pub(crate) fn view_of(level: &Level, now: u64) -> LevelView {
    let (added_5s, cancelled_5s, traded_5s) = level.flow.window(now);
    LevelView {
        total_size: level.total_size,
        order_count: level.order_count,
        added_5s,
        cancelled_5s,
        traded_5s,
    }
}
