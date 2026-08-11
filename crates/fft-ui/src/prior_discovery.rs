//! Background discovery of existing prior-session fftlogs.

use std::collections::{BTreeMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};

use fft_log::LogReader;

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

/// Discover applicable prior logs beside `replay_path` and in the session cache.
///
/// Same-directory candidates win over cache candidates. Explicit paths supplement
/// discovery and win trade-date deduplication when they identify an applicable prior.
pub fn discover_prior_sessions(replay_path: &Path, explicit: &[PathBuf]) -> Vec<DiscoveredPrior> {
    let (reader, _) = match LogReader::open(replay_path) {
        Ok(opened) => opened,
        Err(err) => {
            eprintln!(
                "fft: warning: cannot read replay metadata for prior discovery {}: {err}",
                replay_path.display()
            );
            return Vec::new();
        }
    };
    let current_trade_date = reader.meta().trade_date;
    let instrument_id = reader.meta().instrument_id;
    drop(reader);

    let explicit_dates = explicit
        .iter()
        .filter_map(|path| read_candidate_quiet(path))
        .filter(|candidate| {
            candidate.instrument_id == instrument_id && candidate.trade_date < current_trade_date
        })
        .map(|candidate| candidate.trade_date)
        .collect::<HashSet<_>>();

    let same_dir = replay_path.parent().unwrap_or_else(|| Path::new("."));
    let cache_dir = session_cache_dir();
    discover_in_dirs(
        same_dir,
        cache_dir.as_deref(),
        replay_path,
        instrument_id,
        current_trade_date,
        &explicit_dates,
    )
}

fn session_cache_dir() -> Option<PathBuf> {
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
mod tests {
    use std::sync::atomic::{AtomicU64, Ordering};

    use fft_core::{InstrumentMeta, Price, Ts};
    use fft_log::LogWriter;

    use super::*;

    static NEXT_TEMP: AtomicU64 = AtomicU64::new(0);

    struct TempDir(PathBuf);

    impl TempDir {
        fn new() -> Self {
            let nonce = NEXT_TEMP.fetch_add(1, Ordering::Relaxed);
            let path = std::env::temp_dir().join(format!(
                "fft-prior-discovery-{}-{nonce}",
                std::process::id()
            ));
            fs::create_dir_all(&path).expect("create temp dir");
            Self(path)
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn write_log(path: &Path, trade_date: u32, instrument_id: u32) {
        let meta = InstrumentMeta {
            symbol: "ESU6".into(),
            instrument_id,
            dataset: "GLBX.MDP3".into(),
            min_price_increment: Price(250_000_000),
            unit_of_measure_qty: 50_000_000_000,
            display_factor: 1,
            trade_date,
            session_open: Ts(1),
        };
        LogWriter::create(path, &meta)
            .expect("create log")
            .close()
            .expect("close log");
    }

    #[test]
    fn filters_sorts_and_prefers_same_dir_over_cache() {
        let root = TempDir::new();
        let same = root.0.join("same");
        let cache = root.0.join("cache");
        fs::create_dir_all(&same).unwrap();
        fs::create_dir_all(&cache).unwrap();
        let replay = same.join("wed.fftlog");
        write_log(&replay, 20_302, 42);
        write_log(&same.join("tue.fftlog"), 20_301, 42);
        write_log(&cache.join("tue-cache.fftlog"), 20_301, 42);
        write_log(&cache.join("mon.fftlog"), 20_300, 42);
        write_log(&same.join("thu.fftlog"), 20_303, 42);
        write_log(&same.join("wrong-instrument.fftlog"), 20_299, 7);
        fs::write(same.join("notes.txt"), b"not a log").unwrap();

        let found = discover_in_dirs(&same, Some(&cache), &replay, 42, 20_302, &HashSet::new());

        assert_eq!(
            found,
            vec![
                DiscoveredPrior {
                    path: cache.join("mon.fftlog"),
                    trade_date: 20_300,
                },
                DiscoveredPrior {
                    path: same.join("tue.fftlog"),
                    trade_date: 20_301,
                },
            ]
        );
    }

    #[test]
    fn explicit_prior_wins_trade_date_deduplication() {
        let root = TempDir::new();
        let same = root.0.join("same");
        fs::create_dir_all(&same).unwrap();
        let replay = same.join("wed.fftlog");
        let discovered_tue = same.join("tue.fftlog");
        let explicit_tue = root.0.join("explicit-tue.fftlog");
        write_log(&replay, 20_302, 42);
        write_log(&discovered_tue, 20_301, 42);
        write_log(&explicit_tue, 20_301, 42);

        let found = discover_prior_sessions(&replay, &[explicit_tue]);

        assert!(found.is_empty());
    }
}
