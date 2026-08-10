//! Shared primitive market types — the in-memory form of the frozen wire schema
//! (`docs/FFTLOG-V2.md` §4). `fft-core` has zero dependencies; every crate builds on it.
//! Field widths and unit conventions are wire-frozen: changing anything here requires a
//! spec edit in the same commit.

#![forbid(unsafe_code)]

/// Timestamp: nanoseconds since Unix epoch, UTC (Databento `ts_event` basis).
/// Trade-date semantics live in `America/Chicago`; this type is always UTC.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug, Default, Hash)]
pub struct Ts(pub u64);

/// Price in 1e-9 fixed-point units (Databento native scale). ES 5000.25 = 5_000_250_000_000.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug, Default, Hash)]
pub struct Price(pub i64);

/// Source sequence number (Databento MBO channel sequence).
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug, Default, Hash)]
pub struct Seq(pub u32);

/// CME order id — the native-refresh (iceberg) key.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug, Default, Hash)]
pub struct OrderId(pub u64);

/// Book side. Wire values are frozen.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Hash)]
#[repr(u8)]
pub enum Side {
    None = 0,
    Bid = 1,
    Ask = 2,
}

impl Side {
    pub fn from_u8(v: u8) -> Option<Side> {
        match v {
            0 => Some(Side::None),
            1 => Some(Side::Bid),
            2 => Some(Side::Ask),
            _ => None,
        }
    }
}

/// Canonical event kind. Wire values are frozen (`docs/FFTLOG-V2.md` §4).
/// `TsReset` (wire value 9) is a framing artifact owned by `fft-log`; it never
/// appears in an in-memory `CanonicalEvent`, which carries absolute timestamps.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Hash)]
#[repr(u8)]
pub enum EventKind {
    Add = 1,
    Cancel = 2,
    Modify = 3,
    Trade = 4,
    Fill = 5,
    Clear = 6,
    Status = 7,
    Gap = 8,
}

impl EventKind {
    pub fn from_u8(v: u8) -> Option<EventKind> {
        match v {
            1 => Some(EventKind::Add),
            2 => Some(EventKind::Cancel),
            3 => Some(EventKind::Modify),
            4 => Some(EventKind::Trade),
            5 => Some(EventKind::Fill),
            6 => Some(EventKind::Clear),
            7 => Some(EventKind::Status),
            8 => Some(EventKind::Gap),
            _ => None,
        }
    }
}

/// Databento `flags::SNAPSHOT` in pinned dbn 0.65.0.
pub const DATABENTO_SNAPSHOT_FLAG: u16 = 1 << 5;

/// One canonical market event: the in-memory form of the frozen 32-byte wire record,
/// with the frame-relative `ts_delta` already resolved to an absolute [`Ts`].
///
/// Field reuse (wire-frozen):
/// - `Gap`: `price` carries the expected sequence, `order_id` the observed one —
///   use [`CanonicalEvent::gap`] / [`CanonicalEvent::gap_seqs`], never the raw fields.
/// - `Status`: `size` carries the `status`-schema code.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct CanonicalEvent {
    pub kind: EventKind,
    pub side: Side,
    /// Source flags (e.g. end-of-event-group marker), passed through from DBN.
    pub flags: u16,
    pub size: u32,
    pub ts: Ts,
    pub seq: Seq,
    pub price: Price,
    pub order_id: OrderId,
}

impl CanonicalEvent {
    /// True for records replayed out of a Databento snapshot block. Snapshot records
    /// carry the **original order-entry** ts and seq (`docs/FFTLOG-V2.md` §4), which are
    /// neither channel-sequenced nor monotonic, so they are exempt from every form of
    /// channel-seq accounting: watermarks, cursors, and gap detection ignore them.
    pub fn is_snapshot(&self) -> bool {
        self.flags & DATABENTO_SNAPSHOT_FLAG != 0
    }

    /// Construct a gap record: the feed skipped from `expected` to `observed`.
    /// Downstream state machines must transition to their gap states (classification
    /// across a gap reads *unavailable*, never false — PRD §4.4).
    pub fn gap(ts: Ts, expected: u64, observed: u64) -> CanonicalEvent {
        CanonicalEvent {
            kind: EventKind::Gap,
            side: Side::None,
            flags: 0,
            size: 0,
            ts,
            seq: Seq(0),
            price: Price(i64::try_from(expected).expect("gap expected_seq exceeds i64")),
            order_id: OrderId(observed),
        }
    }

    /// `(expected, observed)` source sequences of a gap record.
    /// Panics on any other kind — reading gap fields off a non-gap event is a bug.
    pub fn gap_seqs(&self) -> (u64, u64) {
        assert_eq!(self.kind, EventKind::Gap, "gap_seqs() on {:?}", self.kind);
        (
            u64::try_from(self.price.0).expect("gap expected_seq negative"),
            self.order_id.0,
        )
    }
}

/// Instrument metadata carried in the fftlog header (`docs/FFTLOG-V2.md` §2).
///
/// `min_price_increment` and `unit_of_measure_qty` come from the Databento
/// `definition` schema — **not** `contract_multiplier` or `min_price_increment_amount`
/// (both documented look-alike traps).
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct InstrumentMeta {
    /// Raw contract symbol (e.g. `ESU6`) — raw front months, never spliced.
    pub symbol: String,
    pub instrument_id: u32,
    /// Dataset identifier (e.g. `GLBX.MDP3`).
    pub dataset: String,
    /// Tick size in 1e-9 price units (ES: 0.25 → 250_000_000).
    pub min_price_increment: Price,
    /// Contract unit quantity in 1e-9 units (ES: 50 → 50_000_000_000).
    pub unit_of_measure_qty: i64,
    /// Databento display factor (power-of-ten scaling for display, passed through).
    pub display_factor: i64,
    /// CME trade date: days since Unix epoch of the **America/Chicago** trade date.
    pub trade_date: u32,
    /// Globex session open (17:00 CT of the prior calendar day), UTC ns.
    pub session_open: Ts,
}

impl InstrumentMeta {
    /// Currency value of one tick, in 1e-9 currency units:
    /// `min_price_increment × unit_of_measure_qty` (ES: 0.25 × 50 = $12.50 → 12_500_000_000).
    /// i128 math — the raw product overflows i64.
    pub fn tick_value(&self) -> i64 {
        let product = i128::from(self.min_price_increment.0) * i128::from(self.unit_of_measure_qty);
        i64::try_from(product / 1_000_000_000).expect("tick value exceeds i64")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wire_values_round_trip() {
        for k in [1u8, 2, 3, 4, 5, 6, 7, 8] {
            assert_eq!(EventKind::from_u8(k).unwrap() as u8, k);
        }
        assert_eq!(EventKind::from_u8(0), None);
        assert_eq!(EventKind::from_u8(9), None); // TsReset is fft-log internal
        for s in [0u8, 1, 2] {
            assert_eq!(Side::from_u8(s).unwrap() as u8, s);
        }
        assert_eq!(Side::from_u8(3), None);
    }

    #[test]
    fn gap_seqs_round_trip() {
        let e = CanonicalEvent::gap(Ts(1), 100, 107);
        assert_eq!(e.gap_seqs(), (100, 107));
    }

    #[test]
    #[should_panic(expected = "gap_seqs() on Add")]
    fn gap_seqs_on_non_gap_panics() {
        let e = CanonicalEvent {
            kind: EventKind::Add,
            side: Side::Bid,
            flags: 0,
            size: 1,
            ts: Ts(0),
            seq: Seq(0),
            price: Price(0),
            order_id: OrderId(1),
        };
        e.gap_seqs();
    }

    #[test]
    fn es_tick_value() {
        let meta = InstrumentMeta {
            symbol: "ESU6".into(),
            instrument_id: 42,
            dataset: "GLBX.MDP3".into(),
            min_price_increment: Price(250_000_000), // 0.25
            unit_of_measure_qty: 50_000_000_000,     // 50
            display_factor: 1,
            trade_date: 20_662,
            session_open: Ts(0),
        };
        assert_eq!(meta.tick_value(), 12_500_000_000); // $12.50
    }
}
