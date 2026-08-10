//! Deterministic small fixtures under `fixtures/fft-log/` (docs/FIXTURES.md: ≤ 1 MiB
//! each, resolved from `CARGO_MANIFEST_DIR`). This test (re)generates them from fixed
//! inputs and verifies the recovery/corruption behaviour each one exists to pin down,
//! so other crates and CI can rely on the files.

mod common;

use std::path::{Path, PathBuf};

use common::{es_meta, mono_events};
use fft_core::CanonicalEvent;
use fft_log::{
    FRAME_HEADER_LEN, INDEX_ENTRY_LEN, IndexSource, LogError, LogReader, LogWriter, SECTION_BOOK,
    SECTION_SESSION, SectionRef, TRAILER_LEN,
};

fn fixtures_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/fft-log")
}

fn batches() -> [Vec<CanonicalEvent>; 3] {
    [
        mono_events(60, 1_000_000, 1),
        mono_events(60, 2_000_000, 61),
        mono_events(60, 3_000_000, 121),
    ]
}

/// Base log: three EVENTS frames with a checkpoint between frames 1 and 2, closed.
fn write_base(path: &Path) -> Vec<u8> {
    let _ = std::fs::remove_file(path);
    let [b0, b1, b2] = batches();
    let mut w = LogWriter::create(path, &es_meta()).unwrap();
    w.append_events(&b0).unwrap();
    w.append_events(&b1).unwrap();
    w.write_checkpoint([
        SectionRef {
            id: SECTION_BOOK,
            version: 1,
            flags: 0,
            bytes: &[0xB0; 256],
        },
        SectionRef {
            id: SECTION_SESSION,
            version: 1,
            flags: 0,
            bytes: &[0x5E; 32],
        },
    ])
    .unwrap();
    w.append_events(&b2).unwrap();
    w.close().unwrap();
    std::fs::read(path).unwrap()
}

/// Same frames but the writer "crashes" (no close): LIVE stays set, no footer.
fn write_live_base(path: &Path) -> Vec<u8> {
    let _ = std::fs::remove_file(path);
    let [b0, b1, b2] = batches();
    let mut w = LogWriter::create(path, &es_meta()).unwrap();
    for b in [&b0, &b1, &b2] {
        w.append_events(b).unwrap();
    }
    drop(w);
    std::fs::read(path).unwrap()
}

#[test]
fn generate_and_verify_fixtures() {
    let dir = fixtures_dir();
    std::fs::create_dir_all(&dir).unwrap();

    // 1. clean_small.fftlog — cleanly closed log with a checkpoint.
    let clean = dir.join("clean_small.fftlog");
    write_base(&clean);
    let (reader, report) = LogReader::open(&clean).unwrap();
    assert_eq!(report.index_source, IndexSource::Footer);
    assert_eq!(reader.frame_count(), 4);
    assert_eq!(reader.read_checkpoint(2).unwrap().len(), 2);
    let events: Vec<_> = reader
        .events(0..reader.frame_count())
        .collect::<Result<_, _>>()
        .unwrap();
    assert_eq!(events, batches().into_iter().flatten().collect::<Vec<_>>());
    drop(reader);

    // 2. torn_tail.fftlog — LIVE log truncated mid-way into its final frame.
    let torn = dir.join("torn_tail.fftlog");
    let live_bytes = write_live_base(&torn);
    let (reader, _) = LogReader::open(&torn).unwrap();
    let final_offset = reader.index()[2].offset;
    drop(reader);
    let cut = final_offset as usize + FRAME_HEADER_LEN + 7; // inside the final payload
    std::fs::write(&torn, &live_bytes[..cut]).unwrap();
    let (reader, report) = LogReader::open(&torn).unwrap();
    let recovery = report.recovery.expect("torn fixture must trigger recovery");
    assert_eq!(recovery.dropped_bytes, cut as u64 - final_offset);
    assert_eq!(reader.frame_count(), 2);
    drop(reader);

    // 3. corrupt_frame.fftlog — closed log with one payload byte flipped in frame 0.
    let corrupt_frame = dir.join("corrupt_frame.fftlog");
    let mut bytes = write_base(&corrupt_frame);
    let (reader, _) = LogReader::open(&corrupt_frame).unwrap();
    let flip = reader.index()[0].offset as usize + FRAME_HEADER_LEN + 5;
    drop(reader);
    bytes[flip] ^= 0xff;
    std::fs::write(&corrupt_frame, &bytes).unwrap();
    let (reader, _) = LogReader::open(&corrupt_frame).unwrap();
    let err = reader.events(0..1).next().unwrap().unwrap_err();
    assert!(matches!(err, LogError::PayloadChecksum { .. }));
    drop(reader);

    // 4. corrupt_footer.fftlog — closed log with one index byte flipped; chain intact.
    let corrupt_footer = dir.join("corrupt_footer.fftlog");
    let mut bytes = write_base(&corrupt_footer);
    let index_start = bytes.len() - TRAILER_LEN - 4 * INDEX_ENTRY_LEN;
    bytes[index_start + 1] ^= 0xff;
    std::fs::write(&corrupt_footer, &bytes).unwrap();
    let (reader, report) = LogReader::open(&corrupt_footer).unwrap();
    assert_eq!(report.index_source, IndexSource::RebuiltCorruptIndex);
    assert!(!report.warnings.is_empty());
    assert_eq!(reader.frame_count(), 4);
    drop(reader);

    // Fixture policy: every file ≤ 1 MiB.
    for name in [
        "clean_small.fftlog",
        "torn_tail.fftlog",
        "corrupt_frame.fftlog",
        "corrupt_footer.fftlog",
    ] {
        let len = std::fs::metadata(dir.join(name)).unwrap().len();
        assert!(
            len <= 1024 * 1024,
            "{name} is {len} bytes, over the 1 MiB policy cap"
        );
    }
}
