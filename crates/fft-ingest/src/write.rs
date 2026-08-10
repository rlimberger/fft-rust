//! DBN → fftlog v2 writer.
//!
//! Instrument tick fields are **never** invented from the mbo-only batch: callers must
//! supply `min_price_increment`, `unit_of_measure_qty`, and `display_factor` explicitly
//! (CLI option A). ES reference values are printed in `write` help only.
//!
//! CHECKPOINT frames are intentionally skipped here — book/profile/CVD/refresh
//! sections are the engine's job (`docs/FFTLOG-V2.md` §5). `LogWriter` does not require
//! checkpoints; a clean `close()` with EVENTS frames + footer is a valid session log.

use std::path::{Path, PathBuf};

use dbn::Metadata;
use fft_core::{CanonicalEvent, EventKind, InstrumentMeta, Price};
use fft_log::LogWriter;
use jiff::civil::Date;

use crate::decode::{DecodedEvent, IngestError, open_zstd_file};
use crate::session::{self, TradeDateBucketer};

/// Default CME ES front-month instrument id for the sample week (ESU6).
pub const DEFAULT_INSTRUMENT_ID: u32 = 42_140_870;

/// Default EVENTS-frame batch size (events per `LogWriter::append_events` call).
pub const DEFAULT_BATCH_SIZE: usize = 8_192;

/// ES reference tick fields for help text only — never applied unless the caller passes
/// the matching flags. Units are Databento 1e-9 fixed-point.
pub const ES_HELP_TICK: i64 = 250_000_000;
pub const ES_HELP_UOM_QTY: i64 = 50_000_000_000;
pub const ES_HELP_DISPLAY_FACTOR: i64 = 1;

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
    pub gaps_kept: u64,
    pub snapshots_kept: u64,
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

fn is_snapshot(ev: &DecodedEvent) -> bool {
    ev.event.kind != EventKind::Gap && ev.event.flags & u16::from(dbn::flags::SNAPSHOT) != 0
}

/// Keep channel gaps and the selected instrument's events. SNAPSHOT records for that
/// instrument are kept regardless of trade-date bucket (day-file book replay uses a
/// snapshot-creation `ts_event`, not session time). Live events and gaps must bucket to
/// `trade_date`.
fn keep_event(
    ev: &DecodedEvent,
    instrument_id: u32,
    trade_date: Date,
    bucketer: &mut TradeDateBucketer,
) -> bool {
    match ev.event.kind {
        EventKind::Gap => bucketer.bucket(ev.event.ts) == trade_date,
        _ if ev.instrument_id != instrument_id => false,
        _ if is_snapshot(ev) => true,
        _ => bucketer.bucket(ev.event.ts) == trade_date,
    }
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
    let mut bucketer = TradeDateBucketer::default();
    let mut batch: Vec<CanonicalEvent> = Vec::with_capacity(config.batch_size);
    let mut stats = WriteStats {
        events_written: 0,
        frames: 0,
        gaps_kept: 0,
        snapshots_kept: 0,
    };

    for path in &config.inputs {
        let mut decoder = open_zstd_file(path)?;
        while let Some(ev) = decoder.next_event()? {
            if !keep_event(&ev, config.instrument_id, config.trade_date, &mut bucketer) {
                continue;
            }
            if ev.event.kind == EventKind::Gap {
                stats.gaps_kept += 1;
            } else if is_snapshot(&ev) {
                stats.snapshots_kept += 1;
            }
            batch.push(ev.event);
            if batch.len() >= config.batch_size {
                flush_batch(&mut writer, &mut batch, &mut stats)?;
            }
        }
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

/// Help text for the `write` subcommand (ES values are reference only).
pub fn write_usage() -> &'static str {
    "write <out.fftlog> <path.dbn.zst>... --trade-date YYYY-MM-DD \
     --tick N --uom-qty N --display-factor N [--instrument-id N] [--symbol SYM] [--batch-size N]\n  \
     tick fields are required (mbo files have no definition schema).\n  \
     ES reference (pass explicitly; never assumed): \
     --tick 250000000 --uom-qty 50000000000 --display-factor 1\n  \
     default --instrument-id 42140870 (ESU6 sample week); default --batch-size 8192\n  \
     checkpoints are not written (engine owns book/profile sections)"
}

/// Parse `write` argv (tokens after the `write` subcommand).
pub fn parse_write_args(args: &[String]) -> Result<WriteConfig, IngestError> {
    if args.len() < 2 {
        return Err(IngestError::Cli(write_usage().into()));
    }
    let output = PathBuf::from(&args[0]);
    let mut inputs: Vec<PathBuf> = Vec::new();
    let mut trade_date: Option<Date> = None;
    let mut tick: Option<i64> = None;
    let mut uom_qty: Option<i64> = None;
    let mut display_factor: Option<i64> = None;
    let mut instrument_id = DEFAULT_INSTRUMENT_ID;
    let mut instrument_id_set = false;
    let mut symbol: Option<String> = None;
    let mut batch_size = DEFAULT_BATCH_SIZE;

    let mut i = 1;
    while i < args.len() {
        match args[i].as_str() {
            "--trade-date" => {
                let v = expect_val(args, &mut i, "--trade-date")?;
                trade_date = Some(parse_date(&v)?);
            }
            "--tick" => {
                let v = expect_val(args, &mut i, "--tick")?;
                tick = Some(parse_i64(&v, "--tick")?);
            }
            "--uom-qty" => {
                let v = expect_val(args, &mut i, "--uom-qty")?;
                uom_qty = Some(parse_i64(&v, "--uom-qty")?);
            }
            "--display-factor" => {
                let v = expect_val(args, &mut i, "--display-factor")?;
                display_factor = Some(parse_i64(&v, "--display-factor")?);
            }
            "--instrument-id" => {
                let v = expect_val(args, &mut i, "--instrument-id")?;
                instrument_id = v
                    .parse::<u32>()
                    .map_err(|_| IngestError::Cli(format!("invalid --instrument-id value: {v}")))?;
                instrument_id_set = true;
            }
            "--symbol" => {
                symbol = Some(expect_val(args, &mut i, "--symbol")?);
            }
            "--batch-size" => {
                let v = expect_val(args, &mut i, "--batch-size")?;
                batch_size = v
                    .parse::<usize>()
                    .map_err(|_| IngestError::Cli(format!("invalid --batch-size value: {v}")))?;
            }
            flag if flag.starts_with("--") => {
                return Err(IngestError::Cli(format!("unknown write flag: {flag}")));
            }
            path => inputs.push(PathBuf::from(path)),
        }
        i += 1;
    }

    if inputs.is_empty() {
        return Err(IngestError::Cli(
            "write requires at least one input path".into(),
        ));
    }
    let trade_date = trade_date
        .ok_or_else(|| IngestError::Cli("write requires --trade-date YYYY-MM-DD".into()))?;
    let tick = tick.ok_or_else(|| {
        IngestError::Cli(format!(
            "write requires --tick N (ES reference {ES_HELP_TICK}; never assumed)"
        ))
    })?;
    let uom_qty = uom_qty.ok_or_else(|| {
        IngestError::Cli(format!(
            "write requires --uom-qty N (ES reference {ES_HELP_UOM_QTY}; never assumed)"
        ))
    })?;
    let display_factor = display_factor.ok_or_else(|| {
        IngestError::Cli(format!(
            "write requires --display-factor N (ES reference {ES_HELP_DISPLAY_FACTOR}; never assumed)"
        ))
    })?;

    // Optional: --symbol alone resolves instrument_id from the first input's mappings.
    if let Some(sym) = symbol.as_deref()
        && !instrument_id_set
    {
        let decoder = open_zstd_file(&inputs[0])?;
        instrument_id = id_for_symbol(decoder.metadata(), sym).ok_or_else(|| {
            IngestError::UnknownInstrument {
                instrument_id: 0,
                hint: format!("symbol {sym} not found in DBN metadata mappings"),
            }
        })?;
    }

    Ok(WriteConfig {
        output,
        inputs,
        instrument_id,
        symbol,
        trade_date,
        min_price_increment: Price(tick),
        unit_of_measure_qty: uom_qty,
        display_factor,
        batch_size,
    })
}

fn expect_val(args: &[String], i: &mut usize, flag: &str) -> Result<String, IngestError> {
    let v = args
        .get(*i + 1)
        .ok_or_else(|| IngestError::Cli(format!("{flag} requires a value")))?;
    *i += 1;
    Ok(v.clone())
}

fn parse_i64(s: &str, flag: &str) -> Result<i64, IngestError> {
    s.parse::<i64>()
        .map_err(|_| IngestError::Cli(format!("invalid {flag} value: {s}")))
}

fn parse_date(s: &str) -> Result<Date, IngestError> {
    let parts: Vec<&str> = s.split('-').collect();
    if parts.len() != 3 {
        return Err(IngestError::Cli(format!(
            "invalid --trade-date {s}; expected YYYY-MM-DD"
        )));
    }
    let y: i16 = parts[0]
        .parse()
        .map_err(|_| IngestError::Cli(format!("invalid --trade-date year: {s}")))?;
    let m: i8 = parts[1]
        .parse()
        .map_err(|_| IngestError::Cli(format!("invalid --trade-date month: {s}")))?;
    let d: i8 = parts[2]
        .parse()
        .map_err(|_| IngestError::Cli(format!("invalid --trade-date day: {s}")))?;
    Date::new(y, m, d).map_err(|err| IngestError::Cli(format!("invalid --trade-date {s}: {err}")))
}

/// Decode path with the same filter as [`write_fftlog`] — used by roundtrip tests.
pub fn decode_filtered(
    path: &Path,
    instrument_id: u32,
    trade_date: Date,
) -> Result<Vec<DecodedEvent>, IngestError> {
    let mut decoder = open_zstd_file(path)?;
    let mut bucketer = TradeDateBucketer::default();
    let mut out = Vec::new();
    while let Some(ev) = decoder.next_event()? {
        if keep_event(&ev, instrument_id, trade_date, &mut bucketer) {
            out.push(ev);
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use jiff::civil::date;

    #[test]
    fn parse_requires_tick_fields() {
        let args = ["out.fftlog".into(), "in.dbn.zst".into()];
        let err = parse_write_args(&args).unwrap_err().to_string();
        assert!(err.contains("--trade-date"), "{err}");
    }

    #[test]
    fn parse_full_es_explicit() {
        let args: Vec<String> = [
            "out.fftlog",
            "a.dbn.zst",
            "b.dbn.zst",
            "--trade-date",
            "2026-07-29",
            "--tick",
            "250000000",
            "--uom-qty",
            "50000000000",
            "--display-factor",
            "1",
            "--instrument-id",
            "42140870",
            "--symbol",
            "ESU6",
        ]
        .into_iter()
        .map(str::to_owned)
        .collect();
        let cfg = parse_write_args(&args).unwrap();
        assert_eq!(cfg.inputs.len(), 2);
        assert_eq!(cfg.instrument_id, DEFAULT_INSTRUMENT_ID);
        assert_eq!(cfg.symbol.as_deref(), Some("ESU6"));
        assert_eq!(cfg.trade_date, date(2026, 7, 29));
        assert_eq!(cfg.min_price_increment, Price(ES_HELP_TICK));
        assert_eq!(cfg.unit_of_measure_qty, ES_HELP_UOM_QTY);
        assert_eq!(cfg.display_factor, ES_HELP_DISPLAY_FACTOR);
        let meta = cfg.build_meta("GLBX.MDP3", "ESU6".into());
        assert_eq!(meta.session_open, session::session_open(date(2026, 7, 29)));
        assert_eq!(meta.trade_date, session::trade_date_days(date(2026, 7, 29)));
    }
}
