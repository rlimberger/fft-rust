//! Golden DBN → fftlog v2 → LogReader roundtrip.
//! Paths resolve from `CARGO_MANIFEST_DIR` (docs/FIXTURES.md).

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use fft_ingest::decode::canonical_line;
use fft_ingest::write::{
    ES_HELP_DISPLAY_FACTOR, ES_HELP_TICK, ES_HELP_UOM_QTY, WriteConfig, decode_filtered,
    write_fftlog,
};
use fft_log::LogReader;
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

#[test]
fn wed_head_write_roundtrip_matches_decoder_and_expect() {
    let dbn = fixtures_ingest().join("glbx-mdp3-20260729-head.dbn.zst");
    let expect_path = fixtures_ingest().join("glbx-mdp3-20260729-head.expect");
    assert!(dbn.is_file(), "missing {}", dbn.display());

    let trade_date = date(2026, 7, 29);
    let decoded =
        decode_filtered(&dbn, GOLDEN_INSTRUMENT_ID, trade_date).expect("decode filtered golden");
    assert!(
        decoded.len() >= 50,
        "golden filter produced {} events, need ≥ 50",
        decoded.len()
    );

    let out = temp_fftlog("wed-head");
    let _ = std::fs::remove_file(&out);
    let cfg = WriteConfig {
        output: out.clone(),
        inputs: vec![dbn.clone()],
        instrument_id: GOLDEN_INSTRUMENT_ID,
        symbol: Some("GOLDEN-10252".into()), // fixture head has no ESU6 mapping for 10252
        trade_date,
        min_price_increment: fft_core::Price(ES_HELP_TICK),
        unit_of_measure_qty: ES_HELP_UOM_QTY,
        display_factor: ES_HELP_DISPLAY_FACTOR,
        batch_size: 32,
    };
    let stats = write_fftlog(&cfg).expect("write fftlog");
    assert_eq!(stats.events_written, decoded.len() as u64);

    let (reader, report) = LogReader::open(&out).expect("open fftlog");
    assert!(
        report.recovery.is_none(),
        "clean close must not report LIVE recovery: {:?}",
        report.warnings
    );
    assert_eq!(reader.meta().instrument_id, GOLDEN_INSTRUMENT_ID);
    assert_eq!(reader.meta().symbol, "GOLDEN-10252");
    assert_eq!(reader.meta().min_price_increment.0, ES_HELP_TICK);
    assert_eq!(reader.meta().unit_of_measure_qty, ES_HELP_UOM_QTY);

    let from_log: Vec<_> = reader
        .events(0..reader.frame_count())
        .collect::<Result<_, _>>()
        .expect("decode log events");
    assert_eq!(from_log.len(), decoded.len());
    for (i, (got, want)) in from_log.iter().zip(decoded.iter()).enumerate() {
        assert_eq!(got, &want.event, "event {i} diverged");
    }

    let expect = std::fs::read_to_string(&expect_path).expect("read expect");
    let expect_lines: Vec<&str> = expect.lines().collect();
    assert_eq!(expect_lines.len(), 50);
    let got_lines: Vec<String> = decoded.iter().take(50).map(canonical_line).collect();
    assert_eq!(
        got_lines, expect_lines,
        "filtered decode diverged from golden expect"
    );

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
