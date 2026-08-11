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

use dbn::Metadata;
use fft_core::CanonicalEvent;

mod decoder;
mod error;
mod gap;
mod mbo;

pub use decoder::{CanonicalDecoder, ZstdFileDecoder, open_zstd_file};
pub use error::IngestError;
pub use gap::GapDetector;
pub use mbo::map_mbo;

/// A canonical event plus the DBN instrument id it belongs to (`CanonicalEvent` itself is
/// single-instrument by design; the fftlog header carries the id). Synthesized gap events
/// carry `instrument_id` 0 — sequence numbering is channel-wide, not per-instrument.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct DecodedEvent {
    pub instrument_id: u32,
    pub event: CanonicalEvent,
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
