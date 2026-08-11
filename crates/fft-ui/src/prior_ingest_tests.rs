use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use fft_core::{InstrumentMeta, Price, Ts};

use crate::datetime::days_from_civil;
use crate::prior_discovery::PriorDiscovery;

use super::*;

static NEXT_TEMP: AtomicU64 = AtomicU64::new(0);

struct TempDir(PathBuf);

impl TempDir {
    fn new() -> Self {
        let nonce = NEXT_TEMP.fetch_add(1, Ordering::Relaxed);
        let path =
            std::env::temp_dir().join(format!("fft-prior-ingest-{}-{nonce}", std::process::id()));
        fs::create_dir_all(&path).expect("create temp dir");
        Self(path)
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn dbn_path(date: &str) -> PathBuf {
    PathBuf::from(format!("glbx-mdp3-{date}.mbo.dbn.zst"))
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
    let dbn_dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../data/GLBX-20260803-4WJS899FNL");
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
