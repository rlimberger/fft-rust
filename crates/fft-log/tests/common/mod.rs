//! Shared helpers for the fft-log integration tests: unique temp paths with cleanup,
//! a canonical ES-like `InstrumentMeta`, and deterministic event builders.
#![allow(dead_code)] // each test target compiles this module; not all use every helper.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use fft_core::{CanonicalEvent, EventKind, InstrumentMeta, OrderId, Price, Seq, Side, Ts};
use fft_log::{LogReader, LogWriter, OpenReport};

/// A temp file path that deletes itself on drop.
pub struct TempPath(pub PathBuf);

impl TempPath {
    pub fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TempPath {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

/// Unique path under the OS temp dir; never collides across tests or processes.
pub fn temp_path(name: &str) -> TempPath {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    TempPath(std::env::temp_dir().join(format!(
        "fft-log-test-{}-{n}-{name}.fftlog",
        std::process::id()
    )))
}

/// ESU6-flavoured instrument metadata.
pub fn es_meta() -> InstrumentMeta {
    InstrumentMeta {
        symbol: "ESU6".into(),
        instrument_id: 42,
        dataset: "GLBX.MDP3".into(),
        min_price_increment: Price(250_000_000),
        unit_of_measure_qty: 50_000_000_000,
        display_factor: 1,
        trade_date: 20_662,
        session_open: Ts(1_785_000_000_000_000_000),
    }
}

/// One deterministic event.
pub fn ev(kind: EventKind, side: Side, ts: u64, seq: u32) -> CanonicalEvent {
    CanonicalEvent {
        kind,
        side,
        flags: (seq % 251) as u16,
        size: 1 + seq % 20,
        ts: Ts(ts),
        seq: Seq(seq),
        price: Price(5_000_250_000_000 + i64::from(seq % 40) * 250_000_000),
        order_id: OrderId(u64::from(seq) * 7 + 1),
    }
}

/// `n` deterministic events with monotonically increasing timestamps starting at
/// `start_ts`, cycling through a realistic kind/side mix and including a Gap.
pub fn mono_events(n: usize, start_ts: u64, start_seq: u32) -> Vec<CanonicalEvent> {
    let kinds = [
        (EventKind::Add, Side::Bid),
        (EventKind::Add, Side::Ask),
        (EventKind::Cancel, Side::Bid),
        (EventKind::Modify, Side::Ask),
        (EventKind::Trade, Side::None),
        (EventKind::Fill, Side::Bid),
    ];
    (0..n)
        .map(|i| {
            let seq = start_seq + i as u32;
            let ts = start_ts + i as u64 * 350;
            if i == n / 2 && n > 4 {
                CanonicalEvent::gap(Ts(ts), u64::from(seq), u64::from(seq) + 5)
            } else {
                let (kind, side) = kinds[i % kinds.len()];
                ev(kind, side, ts, seq)
            }
        })
        .collect()
}

/// Write `batches` (one EVENTS frame each) and close cleanly. Returns the file bytes.
pub fn write_closed(path: &Path, batches: &[Vec<CanonicalEvent>]) -> Vec<u8> {
    let mut w = LogWriter::create(path, &es_meta()).expect("create");
    for b in batches {
        w.append_events(b).expect("append");
    }
    w.close().expect("close");
    std::fs::read(path).expect("read back")
}

/// Write `batches` and drop the writer without closing: the file stays LIVE, exactly
/// the §8 crash state. Returns the file bytes.
pub fn write_live(path: &Path, batches: &[Vec<CanonicalEvent>]) -> Vec<u8> {
    let mut w = LogWriter::create(path, &es_meta()).expect("create");
    for b in batches {
        w.append_events(b).expect("append");
    }
    drop(w); // no close: LIVE stays set
    std::fs::read(path).expect("read back")
}

/// Open and collect all events of every frame.
pub fn read_all_events(path: &Path) -> (Vec<CanonicalEvent>, OpenReport) {
    let (reader, report) = LogReader::open(path).expect("open");
    let events: Vec<CanonicalEvent> = reader
        .events(0..reader.frame_count())
        .collect::<Result<_, _>>()
        .expect("decode events");
    (events, report)
}
