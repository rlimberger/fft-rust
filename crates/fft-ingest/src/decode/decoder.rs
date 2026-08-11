//! Streaming DBN-MBO → canonical decoder with batch gap policy.

use std::fs::File;
use std::io::{self, BufReader};
use std::path::Path;

use dbn::decode::dbn::Decoder as DbnDecoder;
use dbn::decode::{DbnMetadata, DecodeRecordRef};
use dbn::{MboMsg, Metadata};

use super::DecodedEvent;
use super::error::IngestError;
use super::gap::GapDetector;
use super::mbo::map_mbo;

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
