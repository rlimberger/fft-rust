//! §6 footer/index: the footer index equals a rebuilt-by-scan index; index corruption
//! over an intact frame chain rebuilds with a visible warning; damage that makes the
//! chain unprovable is loud.

mod common;

use common::{mono_events, temp_path};
use fft_log::{INDEX_ENTRY_LEN, IndexSource, LogError, LogReader, TRAILER_LEN};

fn three_frame_closed() -> (common::TempPath, Vec<u8>) {
    let tmp = temp_path("footer-src");
    let bytes = common::write_closed(
        tmp.path(),
        &[
            mono_events(30, 1_000, 1),
            mono_events(30, 50_000, 31),
            mono_events(30, 90_000, 61),
        ],
    );
    (tmp, bytes)
}

#[test]
fn footer_index_equals_rebuilt_index() {
    let (tmp, bytes) = three_frame_closed();
    let (reader, report) = LogReader::open(tmp.path()).unwrap();
    assert_eq!(report.index_source, IndexSource::Footer);
    let footer_index = reader.index().to_vec();
    drop(reader);

    // Strip the footer: the reader must rebuild the identical index by walking frame
    // headers, and say so.
    let footer_len = footer_index.len() * INDEX_ENTRY_LEN + TRAILER_LEN;
    let case = temp_path("footer-stripped");
    std::fs::write(case.path(), &bytes[..bytes.len() - footer_len]).unwrap();
    let (reader, report) = LogReader::open(case.path()).unwrap();
    assert_eq!(report.index_source, IndexSource::RebuiltMissingFooter);
    assert!(!report.warnings.is_empty());
    assert_eq!(reader.index(), footer_index.as_slice());
}

#[test]
fn corrupt_index_over_intact_chain_rebuilds_with_warning() {
    let (tmp, bytes) = three_frame_closed();
    let (reader, _) = LogReader::open(tmp.path()).unwrap();
    let footer_index = reader.index().to_vec();
    drop(reader);

    // Flip a byte inside the index entries (trailer left intact).
    let index_start = bytes.len() - TRAILER_LEN - footer_index.len() * INDEX_ENTRY_LEN;
    let mut corrupt = bytes;
    corrupt[index_start + 2] ^= 0xff;
    let case = temp_path("footer-corrupt-index");
    std::fs::write(case.path(), &corrupt).unwrap();

    let (reader, report) = LogReader::open(case.path()).unwrap();
    assert_eq!(report.index_source, IndexSource::RebuiltCorruptIndex);
    assert!(
        !report.warnings.is_empty(),
        "rebuild must surface a visible warning"
    );
    assert_eq!(reader.index(), footer_index.as_slice());
    // The rebuilt index serves data as usual.
    let events: Vec<_> = reader
        .events(0..reader.frame_count())
        .collect::<Result<_, _>>()
        .unwrap();
    assert_eq!(events.len(), 90);
}

#[test]
fn unprovable_footer_damage_is_loud() {
    let (_tmp, bytes) = three_frame_closed();

    // Corrupt the trailer's index_len so the frame region cannot be delimited.
    let mut corrupt = bytes.clone();
    let index_len_at = corrupt.len() - TRAILER_LEN;
    corrupt[index_len_at] ^= 0xff;
    let case = temp_path("footer-badlen");
    std::fs::write(case.path(), &corrupt).unwrap();
    let err = LogReader::open(case.path()).unwrap_err();
    assert!(
        matches!(
            err,
            LogError::CorruptIndex { .. } | LogError::CorruptTail { .. }
        ),
        "got {err}"
    );

    // Corrupt the trailer magic on a closed file: no footer, and the leftover index
    // bytes cannot validate as frames — loud, never a silent partial read.
    let mut corrupt = bytes;
    let magic_at = corrupt.len() - 1;
    corrupt[magic_at] ^= 0xff;
    let case = temp_path("footer-badmagic");
    std::fs::write(case.path(), &corrupt).unwrap();
    let err = LogReader::open(case.path()).unwrap_err();
    assert!(matches!(err, LogError::CorruptTail { .. }), "got {err}");
}
