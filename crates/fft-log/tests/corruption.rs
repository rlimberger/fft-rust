//! §7 commit rule under corruption: a flipped byte in a committed frame of a CLOSED
//! file is a loud error; the same flip in the tail of a LIVE file is an uncommitted
//! tail, recovered on open.

mod common;

use common::{mono_events, temp_path};
use fft_log::{FRAME_HEADER_LEN, IndexSource, LogError, LogReader};

/// Byte range of frame `index`'s compressed payload, via the pristine reader.
fn payload_byte(reader: &LogReader, index: usize) -> u64 {
    let offset = reader.index()[index].offset;
    offset + FRAME_HEADER_LEN as u64 + 3 // an arbitrary byte inside the payload
}

#[test]
fn closed_file_payload_flip_is_loud() {
    let tmp = temp_path("closed-flip-src");
    let bytes = common::write_closed(
        tmp.path(),
        &[mono_events(40, 1_000, 1), mono_events(40, 2_000, 41)],
    );
    let (reader, _) = LogReader::open(tmp.path()).unwrap();
    let flip_at = payload_byte(&reader, 0);
    drop(reader);

    let mut corrupt = bytes.clone();
    corrupt[flip_at as usize] ^= 0xff;
    let case = temp_path("closed-flip");
    std::fs::write(case.path(), &corrupt).unwrap();

    // The footer is intact, so open succeeds; touching the frame is loud.
    let (reader, report) = LogReader::open(case.path()).unwrap();
    assert_eq!(report.index_source, IndexSource::Footer);
    let err = reader
        .events(0..reader.frame_count())
        .next()
        .unwrap()
        .unwrap_err();
    assert!(matches!(err, LogError::PayloadChecksum { .. }), "got {err}");

    // With the footer stripped the same flip is loud at open: a closed file's
    // non-validating bytes are corruption, never a skippable tail.
    let (reader, _) = LogReader::open(tmp.path()).unwrap();
    let frames_end = {
        let h = reader.frame_header(1).unwrap();
        reader.index()[1].offset + FRAME_HEADER_LEN as u64 + u64::from(h.compressed_len)
    };
    drop(reader);
    let case2 = temp_path("closed-flip-nofooter");
    std::fs::write(case2.path(), &corrupt[..frames_end as usize]).unwrap();
    let err = LogReader::open(case2.path()).unwrap_err();
    assert!(matches!(err, LogError::CorruptTail { .. }), "got {err}");
}

#[test]
fn closed_file_frame_header_flip_is_loud() {
    let tmp = temp_path("closed-hdr-src");
    let bytes = common::write_closed(tmp.path(), &[mono_events(40, 1_000, 1)]);
    let (reader, _) = LogReader::open(tmp.path()).unwrap();
    let flip_at = reader.index()[0].offset + 20; // inside first_ts of the frame header
    drop(reader);

    let mut corrupt = bytes;
    corrupt[flip_at as usize] ^= 0x01;
    let case = temp_path("closed-hdr");
    std::fs::write(case.path(), &corrupt).unwrap();
    let (reader, _) = LogReader::open(case.path()).unwrap();
    let err = reader.frame_header(0).unwrap_err();
    assert!(
        matches!(err, LogError::FrameHeaderChecksum { .. }),
        "got {err}"
    );
}

#[test]
fn live_file_tail_flip_is_uncommitted_tail() {
    let tmp = temp_path("live-flip-src");
    let bytes = common::write_live(
        tmp.path(),
        &[mono_events(40, 1_000, 1), mono_events(40, 2_000, 41)],
    );
    let (reader, _) = LogReader::open(tmp.path()).unwrap();
    let final_offset = reader.index()[1].offset;
    let flip_at = payload_byte(&reader, 1);
    let expected = mono_events(40, 1_000, 1);
    drop(reader);

    let mut corrupt = bytes;
    corrupt[flip_at as usize] ^= 0xff;
    let case = temp_path("live-flip");
    std::fs::write(case.path(), &corrupt).unwrap();

    let (reader, report) = LogReader::open(case.path()).unwrap();
    let recovery = report.recovery.expect("LIVE open surfaces recovery");
    assert_eq!(recovery.dropped_bytes, corrupt.len() as u64 - final_offset);
    assert_eq!(reader.frame_count(), 1);
    let events: Vec<_> = reader.events(0..1).collect::<Result<_, _>>().unwrap();
    assert_eq!(events, expected);
}
