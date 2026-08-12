//! Unit tests for `shell_replay` (kept separate so the module stays under ~500 lines).

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use fft_core::{CanonicalEvent, EventKind, InstrumentMeta, OrderId, Price, Seq, Side, Ts};
use fft_log::LogWriter;

use super::*;

static NEXT_TEMP: AtomicU64 = AtomicU64::new(0);

struct TempPath(PathBuf);

impl TempPath {
    fn new(name: &str) -> Self {
        let nonce = NEXT_TEMP.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "fft-shell-replay-{}-{nonce}-{name}.fftlog",
            std::process::id()
        ));
        Self(path)
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TempPath {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

fn es_meta() -> InstrumentMeta {
    InstrumentMeta {
        symbol: "ESU6".into(),
        instrument_id: 42,
        dataset: "GLBX.MDP3".into(),
        min_price_increment: Price(250_000_000),
        unit_of_measure_qty: 50_000_000_000,
        display_factor: 1,
        trade_date: 20_663,
        session_open: Ts(1_000),
    }
}

fn add(ts: u64, seq: u32) -> CanonicalEvent {
    CanonicalEvent {
        kind: EventKind::Add,
        side: Side::Bid,
        flags: 0,
        size: 1,
        ts: Ts(ts),
        seq: Seq(seq),
        price: Price(5_000_000_000_000),
        order_id: OrderId(u64::from(seq)),
    }
}

fn write_events_log(path: &Path, timestamps: &[u64]) {
    let mut writer = LogWriter::create(path, &es_meta()).expect("create log");
    let events: Vec<_> = timestamps
        .iter()
        .enumerate()
        .map(|(i, &ts)| add(ts, (i as u32) + 1))
        .collect();
    writer.append_events(&events).expect("append");
    writer.close().expect("close");
}

#[test]
fn snap_sim_live_head_uses_last_event_at_or_before_head() {
    let tmp = TempPath::new("snap-floor");
    write_events_log(tmp.path(), &[1_000, 2_000, 3_000, 4_000]);

    let snapped = snap_sim_live_head_result(tmp.path(), 3_500).expect("snap");
    assert_eq!(snapped, 3_000);
}

#[test]
fn snap_sim_live_head_exact_event_ts_returns_that_ts() {
    let tmp = TempPath::new("snap-exact");
    write_events_log(tmp.path(), &[1_000, 2_000, 3_000, 4_000]);

    let snapped = snap_sim_live_head_result(tmp.path(), 2_000).expect("snap");
    assert_eq!(snapped, 2_000);
}

#[test]
fn snap_sim_live_head_past_last_event_returns_last_ts() {
    let tmp = TempPath::new("snap-past-eof");
    write_events_log(tmp.path(), &[1_000, 2_000, 3_000]);

    let snapped = snap_sim_live_head_result(tmp.path(), 9_999).expect("snap");
    assert_eq!(snapped, 3_000);
}

#[test]
fn snap_sim_live_head_same_ts_burst_returns_that_ts() {
    let tmp = TempPath::new("snap-same-ts");
    write_events_log(tmp.path(), &[1_000, 2_000, 2_000, 3_000]);

    let snapped = snap_sim_live_head_result(tmp.path(), 2_000).expect("snap");
    assert_eq!(snapped, 2_000);
}

#[test]
fn snap_sim_live_head_before_first_event_is_err() {
    let tmp = TempPath::new("snap-before");
    write_events_log(tmp.path(), &[1_000, 2_000]);

    let err = snap_sim_live_head_result(tmp.path(), 500).expect_err("head before log");
    assert!(
        err.contains("no in-log event at or before"),
        "unexpected err: {err}"
    );
}

#[test]
fn snap_sim_live_head_missing_log_is_err() {
    let missing = std::env::temp_dir().join(format!(
        "fft-shell-replay-missing-{}-{}.fftlog",
        std::process::id(),
        NEXT_TEMP.fetch_add(1, Ordering::Relaxed)
    ));
    let err = snap_sim_live_head_result(&missing, 1).expect_err("missing log");
    assert!(
        err.contains("failed to open --sim-live log"),
        "unexpected err: {err}"
    );
}

#[test]
fn startup_source_meta_path_and_starts_engine() {
    let replay_path = PathBuf::from("/tmp/replay.fftlog");
    let sim_path = PathBuf::from("/tmp/sim.fftlog");
    let live_out = PathBuf::from("/tmp/live.fftlog");

    assert_eq!(StartupSource::None.meta_path(), None);
    assert!(!StartupSource::None.starts_engine());

    let replay = StartupSource::Replay {
        path: replay_path.clone(),
        replay_at: Some(1),
    };
    assert_eq!(replay.meta_path(), Some(replay_path));
    assert!(replay.starts_engine());

    let sim = StartupSource::SimLive {
        path: sim_path.clone(),
        head_ts: 9,
        live_out,
    };
    assert_eq!(sim.meta_path(), Some(sim_path));
    assert!(sim.starts_engine());
}
