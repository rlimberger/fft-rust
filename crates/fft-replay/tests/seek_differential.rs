//! Seek (checkpoint restore + tail) must match forward replay order-exact.

mod common;

use common::*;
use fft_book::{
    BOOK_SECTION_ID, BOOK_SECTION_VERSION, Book, FLOW_SECTION_ID, FLOW_SECTION_VERSION,
    REFRESH_SECTION_ID, REFRESH_SECTION_VERSION, RestoreError,
};
use fft_core::Side;
use fft_log::SectionRef;
use fft_profile::{
    CVD_SECTION_ID, CVD_SECTION_VERSION, MultiProfile, PROFILE_SECTION_ID, PROFILE_SECTION_VERSION,
    SESSION_SECTION_ID, SESSION_SECTION_VERSION,
};
use fft_replay::{ReplayError, ReplaySource};

#[test]
fn seek_matches_forward_at_multiple_targets() {
    let tmp = temp_path("seek-diff");
    write_checkpointed_log(tmp.path(), 400, 100);

    let targets = [
        SESSION_OPEN_NS,
        SESSION_OPEN_NS + 50 * 1_000_000,
        SESSION_OPEN_NS + 150 * 1_000_000,
        SESSION_OPEN_NS + 275 * 1_000_000,
        SESSION_OPEN_NS + 399 * 1_000_000,
    ];

    for target in targets {
        let mut forward_src = ReplaySource::open(tmp.path()).expect("open forward");
        let mut forward_book = Book::new(forward_src.meta().min_price_increment);
        let mut forward_profile = MultiProfile::new(forward_src.meta().min_price_increment);
        forward_profile.begin_session(forward_src.meta().trade_date);
        forward_to(
            &mut forward_src,
            &mut forward_book,
            &mut forward_profile,
            target,
        );

        let mut seek_src = ReplaySource::open(tmp.path()).expect("open seek");
        let mut seek_book = Book::new(seek_src.meta().min_price_increment);
        let mut seek_profile = MultiProfile::new(seek_src.meta().min_price_increment);
        seek_profile.begin_session(seek_src.meta().trade_date);
        let report = seek_src
            .seek(target, &mut seek_book, &mut seek_profile, || false)
            .expect("seek");
        assert!(!report.cancelled);
        assert_eq!(report.target_ts, target);
        assert_eq!(seek_book.serialize_book(), forward_book.serialize_book());
        assert_eq!(seek_book.serialize_flow(), forward_book.serialize_flow());
        assert_eq!(
            seek_book.serialize_refresh(),
            forward_book.serialize_refresh()
        );
        assert_eq!(seek_profile.serialize(), forward_profile.serialize());
        assert_eq!(seek_src.applied_seq(), forward_src.applied_seq());
        assert_eq!(seek_src.applied_ts(), forward_src.applied_ts());
    }
}

#[test]
fn seek_without_preceding_checkpoint_replays_from_start() {
    let tmp = temp_path("seek-from-start");
    let meta = es_meta();
    let mut writer = fft_log::LogWriter::create(tmp.path(), &meta).unwrap();
    let events: Vec<_> = (0..40)
        .map(|i| {
            let side = if i % 2 == 0 { Side::Bid } else { Side::Ask };
            let ticks = if side == Side::Bid {
                20_000 - 1 - (i % 10)
            } else {
                20_000 + 1 + (i % 10)
            };
            add(
                (i + 1) as u64,
                side,
                ticks,
                3,
                SESSION_OPEN_NS + i as u64 * 1_000,
                (i + 1) as u32,
            )
        })
        .collect();
    writer.append_events(&events).unwrap();
    writer.close().unwrap();

    let mut src = ReplaySource::open(tmp.path()).unwrap();
    let mut book = Book::new(src.meta().min_price_increment);
    let mut profile = MultiProfile::new(src.meta().min_price_increment);
    profile.begin_session(src.meta().trade_date);
    let report = src
        .seek(SESSION_OPEN_NS + 20_000, &mut book, &mut profile, || false)
        .unwrap();
    assert!(report.replayed_from_start);
    assert!(report.checkpoint_frame.is_none());
    assert!(!report.cancelled);
    assert!(report.tail_events > 0);
}

#[test]
fn seek_cancel_leaves_no_success_report() {
    let tmp = temp_path("seek-cancel");
    write_checkpointed_log(tmp.path(), 2_000, 500);

    let mut src = ReplaySource::open(tmp.path()).unwrap();
    let mut book = Book::new(src.meta().min_price_increment);
    let mut profile = MultiProfile::new(src.meta().min_price_increment);
    profile.begin_session(src.meta().trade_date);
    let mut polls = 0u32;
    let report = src
        .seek(
            SESSION_OPEN_NS + 1_999 * 1_000_000,
            &mut book,
            &mut profile,
            || {
                polls += 1;
                polls >= 2
            },
        )
        .unwrap();
    assert!(report.cancelled);
}

fn write_single_checkpoint(path: &std::path::Path, omit: Option<u16>, bad_book_inner: bool) {
    let meta = es_meta();
    let event = add(1, Side::Bid, 19_999, 3, SESSION_OPEN_NS, 1);
    let mut book = Book::new(meta.min_price_increment);
    book.apply(&event);
    let mut profile = MultiProfile::new(meta.min_price_increment);
    profile.begin_session(meta.trade_date);
    profile.apply(&event);

    let mut book_bytes = book.serialize_book();
    if bad_book_inner {
        book_bytes[..2].copy_from_slice(&99u16.to_le_bytes());
    }
    let flow_bytes = book.serialize_flow();
    let refresh_bytes = book.serialize_refresh();
    let secs = profile.serialize();
    let mut sections = vec![
        SectionRef {
            id: BOOK_SECTION_ID,
            version: BOOK_SECTION_VERSION,
            flags: 0,
            bytes: &book_bytes,
        },
        SectionRef {
            id: FLOW_SECTION_ID,
            version: FLOW_SECTION_VERSION,
            flags: 0,
            bytes: &flow_bytes,
        },
        SectionRef {
            id: PROFILE_SECTION_ID,
            version: PROFILE_SECTION_VERSION,
            flags: 0,
            bytes: &secs.profile,
        },
        SectionRef {
            id: CVD_SECTION_ID,
            version: CVD_SECTION_VERSION,
            flags: 0,
            bytes: &secs.cvd,
        },
        SectionRef {
            id: REFRESH_SECTION_ID,
            version: REFRESH_SECTION_VERSION,
            flags: 0,
            bytes: &refresh_bytes,
        },
        SectionRef {
            id: SESSION_SECTION_ID,
            version: SESSION_SECTION_VERSION,
            flags: 0,
            bytes: &secs.session,
        },
    ];
    sections.retain(|section| Some(section.id) != omit);
    let mut writer = fft_log::LogWriter::create(path, &meta).unwrap();
    writer.append_events(&[event]).unwrap();
    writer.write_checkpoint(sections).unwrap();
    writer.close().unwrap();
}

#[test]
fn seek_requires_flow_and_refresh_sections() {
    for (id, expected) in [
        (FLOW_SECTION_ID, "FLOW"),
        (REFRESH_SECTION_ID, "REFRESH"),
        (SESSION_SECTION_ID, "SESSION"),
    ] {
        let tmp = temp_path(expected);
        write_single_checkpoint(tmp.path(), Some(id), false);
        let mut source = ReplaySource::open(tmp.path()).unwrap();
        let mut book = Book::new(source.meta().min_price_increment);
        let mut profile = MultiProfile::new(source.meta().min_price_increment);
        profile.begin_session(source.meta().trade_date);
        let error = source
            .seek(SESSION_OPEN_NS, &mut book, &mut profile, || false)
            .unwrap_err();
        assert!(matches!(error, ReplayError::MissingSection(section) if section == expected));
    }
}

#[test]
fn seek_maps_typed_book_restore_errors() {
    let tmp = temp_path("bad-book-inner-version");
    write_single_checkpoint(tmp.path(), None, true);
    let mut source = ReplaySource::open(tmp.path()).unwrap();
    let mut book = Book::new(source.meta().min_price_increment);
    let mut profile = MultiProfile::new(source.meta().min_price_increment);
    profile.begin_session(source.meta().trade_date);
    let error = source
        .seek(SESSION_OPEN_NS, &mut book, &mut profile, || false)
        .unwrap_err();
    assert!(matches!(
        error,
        ReplayError::BookRestore(RestoreError::UnsupportedVersion {
            section: "BOOK",
            version: 99,
        })
    ));
}
