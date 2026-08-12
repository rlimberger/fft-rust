mod common;

use common::{SESSION_OPEN_NS, add, es_meta, temp_path, write_checkpointed_log};
use fft_core::{CanonicalEvent, EventKind, Side};
use fft_log::{INDEX_ENTRY_LEN, LogReader, LogWriter, TRAILER_LEN};
use fft_replay::{ReplayError, write_with_injected_gap};

fn read_events(path: &std::path::Path) -> Vec<CanonicalEvent> {
    let (reader, report) = LogReader::open(path).expect("open output");
    assert!(
        report.warnings.is_empty(),
        "output warnings: {:?}",
        report.warnings
    );
    reader
        .events(0..reader.frame_count())
        .collect::<Result<_, _>>()
        .expect("read output events")
}

fn write_events(path: &std::path::Path, events: &[CanonicalEvent]) {
    let mut writer = LogWriter::create(path, &es_meta()).expect("create source");
    writer.append_events(events).expect("append source events");
    writer.close().expect("close source");
}

#[test]
fn inserts_exactly_one_gap_at_boundary_and_preserves_every_source_event() {
    let src = temp_path("splice-source");
    let dst = temp_path("splice-output");
    write_checkpointed_log(src.path(), 12, 4);
    let original = read_events(src.path());
    let boundary = original[5].ts.0;
    let copy_through = original[9].ts.0;

    let (output_events, gap_ts) =
        write_with_injected_gap(src.path(), dst.path(), boundary, copy_through, 7, 11)
            .expect("splice");
    let output = read_events(dst.path());
    let expected_source: Vec<_> = original
        .iter()
        .copied()
        .take_while(|event| event.ts.0 <= copy_through)
        .collect();
    let gaps: Vec<_> = output
        .iter()
        .enumerate()
        .filter(|(_, event)| event.kind == EventKind::Gap)
        .collect();

    assert_eq!(output_events, expected_source.len() as u64 + 1);
    assert_eq!(gaps.len(), 1);
    assert_eq!(gaps[0].0, 6);
    assert_eq!(gaps[0].1.ts.0, original[6].ts.0);
    assert_eq!(gap_ts, original[6].ts.0);
    assert_eq!(gaps[0].1.gap_seqs(), (7, 11));

    let non_gaps: Vec<_> = output
        .into_iter()
        .filter(|event| event.kind != EventKind::Gap)
        .collect();
    assert_eq!(non_gaps, expected_source);
}

#[test]
fn no_anchor_fails_without_creating_destination() {
    let src = temp_path("no-anchor-source");
    let dst = temp_path("no-anchor-output");
    write_checkpointed_log(src.path(), 4, 4);

    let error = write_with_injected_gap(
        src.path(),
        dst.path(),
        SESSION_OPEN_NS + 10_000_000,
        SESSION_OPEN_NS + 3_000_000,
        5,
        6,
    )
    .expect_err("missing anchor must fail");

    assert!(matches!(error, ReplayError::SpliceAnchorNotFound { .. }));
    assert!(!dst.path().exists());
}

#[test]
fn malformed_gap_sequences_fail_without_creating_destination() {
    let src = temp_path("bad-gap-source");
    let dst = temp_path("bad-gap-output");
    write_checkpointed_log(src.path(), 4, 4);

    let error = write_with_injected_gap(
        src.path(),
        dst.path(),
        SESSION_OPEN_NS,
        SESSION_OPEN_NS + 3_000_000,
        9,
        9,
    )
    .expect_err("non-forward Gap must fail");

    assert!(matches!(
        error,
        ReplayError::InvalidGapSequences {
            expected: 9,
            observed: 9
        }
    ));
    assert!(!dst.path().exists());
}

#[test]
fn backwards_timestamp_fails_without_creating_destination() {
    let src = temp_path("backward-source");
    let dst = temp_path("backward-output");
    let events = [
        add(1, Side::Bid, 20_000, 1, SESSION_OPEN_NS, 1),
        add(2, Side::Ask, 20_001, 1, SESSION_OPEN_NS + 2, 2),
        add(3, Side::Bid, 19_999, 1, SESSION_OPEN_NS + 1, 3),
    ];
    write_events(src.path(), &events);

    let error = write_with_injected_gap(
        src.path(),
        dst.path(),
        SESSION_OPEN_NS,
        SESSION_OPEN_NS + 2,
        2,
        3,
    )
    .expect_err("backwards timestamp must fail");

    assert!(matches!(
        error,
        ReplayError::NonMonotonicSpliceInput { event_index: 2, .. }
    ));
    assert!(!dst.path().exists());
}

#[test]
fn source_open_warnings_are_rejected_without_creating_destination() {
    let clean = temp_path("warning-clean-source");
    let warned = temp_path("warning-source");
    let dst = temp_path("warning-output");
    write_checkpointed_log(clean.path(), 8, 8);

    let (reader, report) = LogReader::open(clean.path()).expect("open clean source");
    assert!(report.warnings.is_empty());
    let footer_len = reader.frame_count() * INDEX_ENTRY_LEN + TRAILER_LEN;
    drop(reader);
    let bytes = std::fs::read(clean.path()).expect("read clean source");
    std::fs::write(warned.path(), &bytes[..bytes.len() - footer_len])
        .expect("write footerless source");

    let error = write_with_injected_gap(
        warned.path(),
        dst.path(),
        SESSION_OPEN_NS + 2_000_000,
        SESSION_OPEN_NS + 7_000_000,
        4,
        5,
    )
    .expect_err("open warning must fail");

    match error {
        ReplayError::SpliceOpenWarnings(warnings) => assert!(!warnings.is_empty()),
        other => panic!("unexpected error: {other}"),
    }
    assert!(!dst.path().exists());
}
