//! §8 crash recovery: a LIVE multi-frame log truncated at **every** byte offset within
//! the final frame must reopen onto the last committed frame with an exact
//! dropped-byte report — no panics, no silent wrong data.

mod common;

use common::{mono_events, temp_path};
use fft_log::{IndexSource, LogReader};

#[test]
fn every_truncation_offset_recovers_to_last_committed_frame() {
    let batches = [
        mono_events(40, 1_000, 1),
        mono_events(40, 100_000, 41),
        mono_events(40, 200_000, 81),
    ];
    let tmp = temp_path("torn-src");
    let bytes = common::write_live(tmp.path(), &batches);

    // Locate the final frame and the committed state before it via a full open.
    let (reader, report) = LogReader::open(tmp.path()).unwrap();
    assert_eq!(reader.frame_count(), 3);
    assert_eq!(report.recovery.unwrap().dropped_bytes, 0);
    let final_frame_offset = reader.index()[2].offset;
    let frame1_header = reader.frame_header(1).unwrap();
    let expected_events: Vec<_> = batches[..2].iter().flatten().copied().collect();
    drop(reader);

    for cut in final_frame_offset..bytes.len() as u64 {
        let case = temp_path("torn-cut");
        std::fs::write(case.path(), &bytes[..cut as usize]).unwrap();

        let (reader, report) = LogReader::open(case.path())
            .unwrap_or_else(|e| panic!("open failed at cut {cut}: {e}"));
        assert_eq!(report.index_source, IndexSource::LiveRecovery);
        let recovery = report.recovery.expect("LIVE open must surface recovery");
        assert_eq!(
            recovery.dropped_bytes,
            cut - final_frame_offset,
            "at cut {cut}"
        );
        assert_eq!(
            recovery.last_good,
            Some((fft_core::Ts(frame1_header.last_ts), frame1_header.last_seq)),
            "at cut {cut}"
        );
        assert_eq!(reader.frame_count(), 2, "at cut {cut}");
        let events: Vec<_> = reader
            .events(0..reader.frame_count())
            .collect::<Result<_, _>>()
            .unwrap_or_else(|e| panic!("decode failed at cut {cut}: {e}"));
        assert_eq!(events, expected_events, "at cut {cut}");
        if recovery.dropped_bytes > 0 {
            assert!(
                !report.warnings.is_empty(),
                "dropped bytes must warn at cut {cut}"
            );
        }
    }
}

#[test]
fn truncation_into_earlier_frames_still_recovers() {
    let batches = [mono_events(40, 1_000, 1), mono_events(40, 100_000, 41)];
    let tmp = temp_path("torn-deep-src");
    let bytes = common::write_live(tmp.path(), &batches);
    let (reader, _) = LogReader::open(tmp.path()).unwrap();
    let frame1_offset = reader.index()[1].offset;
    let expected = batches[0].clone();
    drop(reader);

    // A cut in the middle of frame 1 drops it entirely.
    let cut = frame1_offset + 30;
    let case = temp_path("torn-deep-cut");
    std::fs::write(case.path(), &bytes[..cut as usize]).unwrap();
    let (reader, report) = LogReader::open(case.path()).unwrap();
    assert_eq!(report.recovery.unwrap().dropped_bytes, cut - frame1_offset);
    assert_eq!(reader.frame_count(), 1);
    let events: Vec<_> = reader.events(0..1).collect::<Result<_, _>>().unwrap();
    assert_eq!(events, expected);
}

#[test]
fn live_file_with_no_committed_frames_recovers_to_empty() {
    let tmp = temp_path("torn-empty");
    let bytes = common::write_live(tmp.path(), &[mono_events(10, 1_000, 1)]);
    let (reader, _) = LogReader::open(tmp.path()).unwrap();
    let frame0_offset = reader.index()[0].offset;
    drop(reader);

    let case = temp_path("torn-empty-cut");
    std::fs::write(case.path(), &bytes[..frame0_offset as usize + 10]).unwrap();
    let (reader, report) = LogReader::open(case.path()).unwrap();
    let recovery = report.recovery.unwrap();
    assert_eq!(recovery.dropped_bytes, 10);
    assert_eq!(recovery.last_good, None);
    assert_eq!(reader.frame_count(), 0);
}
