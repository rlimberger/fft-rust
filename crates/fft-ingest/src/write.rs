//! DBN → fftlog v2 writer.
//!
//! Instrument tick fields are **never** invented from the mbo-only batch: callers must
//! supply `min_price_increment`, `unit_of_measure_qty`, and `display_factor` explicitly
//! (CLI option A). ES reference values are printed in `write` help only.
//!
//! CHECKPOINT frames are intentionally skipped here — book/profile/CVD/refresh
//! sections are the engine's job (`docs/FFTLOG-V2.md` §5). `LogWriter` does not require
//! checkpoints; a clean `close()` with EVENTS frames + footer is a valid session log.

mod admit;
mod args;

use std::path::{Path, PathBuf};

use dbn::Metadata;
use fft_core::{CanonicalEvent, InstrumentMeta, Price};
use fft_log::LogWriter;
use jiff::civil::Date;

use crate::decode::{DecodedEvent, GapDetector, IngestError, open_zstd_file};
use crate::session::{self, TradeDateBucketer};

use admit::{FileAdmission, admit_event};

/// Default CME ES front-month instrument id for the sample week (ESU6).
pub const DEFAULT_INSTRUMENT_ID: u32 = 42_140_870;

/// Default EVENTS-frame batch size (events per `LogWriter::append_events` call).
pub const DEFAULT_BATCH_SIZE: usize = 8_192;

/// ES reference tick fields for help text only — never applied unless the caller passes
/// the matching flags. Units are Databento 1e-9 fixed-point.
pub const ES_HELP_TICK: i64 = 250_000_000;
pub const ES_HELP_UOM_QTY: i64 = 50_000_000_000;
pub const ES_HELP_DISPLAY_FACTOR: i64 = 1;

pub use args::{parse_write_args, write_usage};

/// Inputs and explicit header meta for one fftlog write.
#[derive(Debug, Clone)]
pub struct WriteConfig {
    pub output: PathBuf,
    pub inputs: Vec<PathBuf>,
    pub instrument_id: u32,
    /// When set, stamped into the header as-is; otherwise resolved from DBN symbology.
    pub symbol: Option<String>,
    pub trade_date: Date,
    pub min_price_increment: Price,
    pub unit_of_measure_qty: i64,
    pub display_factor: i64,
    pub batch_size: usize,
}

/// Summary printed by `fft-ingest write`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WriteStats {
    pub events_written: u64,
    pub frames: u64,
    /// Sequence-regression Gaps kept after trade-date filter. Nonzero is a real anomaly.
    pub gaps_kept: u64,
    pub snapshots_kept: u64,
    /// SNAPSHOT-flagged records for the instrument that were dropped because the file's
    /// first non-snapshot event did not bucket to the target trade date
    /// (`docs/FFTLOG-V2.md` §4 snapshot admission).
    pub snapshots_dropped: u64,
    /// Forward channel-seq holes ignored under the batch gap policy (filter artifacts).
    pub seq_holes_ignored: u64,
}

/// Resolve `instrument_id` → raw symbol from DBN metadata mappings.
pub fn symbol_for_id(metadata: &Metadata, instrument_id: u32) -> Option<String> {
    for mapping in &metadata.mappings {
        for interval in &mapping.intervals {
            if interval.symbol.parse::<u32>().ok() == Some(instrument_id) {
                return Some(mapping.raw_symbol.clone());
            }
        }
    }
    None
}

/// Resolve raw symbol → `instrument_id` from DBN metadata mappings.
pub fn id_for_symbol(metadata: &Metadata, symbol: &str) -> Option<u32> {
    for mapping in &metadata.mappings {
        if mapping.raw_symbol == symbol {
            for interval in &mapping.intervals {
                if let Ok(id) = interval.symbol.parse::<u32>() {
                    return Some(id);
                }
            }
        }
    }
    None
}

impl WriteConfig {
    /// Build header meta. Symbol comes from `self.symbol` or the first input's mappings.
    pub fn build_meta(&self, dataset: &str, symbol: String) -> InstrumentMeta {
        InstrumentMeta {
            symbol,
            instrument_id: self.instrument_id,
            dataset: dataset.to_owned(),
            min_price_increment: self.min_price_increment,
            unit_of_measure_qty: self.unit_of_measure_qty,
            display_factor: self.display_factor,
            trade_date: session::trade_date_days(self.trade_date),
            session_open: session::session_open(self.trade_date),
        }
    }
}

/// Decode `inputs`, filter to one instrument + CT trade date, append EVENTS frames, close.
pub fn write_fftlog(config: &WriteConfig) -> Result<WriteStats, IngestError> {
    if config.inputs.is_empty() {
        return Err(IngestError::Cli(
            "write requires at least one .dbn.zst input".into(),
        ));
    }
    if config.batch_size == 0 {
        return Err(IngestError::Cli("--batch-size must be > 0".into()));
    }

    let first = open_zstd_file(&config.inputs[0])?;
    let dataset = first.metadata().dataset.clone();
    let symbol = match &config.symbol {
        Some(s) => s.clone(),
        None => symbol_for_id(first.metadata(), config.instrument_id).ok_or_else(|| {
            IngestError::UnknownInstrument {
                instrument_id: config.instrument_id,
                hint: "pass --symbol explicitly or use an id present in the DBN mappings".into(),
            }
        })?,
    };
    drop(first);

    let meta = config.build_meta(&dataset, symbol);
    let mut writer = LogWriter::create(&config.output, &meta)?;
    // Shared across the ordered input list: Globex day files are one continuous
    // channel sequence. A fresh GapDetector per file would miss boundary gaps.
    let mut gaps = GapDetector::default();
    let mut bucketer = TradeDateBucketer::default();
    let mut batch: Vec<CanonicalEvent> = Vec::with_capacity(config.batch_size);
    let mut stats = WriteStats {
        events_written: 0,
        frames: 0,
        gaps_kept: 0,
        snapshots_kept: 0,
        snapshots_dropped: 0,
        seq_holes_ignored: 0,
    };

    for path in &config.inputs {
        let mut decoder = open_zstd_file(path)?;
        decoder.set_gap_detector(std::mem::take(&mut gaps));
        let mut admission = FileAdmission::new(config.instrument_id, config.trade_date);
        while let Some(ev) = decoder.next_event()? {
            let forward_hole = decoder.last_forward_hole();
            admit_event(
                ev,
                &mut admission,
                &mut bucketer,
                &mut stats,
                forward_hole,
                &mut |kept| {
                    batch.push(kept.event);
                },
            );
            while batch.len() >= config.batch_size {
                let frame: Vec<CanonicalEvent> = batch.drain(..config.batch_size).collect();
                writer.append_events(&frame)?;
                stats.events_written += frame.len() as u64;
                stats.frames += 1;
            }
        }
        admission.finish(&mut stats);
        gaps = decoder.into_gap_detector();
    }
    if !batch.is_empty() {
        flush_batch(&mut writer, &mut batch, &mut stats)?;
    }
    if stats.events_written == 0 {
        // Drop without close (LIVE stub) and remove — never leave a zero-event log.
        drop(writer);
        let _ = std::fs::remove_file(&config.output);
        return Err(IngestError::NoEventsWritten {
            instrument_id: config.instrument_id,
            trade_date: config.trade_date.to_string(),
        });
    }
    writer.close()?;
    Ok(stats)
}

fn flush_batch(
    writer: &mut LogWriter,
    batch: &mut Vec<CanonicalEvent>,
    stats: &mut WriteStats,
) -> Result<(), IngestError> {
    let n = batch.len() as u64;
    writer.append_events(batch)?;
    batch.clear();
    stats.events_written += n;
    stats.frames += 1;
    Ok(())
}

/// Decode path with the same §4 admission + filter as [`write_fftlog`] — used by tests.
pub fn decode_filtered(
    path: &Path,
    instrument_id: u32,
    trade_date: Date,
) -> Result<Vec<DecodedEvent>, IngestError> {
    let mut decoder = open_zstd_file(path)?;
    let mut bucketer = TradeDateBucketer::default();
    let mut admission = FileAdmission::new(instrument_id, trade_date);
    let mut stats = WriteStats {
        events_written: 0,
        frames: 0,
        gaps_kept: 0,
        snapshots_kept: 0,
        snapshots_dropped: 0,
        seq_holes_ignored: 0,
    };
    let mut out = Vec::new();
    while let Some(ev) = decoder.next_event()? {
        let forward_hole = decoder.last_forward_hole();
        admit_event(
            ev,
            &mut admission,
            &mut bucketer,
            &mut stats,
            forward_hole,
            &mut |kept| {
                out.push(kept);
            },
        );
    }
    admission.finish(&mut stats);
    Ok(out)
}
