//! Background discovery and auto-ingest of prior-session data.

use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use fft_core::InstrumentMeta;
use fft_ingest::write::{DEFAULT_BATCH_SIZE, WriteConfig, write_fftlog};
use fft_log::LogReader;
use jiff::civil::Date;

use crate::datetime::{civil_from_days, days_from_civil};

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
    available_dates: BTreeSet<u32>,
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
mod tests {
    use std::sync::atomic::{AtomicU64, Ordering};

    use fft_core::{Price, Ts};
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

    fn dbn_path(date: &str) -> PathBuf {
        PathBuf::from(format!("glbx-mdp3-{date}.mbo.dbn.zst"))
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

        let found = discover_prior_sessions(&replay, &[explicit_tue]).unwrap();

        assert!(found.sessions.is_empty());
    }

    #[test]
    fn candidate_dates_use_complete_utc_span_and_skip_weekends_and_current() {
        let files = [
            dbn_path("20260724"),
            dbn_path("20260725"),
            dbn_path("20260726"),
            dbn_path("20260727"),
            dbn_path("20260728"),
            dbn_path("20260729"),
            dbn_path("20260730"),
        ];
        let current = u32::try_from(days_from_civil(2026, 7, 29)).unwrap();

        let dates = candidate_trade_dates(current, &files);

        assert_eq!(
            dates,
            vec![
                u32::try_from(days_from_civil(2026, 7, 27)).unwrap(),
                u32::try_from(days_from_civil(2026, 7, 28)).unwrap(),
            ]
        );
    }

    #[test]
    fn skip_logic_excludes_discovered_and_cached_dates() {
        let candidates = [10, 11, 12, 13];
        let available = BTreeSet::from([10, 12]);
        assert_eq!(missing_trade_dates(&candidates, &available), vec![11, 13]);
    }

    #[test]
    #[ignore = "requires the large sample-week DBN data; run explicitly with --ignored"]
    fn real_data_auto_ingests_one_prior_day() {
        let root = TempDir::new();
        let dbn_dir =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("../../data/GLBX-20260803-4WJS899FNL");
        assert!(dbn_dir.is_dir(), "missing {}", dbn_dir.display());
        let trade_date = u32::try_from(days_from_civil(2026, 7, 27)).unwrap();
        let current = u32::try_from(days_from_civil(2026, 7, 28)).unwrap();
        let discovery = PriorDiscovery {
            current_meta: InstrumentMeta {
                symbol: "ESU6".into(),
                instrument_id: 42_140_870,
                dataset: "GLBX.MDP3".into(),
                min_price_increment: Price(250_000_000),
                unit_of_measure_qty: 50_000_000_000,
                display_factor: 1,
                trade_date: current,
                session_open: Ts(1),
            },
            cache_dir: Some(root.0.clone()),
            sessions: Vec::new(),
            available_dates: BTreeSet::new(),
        };
        let mut completed = Vec::new();

        auto_ingest_missing(&discovery, Some(&dbn_dir), |prior, events| {
            completed.push((prior, events));
        });

        assert_eq!(completed.len(), 1);
        assert_eq!(completed[0].0.trade_date, trade_date);
        assert!(completed[0].1 > 0);
        assert!(completed[0].0.path.is_file());
    }
}
