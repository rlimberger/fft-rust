//! Multi-file stitch: channel-sequence continuity, §4 snapshot admission, and Gap
//! trade-date bucketing across the CT session open.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use dbn::encode::EncodeRecord;
use dbn::encode::dbn::Encoder;
use dbn::record::RecordHeader;
use dbn::{MboMsg, Metadata, SType, Schema, rtype};
use fft_core::{EventKind, Ts};
use fft_ingest::session::{TradeDateBucketer, session_open, trade_date};
use fft_ingest::write::{
    ES_HELP_DISPLAY_FACTOR, ES_HELP_TICK, ES_HELP_UOM_QTY, WriteConfig, write_fftlog,
};
use fft_log::LogReader;
use jiff::civil::date;

const INSTRUMENT_ID: u32 = 42;
/// Just after Globex open for trade date 2026-07-29 (Tue 17:00 CT).
const WED_SESSION_TS: u64 = {
    // 2026-07-28 is day 20_662; July CT is CDT (UTC-5); 17:00 CT = 22:00 UTC.
    (20_662 * 86_400 + 22 * 3_600) * 1_000_000_000 + 1_000_000_000
};

fn temp_path(name: &str) -> PathBuf {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    std::env::temp_dir().join(format!(
        "fft-ingest-stitch-{}-{n}-{name}",
        std::process::id()
    ))
}

fn meta() -> Metadata {
    Metadata::builder()
        .dataset("GLBX.MDP3")
        .schema(Some(Schema::Mbo))
        .start(WED_SESSION_TS)
        .stype_in(Some(SType::InstrumentId))
        .stype_out(SType::InstrumentId)
        .build()
}

fn mbo_live(seq: u32, ts: u64) -> MboMsg {
    MboMsg {
        hd: RecordHeader::new::<MboMsg>(rtype::MBO, 1, INSTRUMENT_ID, ts),
        order_id: 1_000 + u64::from(seq),
        price: 6_420_250_000_000,
        size: 1,
        flags: dbn::FlagSet::from(0),
        channel_id: 0,
        action: b'A' as std::ffi::c_char,
        side: b'B' as std::ffi::c_char,
        ts_recv: ts + 100,
        ts_in_delta: 100,
        sequence: seq,
    }
}

fn mbo_snap(seq: u32, ts: u64) -> MboMsg {
    let mut m = mbo_live(seq, ts);
    m.flags = dbn::FlagSet::empty().set_snapshot();
    // Snapshot order ids distinct from live, and non-channel seqs by construction.
    m.order_id = 9_000_000 + u64::from(seq);
    m
}

fn write_dbn_records(path: &Path, records: &[MboMsg]) {
    let _ = std::fs::remove_file(path);
    let file = std::fs::File::create(path).expect("create dbn");
    let mut enc = Encoder::with_zstd(file, &meta()).expect("encoder");
    for rec in records {
        enc.encode_record(rec).expect("encode mbo");
    }
    enc.flush().expect("flush");
    drop(enc);
}

fn write_dbn(path: &Path, sequences: &[(u32, u64)]) {
    let recs: Vec<_> = sequences
        .iter()
        .map(|&(seq, ts)| mbo_live(seq, ts))
        .collect();
    write_dbn_records(path, &recs);
}

fn write_cfg(out: PathBuf, inputs: Vec<PathBuf>) -> WriteConfig {
    WriteConfig {
        output: out,
        inputs,
        instrument_id: INSTRUMENT_ID,
        symbol: Some("TEST".into()),
        trade_date: date(2026, 7, 29),
        min_price_increment: fft_core::Price(ES_HELP_TICK),
        unit_of_measure_qty: ES_HELP_UOM_QTY,
        display_factor: ES_HELP_DISPLAY_FACTOR,
        batch_size: 64,
    }
}

fn read_events(path: &Path) -> Vec<fft_core::CanonicalEvent> {
    let (reader, _) = LogReader::open(path).expect("open log");
    reader
        .events(0..reader.frame_count())
        .collect::<Result<_, _>>()
        .expect("decode events")
}

fn snap_flag(ev: &fft_core::CanonicalEvent) -> bool {
    ev.flags & u16::from(dbn::flags::SNAPSHOT) != 0
}

#[test]
fn boundary_seq_discontinuity_emits_exactly_one_gap() {
    let a = temp_path("a-jump.dbn.zst");
    let b = temp_path("b-jump.dbn.zst");
    let out = temp_path("jump.fftlog");
    let _ = std::fs::remove_file(&out);

    // File A: 100, 101. File B: 200 — expected next is 102, observed 200.
    write_dbn(&a, &[(100, WED_SESSION_TS), (101, WED_SESSION_TS + 1)]);
    write_dbn(&b, &[(200, WED_SESSION_TS + 2)]);

    let stats = write_fftlog(&write_cfg(out.clone(), vec![a.clone(), b.clone()])).expect("write");
    assert_eq!(stats.gaps_kept, 1, "boundary jump must synthesize one Gap");
    assert_eq!(stats.events_written, 4); // 3 Adds + 1 Gap

    let events = read_events(&out);
    let gaps: Vec<_> = events.iter().filter(|e| e.kind == EventKind::Gap).collect();
    assert_eq!(gaps.len(), 1);
    assert_eq!(gaps[0].gap_seqs(), (102, 200));
    // Gap is stamped with the observing record's ts.
    assert_eq!(gaps[0].ts, Ts(WED_SESSION_TS + 2));

    let _ = std::fs::remove_file(&a);
    let _ = std::fs::remove_file(&b);
    let _ = std::fs::remove_file(&out);
}

#[test]
fn contiguous_inputs_emit_zero_gaps() {
    let a = temp_path("a-cont.dbn.zst");
    let b = temp_path("b-cont.dbn.zst");
    let out = temp_path("cont.fftlog");
    let _ = std::fs::remove_file(&out);

    write_dbn(&a, &[(100, WED_SESSION_TS), (101, WED_SESSION_TS + 1)]);
    write_dbn(&b, &[(102, WED_SESSION_TS + 2), (102, WED_SESSION_TS + 3)]); // same packet ok

    let stats = write_fftlog(&write_cfg(out.clone(), vec![a.clone(), b.clone()])).expect("write");
    assert_eq!(stats.gaps_kept, 0);
    assert_eq!(stats.events_written, 4);

    let events = read_events(&out);
    assert!(events.iter().all(|e| e.kind != EventKind::Gap));

    let _ = std::fs::remove_file(&a);
    let _ = std::fs::remove_file(&b);
    let _ = std::fs::remove_file(&out);
}

/// Gap ts is the observing record's ts; the shared bucketer assigns trade dates the
/// same way for Gaps as for live events. A discontinuity revealed by a post-open
/// record buckets to Wed; one revealed pre-open does not for a Wed write target.
#[test]
fn gap_buckets_by_observing_ts_across_session_open() {
    let open = session_open(date(2026, 7, 29)).0;
    assert_eq!(trade_date(Ts(open - 1)), date(2026, 7, 28));
    assert_eq!(trade_date(Ts(open)), date(2026, 7, 29));

    // Shared bucketer (same type as write_fftlog) across a synthetic two-file stitch:
    // last live on Mon, first live on Wed with a seq jump → Gap stamped at Wed ts.
    let mut bucketer = TradeDateBucketer::default();
    let mon_ts = Ts(open - 1);
    let wed_ts = Ts(open + 1);
    assert_eq!(bucketer.bucket(mon_ts), date(2026, 7, 28));
    assert_eq!(bucketer.bucket(wed_ts), date(2026, 7, 29));

    let a = temp_path("a-boundary.dbn.zst");
    let b = temp_path("b-boundary.dbn.zst");
    let out = temp_path("boundary.fftlog");
    let _ = std::fs::remove_file(&out);

    write_dbn(&a, &[(50, open - 1)]);
    write_dbn(&b, &[(60, open + 1)]); // jump 50 → 60, expected 51

    // Target Wed: Mon live event dropped; Gap (ts=open+1) and Wed Add kept.
    let stats = write_fftlog(&write_cfg(out.clone(), vec![a.clone(), b.clone()])).expect("write");
    assert_eq!(stats.gaps_kept, 1);
    assert_eq!(stats.events_written, 2); // Gap + Add

    let events = read_events(&out);
    assert_eq!(events.len(), 2);
    assert_eq!(events[0].kind, EventKind::Gap);
    assert_eq!(events[0].gap_seqs(), (51, 60));
    assert_eq!(events[0].ts, Ts(open + 1));
    assert_eq!(events[1].kind, EventKind::Add);
    assert_eq!(events[1].seq.0, 60);

    let _ = std::fs::remove_file(&a);
    let _ = std::fs::remove_file(&b);
    let _ = std::fs::remove_file(&out);
}

/// §4 snapshot admission: keep a file's SNAPSHOT block iff its first non-snapshot
/// event buckets to the target trade date. Stale prior-day block is dropped; the
/// admitted block is kept; boundary gap from live seq continuity still fires; snapshot
/// seqs never synthesize phantom Gaps.
#[test]
fn snapshot_admission_keeps_target_day_block_drops_stale() {
    let open = session_open(date(2026, 7, 29)).0;
    // Stale snap ts (well before open) and live ts for Mon / Wed.
    let stale_snap_ts = open - 86_400_000_000_000; // ~1 day before open
    let mon_live_ts = open - 1;
    let wed_live_ts = open + 1_000_000_000;

    let a = temp_path("a-snap.dbn.zst");
    let b = temp_path("b-snap.dbn.zst");
    let out = temp_path("snap-admit.fftlog");
    let _ = std::fs::remove_file(&out);

    // File A (stale day): 2 snapshot records with wild non-channel seqs, then Mon live.
    write_dbn_records(
        &a,
        &[
            mbo_snap(50_000, stale_snap_ts),
            mbo_snap(1, stale_snap_ts + 1), // discontinuous snap seq — must not gap
            mbo_live(100, mon_live_ts),
        ],
    );
    // File B (target day): 3 snapshots + Wed live with a real channel jump from 100 → 200.
    write_dbn_records(
        &b,
        &[
            mbo_snap(99_999, stale_snap_ts),
            mbo_snap(7, stale_snap_ts + 2),
            mbo_snap(7, stale_snap_ts + 3),
            mbo_live(200, wed_live_ts),
        ],
    );

    let stats = write_fftlog(&write_cfg(out.clone(), vec![a.clone(), b.clone()])).expect("write");
    assert_eq!(
        stats.snapshots_dropped, 2,
        "file A snapshot block must be dropped"
    );
    assert_eq!(
        stats.snapshots_kept, 3,
        "file B snapshot block must be admitted"
    );
    assert_eq!(
        stats.gaps_kept, 1,
        "live boundary discontinuity still emits one Gap"
    );
    // 3 snaps (B) + 1 Gap + 1 Wed live (Mon live dropped by trade-date filter).
    assert_eq!(stats.events_written, 5);

    let events = read_events(&out);
    let snaps: Vec<_> = events.iter().filter(|e| snap_flag(e)).collect();
    assert_eq!(snaps.len(), 3);
    assert!(snaps.iter().all(|e| e.seq.0 == 99_999 || e.seq.0 == 7));

    let gaps: Vec<_> = events.iter().filter(|e| e.kind == EventKind::Gap).collect();
    assert_eq!(gaps.len(), 1);
    assert_eq!(
        gaps[0].gap_seqs(),
        (101, 200),
        "gap must be live-channel 100→200, not a phantom from snapshot seqs"
    );

    // No extra gaps from snapshot seq discontinuities.
    assert_eq!(
        events.iter().filter(|e| e.kind == EventKind::Gap).count(),
        1
    );

    let _ = std::fs::remove_file(&a);
    let _ = std::fs::remove_file(&b);
    let _ = std::fs::remove_file(&out);
}

/// Snapshot records bypass GapDetector entirely: wild snap seqs between continuous
/// live channel seqs produce zero Gaps.
#[test]
fn snapshot_seqs_never_synthesize_gaps() {
    let a = temp_path("snap-gap-a.dbn.zst");
    let b = temp_path("snap-gap-b.dbn.zst");
    let out = temp_path("snap-gap.fftlog");
    let _ = std::fs::remove_file(&out);

    write_dbn_records(
        &a,
        &[
            mbo_snap(999_999, WED_SESSION_TS),
            mbo_live(10, WED_SESSION_TS + 1),
            mbo_live(11, WED_SESSION_TS + 2),
        ],
    );
    write_dbn_records(
        &b,
        &[
            mbo_snap(1, WED_SESSION_TS + 3),
            mbo_snap(500_000, WED_SESSION_TS + 4),
            mbo_live(12, WED_SESSION_TS + 5),
            mbo_live(12, WED_SESSION_TS + 6), // same packet
        ],
    );

    let stats = write_fftlog(&write_cfg(out.clone(), vec![a.clone(), b.clone()])).expect("write");
    assert_eq!(
        stats.gaps_kept, 0,
        "snapshot seqs must not feed the gap detector"
    );
    assert_eq!(stats.snapshots_kept, 3);
    assert_eq!(stats.snapshots_dropped, 0);
    // 3 snaps + 4 live
    assert_eq!(stats.events_written, 7);

    let events = read_events(&out);
    assert!(events.iter().all(|e| e.kind != EventKind::Gap));

    let _ = std::fs::remove_file(&a);
    let _ = std::fs::remove_file(&b);
    let _ = std::fs::remove_file(&out);
}
