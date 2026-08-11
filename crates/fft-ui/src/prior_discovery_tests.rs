use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};
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

    let found = discover_prior_sessions(&replay, &[explicit_tue]).unwrap();

    assert!(found.sessions.is_empty());
}
