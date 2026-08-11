//! Auto-ingest of missing prior-session days from DBN into the session cache.
//!
//! Split from `prior_discovery` so discovery scanning and ingest stay under the
//! 500-line house rule.

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use fft_ingest::write::{DEFAULT_BATCH_SIZE, WriteConfig, write_fftlog};
use jiff::civil::Date;

use crate::datetime::{civil_from_days, days_from_civil};
use crate::prior_discovery::{DiscoveredPrior, PriorDiscovery};

/// Resolve the first lexicographic `data/GLBX-*` directory under the current working
/// directory that contains `glbx-mdp3-*.mbo.dbn.zst`, unless explicitly overridden.
pub fn resolve_dbn_dir(override_dir: Option<&Path>) -> Option<PathBuf> {
    if let Some(dir) = override_dir {
        return Some(dir.to_path_buf());
    }
    let mut dirs = fs::read_dir("data")
        .ok()?
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| {
            path.is_dir()
                && path
                    .file_name()
                    .and_then(|name| name.to_str())
                    .is_some_and(|name| name.starts_with("GLBX-"))
        })
        .collect::<Vec<_>>();
    dirs.sort();
    dirs.into_iter().find(|dir| !dbn_files(dir).is_empty())
}

pub fn dbn_files(dir: &Path) -> Vec<PathBuf> {
    let mut files = fs::read_dir(dir)
        .into_iter()
        .flatten()
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| dbn_utc_date(path).is_some())
        .collect::<Vec<_>>();
    files.sort();
    files
}

/// Derive plausible CT trade dates from UTC daily-roll DBN file dates.
///
/// A Globex trade date starts at 17:00 CT on the prior civil day, so it spans the tail
/// of one UTC file and most of the next. Consequently, a UTC range `[first, last]` can
/// fully supply trade dates `[first + 1 day, last]`. We pass every DBN file to ingest and
/// let its America/Chicago admission filter select the requested date. Weekends and the
/// current/future trade date are excluded.
pub fn candidate_trade_dates(current_trade_date: u32, files: &[PathBuf]) -> Vec<u32> {
    let Some(first) = files.iter().filter_map(|path| dbn_utc_date(path)).min() else {
        return Vec::new();
    };
    let Some(last) = files.iter().filter_map(|path| dbn_utc_date(path)).max() else {
        return Vec::new();
    };
    ((first + 1)..=last)
        .filter(|day| *day < current_trade_date && is_weekday(*day))
        .collect()
}

pub fn missing_trade_dates(candidates: &[u32], available: &BTreeSet<u32>) -> Vec<u32> {
    candidates
        .iter()
        .copied()
        .filter(|date| !available.contains(date))
        .collect()
}

/// Ingest missing candidate days oldest-first, invoking `completed` after each atomic
/// cache publication so the caller can progressively dispatch it to the engine.
pub fn auto_ingest_missing(
    discovery: &PriorDiscovery,
    dbn_dir_override: Option<&Path>,
    mut completed: impl FnMut(DiscoveredPrior, u64),
) {
    let Some(cache_dir) = discovery.cache_dir.as_deref() else {
        eprintln!("fft: warning: cannot resolve cache directory for auto-ingest");
        return;
    };
    let Some(dbn_dir) = resolve_dbn_dir(dbn_dir_override) else {
        eprintln!("fft: warning: no DBN source directory found for auto-ingest");
        return;
    };
    let inputs = dbn_files(&dbn_dir);
    if inputs.is_empty() {
        eprintln!(
            "fft: warning: no glbx-mdp3-*.mbo.dbn.zst files found in {}",
            dbn_dir.display()
        );
        return;
    }
    if let Err(err) = fs::create_dir_all(cache_dir) {
        eprintln!(
            "fft: warning: cannot create auto-ingest cache {}: {err}",
            cache_dir.display()
        );
        return;
    }

    let candidates = candidate_trade_dates(discovery.current_meta.trade_date, &inputs);
    for trade_date in missing_trade_dates(&candidates, &discovery.available_dates) {
        let date_text = format_trade_date(trade_date);
        let output = cache_dir.join(format!(
            "{}-{date_text}.fftlog",
            discovery.current_meta.symbol
        ));
        let temp = temp_output(cache_dir, &discovery.current_meta.symbol, trade_date);
        let date = trade_date_to_jiff(trade_date);
        eprintln!(
            "fft: auto-ingesting {date_text} from {} DBN files → {}",
            inputs.len(),
            output.display()
        );
        let config = WriteConfig {
            output: temp.clone(),
            inputs: inputs.clone(),
            instrument_id: discovery.current_meta.instrument_id,
            symbol: Some(discovery.current_meta.symbol.clone()),
            trade_date: date,
            min_price_increment: discovery.current_meta.min_price_increment,
            unit_of_measure_qty: discovery.current_meta.unit_of_measure_qty,
            display_factor: discovery.current_meta.display_factor,
            batch_size: DEFAULT_BATCH_SIZE,
        };
        match write_fftlog(&config) {
            Ok(stats) => {
                if let Err(err) = fs::rename(&temp, &output) {
                    eprintln!(
                        "fft: warning: auto-ingest publish failed for {date_text} at {}: {err}",
                        output.display()
                    );
                    let _ = fs::remove_file(&temp);
                    continue;
                }
                eprintln!(
                    "fft: auto-ingest complete {date_text}: {} events at {}",
                    stats.events_written,
                    output.display()
                );
                completed(
                    DiscoveredPrior {
                        path: output,
                        trade_date,
                    },
                    stats.events_written,
                );
            }
            Err(err) => {
                eprintln!("fft: warning: auto-ingest failed for {date_text}: {err}");
                let _ = fs::remove_file(&temp);
            }
        }
    }
}

fn dbn_utc_date(path: &Path) -> Option<u32> {
    let name = path.file_name()?.to_str()?;
    let digits = name
        .strip_prefix("glbx-mdp3-")?
        .strip_suffix(".mbo.dbn.zst")?;
    if digits.len() != 8 || !digits.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    let year = digits[0..4].parse().ok()?;
    let month = digits[4..6].parse().ok()?;
    let day = digits[6..8].parse().ok()?;
    u32::try_from(days_from_civil(year, month, day)).ok()
}

fn is_weekday(day: u32) -> bool {
    // 1970-01-01 was Thursday; Monday=0 through Sunday=6.
    matches!((day + 3) % 7, 0..=4)
}

fn format_trade_date(trade_date: u32) -> String {
    let (year, month, day) = civil_from_days(i64::from(trade_date));
    format!("{year:04}-{month:02}-{day:02}")
}

fn trade_date_to_jiff(trade_date: u32) -> Date {
    let (year, month, day) = civil_from_days(i64::from(trade_date));
    Date::new(
        i16::try_from(year).expect("trade-date year fits i16"),
        i8::try_from(month).expect("trade-date month fits i8"),
        i8::try_from(day).expect("trade-date day fits i8"),
    )
    .expect("valid trade date")
}

fn temp_output(cache_dir: &Path, symbol: &str, trade_date: u32) -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| duration.as_nanos());
    cache_dir.join(format!(
        ".{symbol}-{trade_date}-{}-{nanos}.fftlog.tmp",
        std::process::id()
    ))
}

#[cfg(test)]
#[path = "prior_ingest_tests.rs"]
mod tests;
