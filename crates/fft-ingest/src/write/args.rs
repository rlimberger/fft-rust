//! `fft-ingest write` CLI argv parsing and usage text.

use std::path::PathBuf;

use fft_core::Price;
use jiff::civil::Date;

use crate::decode::{IngestError, open_zstd_file};

use super::{
    DEFAULT_BATCH_SIZE, DEFAULT_INSTRUMENT_ID, ES_HELP_DISPLAY_FACTOR, ES_HELP_TICK,
    ES_HELP_UOM_QTY, WriteConfig, id_for_symbol,
};

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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session;
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
