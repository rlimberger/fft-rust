//! §7 concurrent LIVE tail: a reader walks only committed frames of a growing file,
//! retries a torn partial final frame without failing, and after clean close sees the
//! full footer index via [`LogReader::refresh`].

mod common;

use std::io::Write;

use common::{es_meta, mono_events, temp_path};
use fft_log::{IndexSource, LogReader, LogWriter};

#[test]
fn refresh_sees_exactly_committed_frames_while_writer_appends() {
    let tmp = temp_path("live-tail-append");
    let b0 = mono_events(20, 1_000, 1);
    let b1 = mono_events(20, 50_000, 21);
    let b2 = mono_events(20, 100_000, 41);

    let mut w = LogWriter::create(tmp.path(), &es_meta()).unwrap();
    w.append_events(&b0).unwrap();

    let (mut reader, report) = LogReader::open(tmp.path()).unwrap();
    assert!(reader.is_live());
    assert_eq!(report.index_source, IndexSource::LiveRecovery);
    assert_eq!(reader.frame_count(), 1);
    let events0: Vec<_> = reader.events(0..1).collect::<Result<_, _>>().unwrap();
    assert_eq!(events0, b0);

    w.append_events(&b1).unwrap();
    let r = reader.refresh().unwrap();
    assert!(r.live);
    assert_eq!(r.new_frames, 1);
    assert_eq!(reader.frame_count(), 2);

    w.append_events(&b2).unwrap();
    let r = reader.refresh().unwrap();
    assert!(r.live);
    assert_eq!(r.new_frames, 1);
    assert_eq!(reader.frame_count(), 3);

    let events: Vec<_> = reader
        .events(0..reader.frame_count())
        .collect::<Result<_, _>>()
        .unwrap();
    let expected: Vec<_> = [b0, b1, b2].into_iter().flatten().collect();
    assert_eq!(events, expected);

    // No new frames yet → refresh is a no-op discovery.
    let r = reader.refresh().unwrap();
    assert!(r.live);
    assert_eq!(r.new_frames, 0);
    assert_eq!(reader.frame_count(), 3);

    // Keep the writer alive so the file stays LIVE through the assertions above.
    drop(w);
}

#[test]
fn torn_partial_final_frame_is_retried_not_fatal_while_live() {
    let tmp = temp_path("live-tail-torn");
    let b0 = mono_events(20, 1_000, 1);
    let b1 = mono_events(20, 50_000, 21);

    let mut w = LogWriter::create(tmp.path(), &es_meta()).unwrap();
    w.append_events(&b0).unwrap();
    w.append_events(&b1).unwrap();
    drop(w); // LIVE, two committed frames, no footer

    let (mut reader, _) = LogReader::open(tmp.path()).unwrap();
    assert!(reader.is_live());
    assert_eq!(reader.frame_count(), 2);
    let frames_end_before = {
        // Force a known committed end via a no-op refresh.
        let r = reader.refresh().unwrap();
        assert_eq!(r.new_frames, 0);
        reader.frame_count()
    };

    // Simulate a torn in-progress frame: append incomplete non-validating bytes.
    {
        let mut f = std::fs::OpenOptions::new()
            .append(true)
            .open(tmp.path())
            .unwrap();
        f.write_all(&[0xAAu8; 17]).unwrap(); // shorter than a frame header
        f.flush().unwrap();
    }

    let r = reader.refresh().unwrap();
    assert!(r.live, "torn tail while LIVE must not clear LIVE");
    assert_eq!(r.new_frames, 0, "partial frame is not committed");
    assert_eq!(
        reader.frame_count(),
        frames_end_before,
        "committed set must be unchanged"
    );
    // Existing frames still decode.
    let events: Vec<_> = reader.events(0..2).collect::<Result<_, _>>().unwrap();
    let expected: Vec<_> = [b0.clone(), b1.clone()].into_iter().flatten().collect();
    assert_eq!(events, expected);

    // Second refresh still non-fatal (retry).
    let r = reader.refresh().unwrap();
    assert!(r.live);
    assert_eq!(r.new_frames, 0);
    assert_eq!(reader.frame_count(), 2);
}

#[test]
fn clean_close_refresh_sees_full_footer_index() {
    let tmp = temp_path("live-tail-close");
    let batches = [
        mono_events(15, 1_000, 1),
        mono_events(15, 40_000, 16),
        mono_events(15, 80_000, 31),
    ];

    let mut w = LogWriter::create(tmp.path(), &es_meta()).unwrap();
    w.append_events(&batches[0]).unwrap();

    let (mut reader, _) = LogReader::open(tmp.path()).unwrap();
    assert!(reader.is_live());
    assert_eq!(reader.frame_count(), 1);

    w.append_events(&batches[1]).unwrap();
    w.append_events(&batches[2]).unwrap();
    let r = reader.refresh().unwrap();
    assert!(r.live);
    assert_eq!(r.new_frames, 2);
    assert_eq!(reader.frame_count(), 3);

    w.close().unwrap();

    let r = reader.refresh().unwrap();
    assert!(!r.live, "LIVE must be cleared after clean close");
    assert_eq!(r.new_frames, 0);
    assert!(!reader.is_live());
    // was_live is sticky to open time; is_live tracks the current header flag.
    assert!(
        reader.was_live(),
        "was_live must remain true after clean-close refresh"
    );
    assert_eq!(reader.frame_count(), 3);

    // Re-open path would report Footer; refresh on the live handle must match.
    let (closed, report) = LogReader::open(tmp.path()).unwrap();
    assert_eq!(report.index_source, IndexSource::Footer);
    assert_eq!(closed.index(), reader.index());
    assert_eq!(closed.frame_count(), 3);

    let events: Vec<_> = reader
        .events(0..reader.frame_count())
        .collect::<Result<_, _>>()
        .unwrap();
    let expected: Vec<_> = batches.into_iter().flatten().collect();
    assert_eq!(events, expected);
}

/// `was_live()` is the open-time LIVE flag; `is_live()` tracks the current header.
/// After opening a LIVE file and refreshing past a clean close they diverge.
#[test]
fn was_live_sticky_is_live_tracks_refresh() {
    let tmp = temp_path("live-was-sticky");
    let batch = mono_events(10, 1_000, 1);

    let mut w = LogWriter::create(tmp.path(), &es_meta()).unwrap();
    w.append_events(&batch).unwrap();

    let (mut reader, report) = LogReader::open(tmp.path()).unwrap();
    assert_eq!(report.index_source, IndexSource::LiveRecovery);
    assert!(reader.was_live(), "opened while LIVE");
    assert!(reader.is_live(), "still LIVE before close");

    w.close().unwrap();
    let r = reader.refresh().unwrap();
    assert!(!r.live);
    assert!(
        reader.was_live(),
        "was_live stays true — open-time observation"
    );
    assert!(
        !reader.is_live(),
        "is_live follows the cleared header after refresh"
    );
    assert_eq!(reader.frame_count(), 1);

    // A fresh open of the closed file sees was_live == false.
    let (closed, _) = LogReader::open(tmp.path()).unwrap();
    assert!(!closed.was_live());
    assert!(!closed.is_live());
}
