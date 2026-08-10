//! Seek (checkpoint restore + tail) must match forward replay order-exact.

mod common;

use common::*;
use fft_book::Book;
use fft_core::Side;
use fft_profile::MultiProfile;
use fft_replay::ReplaySource;

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
        assert_eq!(seek_book.serialize(), forward_book.serialize());
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
