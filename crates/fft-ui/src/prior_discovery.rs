//! Background discovery of prior-session data.
//!
//! Auto-ingest lives in [`crate::prior_ingest`]; public ingest helpers are re-exported here.

use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::fs;
use std::path::{Path, PathBuf};

use fft_core::InstrumentMeta;
use fft_log::LogReader;

pub use crate::prior_ingest::{
    auto_ingest_missing, candidate_trade_dates, dbn_files, missing_trade_dates, resolve_dbn_dir,
};

#[derive(Debug, Clone, PartialEq, Eq)]
struct Candidate {
    path: PathBuf,
    trade_date: u32,
    instrument_id: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiscoveredPrior {
    pub path: PathBuf,
    pub trade_date: u32,
}

#[derive(Debug, Clone)]
pub struct PriorOptions {
    pub discover: bool,
    pub auto_ingest: bool,
    pub dbn_dir: Option<PathBuf>,
}

#[derive(Debug)]
pub struct PriorDiscovery {
    pub current_meta: InstrumentMeta,
    pub cache_dir: Option<PathBuf>,
    pub sessions: Vec<DiscoveredPrior>,
    pub(crate) available_dates: BTreeSet<u32>,
}

/// Discover applicable prior logs beside `replay_path` and in the session cache.
///
/// Same-directory candidates win over cache candidates. Explicit paths supplement
/// discovery and win trade-date deduplication when they identify an applicable prior.
pub fn discover_prior_sessions(replay_path: &Path, explicit: &[PathBuf]) -> Option<PriorDiscovery> {
    let (reader, _) = match LogReader::open(replay_path) {
        Ok(opened) => opened,
        Err(err) => {
            eprintln!(
                "fft: warning: cannot read replay metadata for prior discovery {}: {err}",
                replay_path.display()
            );
            return None;
        }
    };
    let current_meta = reader.meta().clone();
    drop(reader);

    let explicit_dates = explicit
        .iter()
        .filter_map(|path| read_candidate_quiet(path))
        .filter(|candidate| {
            candidate.instrument_id == current_meta.instrument_id
                && candidate.trade_date < current_meta.trade_date
        })
        .map(|candidate| candidate.trade_date)
        .collect::<HashSet<_>>();

    let same_dir = replay_path.parent().unwrap_or_else(|| Path::new("."));
    let cache_dir = session_cache_dir();
    let sessions = discover_in_dirs(
        same_dir,
        cache_dir.as_deref(),
        replay_path,
        current_meta.instrument_id,
        current_meta.trade_date,
        &explicit_dates,
    );
    let available_dates = sessions
        .iter()
        .map(|session| session.trade_date)
        .chain(explicit_dates)
        .collect();
    Some(PriorDiscovery {
        current_meta,
        cache_dir,
        sessions,
        available_dates,
    })
}

pub fn session_cache_dir() -> Option<PathBuf> {
    std::env::var_os("XDG_CACHE_HOME")
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .or_else(|| {
            std::env::var_os("HOME")
                .filter(|value| !value.is_empty())
                .map(|home| PathBuf::from(home).join(".cache"))
        })
        .map(|root| root.join("fft/sessions"))
}

fn discover_in_dirs(
    same_dir: &Path,
    cache_dir: Option<&Path>,
    replay_path: &Path,
    instrument_id: u32,
    current_trade_date: u32,
    explicit_dates: &HashSet<u32>,
) -> Vec<DiscoveredPrior> {
    let mut by_date = BTreeMap::new();
    scan_dir(
        same_dir,
        replay_path,
        instrument_id,
        current_trade_date,
        explicit_dates,
        &mut by_date,
    );
    if let Some(cache_dir) = cache_dir
        && cache_dir != same_dir
    {
        scan_dir(
            cache_dir,
            replay_path,
            instrument_id,
            current_trade_date,
            explicit_dates,
            &mut by_date,
        );
    }
    by_date
        .into_iter()
        .map(|(trade_date, path)| DiscoveredPrior { path, trade_date })
        .collect()
}

fn scan_dir(
    dir: &Path,
    replay_path: &Path,
    instrument_id: u32,
    current_trade_date: u32,
    explicit_dates: &HashSet<u32>,
    by_date: &mut BTreeMap<u32, PathBuf>,
) {
    let entries = match fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return,
        Err(err) => {
            eprintln!(
                "fft: warning: cannot scan prior session directory {}: {err}",
                dir.display()
            );
            return;
        }
    };

    for entry in entries {
        let path = match entry {
            Ok(entry) => entry.path(),
            Err(err) => {
                eprintln!(
                    "fft: warning: cannot read entry in prior session directory {}: {err}",
                    dir.display()
                );
                continue;
            }
        };
        if path == replay_path || path.extension().and_then(|ext| ext.to_str()) != Some("fftlog") {
            continue;
        }
        let Some(candidate) = read_candidate(&path) else {
            continue;
        };
        if candidate.instrument_id != instrument_id
            || candidate.trade_date >= current_trade_date
            || explicit_dates.contains(&candidate.trade_date)
        {
            continue;
        }
        by_date
            .entry(candidate.trade_date)
            .or_insert(candidate.path);
    }
}

fn read_candidate_quiet(path: &Path) -> Option<Candidate> {
    LogReader::open(path).ok().map(|(reader, _)| Candidate {
        path: path.to_path_buf(),
        trade_date: reader.meta().trade_date,
        instrument_id: reader.meta().instrument_id,
    })
}

fn read_candidate(path: &Path) -> Option<Candidate> {
    match LogReader::open(path) {
        Ok((reader, _)) => Some(Candidate {
            path: path.to_path_buf(),
            trade_date: reader.meta().trade_date,
            instrument_id: reader.meta().instrument_id,
        }),
        Err(err) => {
            eprintln!(
                "fft: warning: cannot read prior-session candidate {}: {err}",
                path.display()
            );
            None
        }
    }
}

#[cfg(test)]
#[path = "prior_discovery_tests.rs"]
mod tests;
