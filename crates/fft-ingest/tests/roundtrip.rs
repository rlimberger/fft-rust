//! Golden DBN decode + write-path behaviour under §4 snapshot admission.
//! Paths resolve from `CARGO_MANIFEST_DIR` (docs/FIXTURES.md).

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use fft_ingest::decode::{canonical_line, open_zstd_file};
use fft_ingest::write::{
    ES_HELP_DISPLAY_FACTOR, ES_HELP_TICK, ES_HELP_UOM_QTY, WriteConfig, decode_filtered,
    write_fftlog,
};
use jiff::civil::date;

fn fixtures_ingest() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/ingest")
}

fn temp_fftlog(name: &str) -> PathBuf {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    std::env::temp_dir().join(format!(
        "fft-ingest-rt-{}-{n}-{name}.fftlog",
        std::process::id()
    ))
}

/// The committed golden head is all SNAPSHOT records for instrument 10252 (first
/// instrument in the Wed day-file slice), not ESU6 42140870.
const GOLDEN_INSTRUMENT_ID: u32 = 10_252;

/// Raw decode of the golden head still matches the committed expect vector (admission
/// is a write-path concern; the expect pins the DBN→canonical map).
#[test]
fn wed_head_raw_decode_matches_expect() {
    let dbn = fixtures_ingest().join("glbx-mdp3-20260729-head.dbn.zst");
    let expect_path = fixtures_ingest().join("glbx-mdp3-20260729-head.expect");
    assert!(dbn.is_file(), "missing {}", dbn.display());

    let expect = std::fs::read_to_string(&expect_path).expect("read expect");
    let expect_lines: Vec<&str> = expect.lines().collect();
    assert_eq!(expect_lines.len(), 50);

    let mut decoder = open_zstd_file(&dbn).expect("open golden DBN");
    let mut got = Vec::with_capacity(50);
    while got.len() < 50 {
        let ev = decoder
            .next_event()
            .expect("decode")
            .unwrap_or_else(|| panic!("fixture ended after {} events, wanted 50", got.len()));
        got.push(canonical_line(&ev));
    }
    assert_eq!(got, expect_lines);
}

/// Truncated pure-snapshot head has no non-snapshot event, so §4 admission cannot
/// establish a trade-date decision and drops the block — write refuses empty output.
#[test]
fn pure_snapshot_head_is_not_admitted_without_live_anchor() {
    let dbn = fixtures_ingest().join("glbx-mdp3-20260729-head.dbn.zst");
    let trade_date = date(2026, 7, 29);
    let decoded = decode_filtered(&dbn, GOLDEN_INSTRUMENT_ID, trade_date).expect("decode filtered");
    assert!(
        decoded.is_empty(),
        "snapshot-only truncated head must not admit without a non-snapshot anchor; got {}",
        decoded.len()
    );

    let out = temp_fftlog("wed-head-empty");
    let _ = std::fs::remove_file(&out);
    let cfg = WriteConfig {
        output: out.clone(),
        inputs: vec![dbn],
        instrument_id: GOLDEN_INSTRUMENT_ID,
        symbol: Some("GOLDEN-10252".into()),
        trade_date,
        min_price_increment: fft_core::Price(ES_HELP_TICK),
        unit_of_measure_qty: ES_HELP_UOM_QTY,
        display_factor: ES_HELP_DISPLAY_FACTOR,
        batch_size: 32,
    };
    let err = write_fftlog(&cfg).expect_err("must refuse empty write after admission drop");
    let msg = err.to_string();
    assert!(msg.contains("no events"), "{msg}");
    assert!(!out.exists(), "must not leave a partial file");
    let _ = std::fs::remove_file(&out);
}

#[test]
fn write_fails_loudly_without_matching_events() {
    let dbn = fixtures_ingest().join("glbx-mdp3-20260729-head.dbn.zst");
    let out = temp_fftlog("empty");
    let _ = std::fs::remove_file(&out);
    let cfg = WriteConfig {
        output: out.clone(),
        inputs: vec![dbn],
        instrument_id: 42_140_870, // ESU6 — absent from the 100-record head slice
        symbol: Some("ESU6".into()),
        trade_date: date(2026, 7, 29),
        min_price_increment: fft_core::Price(ES_HELP_TICK),
        unit_of_measure_qty: ES_HELP_UOM_QTY,
        display_factor: ES_HELP_DISPLAY_FACTOR,
        batch_size: 64,
    };
    let err = write_fftlog(&cfg).expect_err("must refuse empty write");
    let msg = err.to_string();
    assert!(msg.contains("no events"), "{msg}");
    assert!(msg.contains("42140870"), "{msg}");
    assert!(
        !out.exists(),
        "must not leave a partial file on empty filter"
    );
    let _ = std::fs::remove_file(&out);
}
