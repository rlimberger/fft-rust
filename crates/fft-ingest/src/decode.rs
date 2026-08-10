//! DBN MBO → `fft_core::CanonicalEvent` (`docs/FFTLOG-V2.md` §4). Every mapping fact is
//! verified against the pinned `dbn-0.65.0` source, cited per item below. Records this
//! module cannot map are loud errors carrying the raw record — never skipped.
//!
//! Mapping (citations are paths inside the `dbn-0.65.0` crate source):
//!
//! | DBN | Canonical | Source |
//! |---|---|---|
//! | action `'A'` Add | `EventKind::Add` | `src/enums.rs:331-333` |
//! | action `'C'` Cancel (full or partial) | `EventKind::Cancel` | `src/enums.rs:328-330` |
//! | action `'M'` Modify (price and/or size) | `EventKind::Modify` | `src/enums.rs:319-321` |
//! | action `'T'` Trade (aggressor; book unaffected) | `EventKind::Trade` | `src/enums.rs:322-324` |
//! | action `'F'` Fill (resting order) | `EventKind::Fill` | `src/enums.rs:325-327` |
//! | action `'R'` cleaR book | `EventKind::Clear` | `src/enums.rs:334-336` |
//! | action `'N'` None ("no effect on the book") | **loud error** — no canonical carrier | `src/enums.rs:337-340` |
//! | side `'B'`/`'A'`/`'N'` | `Side::Bid`/`Ask`/`None` | `src/enums.rs:273-284` |
//! | `price: i64`, 1e-9 fixed point | `Price` verbatim (same scale) | `src/record.rs:82-87`, `FIXED_PRICE_SCALE` `src/lib.rs:150` |
//! | `size: u32` | `size` verbatim | `src/record.rs:88-90` |
//! | `order_id: u64` (venue-assigned) | `OrderId` verbatim | `src/record.rs:80-81` |
//! | `sequence: u32` (venue-assigned) | `Seq` verbatim | `src/record.rs:119-120` |
//! | `hd.ts_event: u64` UTC ns | `Ts` verbatim (canonical basis) | `src/record.rs:59-64` |
//! | `flags: FlagSet` (`u8` bit field) | `flags` zero-extended to `u16` | `src/flags.rs:9-23`, `src/record.rs:91-93` |
//!
//! `EventKind::Status` (wire 7) carries the DBN `status`-schema code; batch job
//! GLBX-20260803-4WJS899FNL is `mbo`-only, so this decoder never emits it.
//!
//! `UNDEF_PRICE = i64::MAX` and `UNDEF_ORDER_SIZE = u32::MAX` (`src/lib.rs:152,154`) are
//! legal only on `'R'` clear records, where the book fields are meaningless; they are
//! normalized to zero there and are loud errors anywhere else.

use std::fmt;
use std::fs::File;
use std::io::{self, BufReader};
use std::path::Path;

use dbn::decode::dbn::Decoder as DbnDecoder;
use dbn::decode::{DbnMetadata, DecodeRecordRef};
use dbn::{MboMsg, Metadata, UNDEF_ORDER_SIZE, UNDEF_PRICE};
use fft_core::{CanonicalEvent, EventKind, OrderId, Price, Seq, Side, Ts};

/// Ingest failure. Every variant carries enough raw content to reproduce the record.
#[derive(Debug)]
pub enum IngestError {
    Dbn(dbn::Error),
    /// DBN action with no canonical carrier (`'N'` None, or an unknown byte).
    UnmappableAction {
        action: u8,
        record: String,
    },
    /// DBN side byte outside `'B'`/`'A'`/`'N'`.
    UnmappableSide {
        side: u8,
        record: String,
    },
    /// A record in the stream that is not an `MboMsg`.
    UnexpectedRecordType {
        record: String,
    },
    /// `UNDEF_PRICE`/`UNDEF_ORDER_SIZE` on a record kind where the field is meaningful.
    UndefinedField {
        field: &'static str,
        record: String,
    },
    /// Instrument tick metadata is not on disk — see [`instrument_meta`].
    MissingDefinition {
        dataset: String,
        schema: String,
    },
    /// fftlog v2 write/read failure.
    Log(fft_log::LogError),
    /// CLI / write-config misuse (missing required flags, bad values).
    Cli(String),
    /// Instrument id has no symbology mapping and `--symbol` was not supplied.
    UnknownInstrument {
        instrument_id: u32,
        hint: String,
    },
    /// Filter matched zero events — refuse to write an empty log.
    NoEventsWritten {
        instrument_id: u32,
        trade_date: String,
    },
}

impl fmt::Display for IngestError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            IngestError::Dbn(err) => write!(f, "DBN decode error: {err}"),
            IngestError::UnmappableAction { action, record } => write!(
                f,
                "unmappable DBN action {:?} (0x{action:02x}) — no canonical event kind; \
                 raw record: {record}",
                char::from(*action),
            ),
            IngestError::UnmappableSide { side, record } => write!(
                f,
                "unmappable DBN side {:?} (0x{side:02x}); raw record: {record}",
                char::from(*side),
            ),
            IngestError::UnexpectedRecordType { record } => write!(
                f,
                "unexpected non-MBO record in mbo stream; raw record: {record}"
            ),
            IngestError::UndefinedField { field, record } => write!(
                f,
                "undefined {field} on a record where it is meaningful; raw record: {record}"
            ),
            IngestError::MissingDefinition { dataset, schema } => write!(
                f,
                "cannot build InstrumentMeta: dataset {dataset} on disk carries schema \
                 `{schema}` only; `min_price_increment` and `unit_of_measure_qty` come from \
                 the Databento `definition` schema (InstrumentDefMsg), which batch job \
                 GLBX-20260803-4WJS899FNL did not include. Acquire the definition schema \
                 for the week before writing fftlog headers — do not fabricate tick metadata. \
                 For `fft-ingest write`, pass --tick/--uom-qty/--display-factor explicitly."
            ),
            IngestError::Log(err) => write!(f, "fftlog error: {err}"),
            IngestError::Cli(msg) => write!(f, "{msg}"),
            IngestError::UnknownInstrument {
                instrument_id,
                hint,
            } => write!(f, "unknown instrument_id {instrument_id}: {hint}"),
            IngestError::NoEventsWritten {
                instrument_id,
                trade_date,
            } => write!(
                f,
                "no events for instrument_id {instrument_id} on trade date {trade_date}; \
                 refusing to write an empty fftlog"
            ),
        }
    }
}

impl std::error::Error for IngestError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            IngestError::Dbn(err) => Some(err),
            IngestError::Log(err) => Some(err),
            _ => None,
        }
    }
}

impl From<dbn::Error> for IngestError {
    fn from(err: dbn::Error) -> Self {
        IngestError::Dbn(err)
    }
}

impl From<fft_log::LogError> for IngestError {
    fn from(err: fft_log::LogError) -> Self {
        IngestError::Log(err)
    }
}

/// A canonical event plus the DBN instrument id it belongs to (`CanonicalEvent` itself is
/// single-instrument by design; the fftlog header carries the id). Synthesized gap events
/// carry `instrument_id` 0 — sequence numbering is channel-wide, not per-instrument.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct DecodedEvent {
    pub instrument_id: u32,
    pub event: CanonicalEvent,
}

/// Map one DBN MBO record to a canonical event. Pure; no sequence accounting.
pub fn map_mbo(rec: &MboMsg) -> Result<CanonicalEvent, IngestError> {
    let raw = || format!("{rec:?}");
    let kind = match u8::try_from(rec.action).unwrap_or(0) {
        b'A' => EventKind::Add,
        b'C' => EventKind::Cancel,
        b'M' => EventKind::Modify,
        b'T' => EventKind::Trade,
        b'F' => EventKind::Fill,
        b'R' => EventKind::Clear,
        action => {
            return Err(IngestError::UnmappableAction {
                action,
                record: raw(),
            });
        }
    };
    let side = match u8::try_from(rec.side).unwrap_or(0) {
        b'B' => Side::Bid,
        b'A' => Side::Ask,
        b'N' => Side::None,
        side => {
            return Err(IngestError::UnmappableSide {
                side,
                record: raw(),
            });
        }
    };
    let clear = kind == EventKind::Clear;
    let price = match rec.price {
        UNDEF_PRICE if clear => 0,
        UNDEF_PRICE => {
            return Err(IngestError::UndefinedField {
                field: "price",
                record: raw(),
            });
        }
        price => price,
    };
    let size = match rec.size {
        UNDEF_ORDER_SIZE if clear => 0,
        UNDEF_ORDER_SIZE => {
            return Err(IngestError::UndefinedField {
                field: "size",
                record: raw(),
            });
        }
        size => size,
    };
    Ok(CanonicalEvent {
        kind,
        side,
        flags: u16::from(rec.flags.raw()),
        size,
        ts: Ts(rec.hd.ts_event),
        seq: Seq(rec.sequence),
        price: Price(price),
        order_id: OrderId(rec.order_id),
    })
}

/// Channel-sequence accounting per DBN channel (`docs/FFTLOG-V2.md` §4 batch gap policy).
///
/// Batch files are symbol-filtered, so **forward** holes are expected filtering artifacts:
/// counted on [`seq_holes_ignored`](Self::seq_holes_ignored) and otherwise ignored — no
/// Gap record, no gap-state poison. A **regression** (`observed < expected`) is a genuine
/// anomaly and synthesizes a canonical Gap. Same-packet repeats (`seq == last`) and the
/// immediate successor are continuous.
///
/// Records with `sequence == 0` carry no venue sequence and are excluded from accounting
/// entirely. Measured on the sample week: Databento's synthetic week-open `Clear` records
/// all carry `sequence` 0 while the first sequenced record starts in the hundreds, so
/// seeding the baseline from 0 would fabricate a discontinuity at every week open.
#[derive(Default)]
pub struct GapDetector {
    last: Vec<(u8, u32)>,
    seq_holes_ignored: u64,
}

impl GapDetector {
    /// Observe a record's channel/sequence; returns a Gap only on sequence regression.
    pub fn observe(&mut self, channel_id: u8, seq: u32, ts: Ts) -> Option<CanonicalEvent> {
        if seq == 0 {
            return None;
        }
        match self.last.iter_mut().find(|(ch, _)| *ch == channel_id) {
            None => {
                self.last.push((channel_id, seq));
                None
            }
            Some((_, last)) => {
                let expected = u64::from(*last) + 1;
                let observed = u64::from(seq);
                if seq == *last || observed == expected {
                    *last = seq;
                    return None;
                }
                *last = seq;
                if observed < expected {
                    Some(CanonicalEvent::gap(ts, expected, observed))
                } else {
                    // Forward jump: filter artifact. Count loudly; do not synthesize Gap.
                    self.seq_holes_ignored += 1;
                    None
                }
            }
        }
    }

    /// Forward channel-seq holes ignored under the batch gap policy (one per discontinuity).
    pub fn seq_holes_ignored(&self) -> u64 {
        self.seq_holes_ignored
    }

    /// Channels seen so far.
    pub fn channels(&self) -> impl Iterator<Item = u8> {
        self.last.iter().map(|(ch, _)| *ch)
    }
}

/// Streaming DBN-MBO → canonical decoder with batch gap policy. Wraps the `dbn`
/// streaming decoder, so memory stays constant regardless of file size.
pub struct CanonicalDecoder<R> {
    inner: DbnDecoder<R>,
    gaps: GapDetector,
    pending: Option<DecodedEvent>,
    gap_count: u64,
    seq_holes_ignored: u64,
    /// Set when the most recent [`next_event`](Self::next_event) live record revealed a
    /// forward hole (no Gap emitted). Cleared on Gap delivery and on continuous records.
    last_forward_hole: bool,
}

/// Concrete decoder type for `.dbn.zst` files (the batch-download layout on disk).
pub type ZstdFileDecoder = CanonicalDecoder<zstd::stream::Decoder<'static, BufReader<File>>>;

/// Open a zstd-compressed DBN file for streaming canonical decode.
pub fn open_zstd_file(path: &Path) -> Result<ZstdFileDecoder, IngestError> {
    Ok(CanonicalDecoder::new(DbnDecoder::from_zstd_file(path)?))
}

impl<R: io::Read> CanonicalDecoder<R> {
    pub fn new(inner: DbnDecoder<R>) -> Self {
        Self {
            inner,
            gaps: GapDetector::default(),
            pending: None,
            gap_count: 0,
            seq_holes_ignored: 0,
            last_forward_hole: false,
        }
    }

    /// Install shared gap state so multi-file stitch continues sequence accounting
    /// across input boundaries (Globex day files are one continuous channel).
    pub fn set_gap_detector(&mut self, gaps: GapDetector) {
        self.gaps = gaps;
    }

    /// Take gap state after a file is exhausted (for the next input in a stitch).
    ///
    /// # Panics
    /// Panics if a gap event is still pending delivery — callers must drain
    /// [`next_event`](Self::next_event) to `None` first.
    pub fn into_gap_detector(self) -> GapDetector {
        assert!(
            self.pending.is_none(),
            "fft-ingest: into_gap_detector with a pending event; drain next_event first"
        );
        self.gaps
    }

    /// The DBN file metadata (dataset, query window, symbology mappings).
    pub fn metadata(&self) -> &Metadata {
        self.inner.metadata()
    }

    /// Sequence regressions synthesized so far **in this decoder instance** (not
    /// cumulative across a multi-file stitch that reuses [`GapDetector`] via
    /// [`set_gap_detector`](Self::set_gap_detector)).
    pub fn gap_count(&self) -> u64 {
        self.gap_count
    }

    /// Forward holes ignored so far **in this decoder instance** (same scoping as
    /// [`gap_count`](Self::gap_count)). Prefer [`GapDetector::seq_holes_ignored`] after
    /// [`into_gap_detector`](Self::into_gap_detector) for stitch-cumulative totals.
    pub fn seq_holes_ignored(&self) -> u64 {
        self.seq_holes_ignored
    }

    /// Whether the just-returned live event revealed a forward hole (batch filter
    /// artifact). Always `false` for Gap/snapshot deliveries.
    pub fn last_forward_hole(&self) -> bool {
        self.last_forward_hole
    }

    /// Distinct DBN channel ids seen so far.
    pub fn channels(&self) -> impl Iterator<Item = u8> {
        self.gaps.channels()
    }

    /// Next canonical event, `Ok(None)` at clean end of stream. A sequence regression
    /// yields the synthesized gap event first, then the record that revealed it.
    pub fn next_event(&mut self) -> Result<Option<DecodedEvent>, IngestError> {
        if let Some(pending) = self.pending.take() {
            self.last_forward_hole = false;
            return Ok(Some(pending));
        }
        let Some(rec_ref) = self.inner.decode_record_ref()? else {
            return Ok(None);
        };
        let Some(mbo) = rec_ref.get::<MboMsg>() else {
            return Err(IngestError::UnexpectedRecordType {
                record: format!("{rec_ref:?}"),
            });
        };
        let event = map_mbo(mbo)?;
        let decoded = DecodedEvent {
            instrument_id: mbo.hd.instrument_id,
            event,
        };
        // SNAPSHOT-flagged records (`src/flags.rs:14-15`) replay resting state with the
        // snapshot server's stale sequence/ts, not channel flow — measured on the sample
        // week: every day file opens with per-instrument Clear + Add snapshot records.
        // They pass through to the caller (flags intact) but never touch gap accounting.
        if mbo.flags.is_snapshot() {
            self.last_forward_hole = false;
            return Ok(Some(decoded));
        }
        let holes_before = self.gaps.seq_holes_ignored();
        if let Some(gap) = self.gaps.observe(mbo.channel_id, mbo.sequence, event.ts) {
            self.gap_count += 1;
            self.last_forward_hole = false;
            self.pending = Some(decoded);
            return Ok(Some(DecodedEvent {
                instrument_id: 0,
                event: gap,
            }));
        }
        let hole = self.gaps.seq_holes_ignored() - holes_before;
        self.seq_holes_ignored += hole;
        self.last_forward_hole = hole > 0;
        Ok(Some(decoded))
    }
}

/// Instrument tick metadata for the fftlog header. **Deliberately incomplete:** the data
/// on disk (schema `mbo` only) does not carry `min_price_increment` or
/// `unit_of_measure_qty`, and fabricating them from memory is forbidden (AGENTS.md,
/// doctrine rule 10). Always errs until a `definition`-schema source exists.
pub fn instrument_meta(dbn_metadata: &Metadata) -> Result<fft_core::InstrumentMeta, IngestError> {
    Err(IngestError::MissingDefinition {
        dataset: dbn_metadata.dataset.clone(),
        schema: dbn_metadata
            .schema
            .map_or_else(|| "mixed".to_owned(), |s| s.to_string()),
    })
}

/// Stable one-line text form of a decoded event — the golden-vector fixture format and
/// the `fft-ingest dump` output. Space-separated:
/// `instrument_id kind side flags size ts seq price order_id`.
pub fn canonical_line(ev: &DecodedEvent) -> String {
    let e = &ev.event;
    format!(
        "{} {:?} {:?} {} {} {} {} {} {}",
        ev.instrument_id, e.kind, e.side, e.flags, e.size, e.ts.0, e.seq.0, e.price.0, e.order_id.0
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use dbn::record::RecordHeader;
    use dbn::rtype;

    fn mbo(action: u8, side: u8, seq: u32) -> MboMsg {
        MboMsg {
            hd: RecordHeader::new::<MboMsg>(rtype::MBO, 1, 42, 1_785_276_000_000_000_000),
            order_id: 647_399_625_133,
            price: 6_420_250_000_000,
            size: 5,
            flags: dbn::FlagSet::from(130),
            channel_id: 0,
            action: action as std::ffi::c_char,
            side: side as std::ffi::c_char,
            ts_recv: 1_785_276_000_000_000_100,
            ts_in_delta: 100,
            sequence: seq,
        }
    }

    #[test]
    fn maps_all_book_actions() {
        for (action, kind) in [
            (b'A', EventKind::Add),
            (b'C', EventKind::Cancel),
            (b'M', EventKind::Modify),
            (b'T', EventKind::Trade),
            (b'F', EventKind::Fill),
            (b'R', EventKind::Clear),
        ] {
            let ev = map_mbo(&mbo(action, b'B', 7)).unwrap();
            assert_eq!(ev.kind, kind, "action {}", char::from(action));
            assert_eq!(ev.side, Side::Bid);
            assert_eq!(ev.flags, 130);
            assert_eq!(ev.size, 5);
            assert_eq!(ev.ts, Ts(1_785_276_000_000_000_000)); // ts_event, never ts_recv
            assert_eq!(ev.seq, Seq(7));
            assert_eq!(ev.price, Price(6_420_250_000_000));
            assert_eq!(ev.order_id, OrderId(647_399_625_133));
        }
    }

    #[test]
    fn action_none_is_a_loud_error() {
        let err = map_mbo(&mbo(b'N', b'N', 7)).unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("unmappable DBN action 'N'"), "{msg}");
        assert!(msg.contains("647399625133"), "raw record missing: {msg}");
    }

    #[test]
    fn undef_price_only_legal_on_clear() {
        let mut rec = mbo(b'R', b'N', 7);
        rec.price = UNDEF_PRICE;
        rec.size = UNDEF_ORDER_SIZE;
        let ev = map_mbo(&rec).unwrap();
        assert_eq!(
            (ev.kind, ev.price, ev.size),
            (EventKind::Clear, Price(0), 0)
        );

        let mut rec = mbo(b'A', b'B', 7);
        rec.price = UNDEF_PRICE;
        let msg = map_mbo(&rec).unwrap_err().to_string();
        assert!(msg.contains("undefined price"), "{msg}");
    }

    #[test]
    fn gap_synthesis() {
        let mut det = GapDetector::default();
        let ts = Ts(1);
        assert_eq!(det.observe(0, 0, ts), None); // seq 0 = unsequenced: never a baseline
        assert_eq!(det.observe(0, 100, ts), None); // first sequenced record: no baseline
        assert_eq!(det.observe(0, 100, ts), None); // same packet
        assert_eq!(det.observe(0, 101, ts), None); // successor
        // Forward jump: counted, no Gap.
        assert_eq!(det.observe(0, 107, ts), None);
        assert_eq!(det.seq_holes_ignored(), 1);
        // Regression (observed < expected) synthesizes a Gap.
        let gap = det
            .observe(0, 3, ts)
            .expect("regression must synthesize a gap");
        assert_eq!(gap.kind, EventKind::Gap);
        assert_eq!(gap.gap_seqs(), (108, 3));
        assert_eq!(det.seq_holes_ignored(), 1); // regressions do not increment holes
        // Channels are tracked independently.
        assert_eq!(det.observe(1, 500, ts), None);
        assert_eq!(det.observe(0, 4, ts), None);
    }
}
