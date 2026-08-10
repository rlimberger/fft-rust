//! Offline `fft-checkpoint` pass: event-identical copy + order-exact restore/tail.

mod common;

use common::*;
use fft_book::Book;
use fft_engine::{CHECKPOINT_EVENT_CADENCE_NS, write_checkpointed_copy};
use fft_log::{KIND_CHECKPOINT, LogReader};
use fft_profile::MultiProfile;
use fft_replay::ReplaySource;

fn collect_events(path: &std::path::Path) -> Vec<fft_core::CanonicalEvent> {
    let (reader, _) = LogReader::open(path).expect("open");
    reader
        .events(0..reader.frame_count())
        .collect::<Result<Vec<_>, _>>()
        .expect("events")
}

fn forward_all(path: &std::path::Path) -> (Book, MultiProfile, u64, u64) {
    let mut src = ReplaySource::open(path).expect("open");
    let mut book = Book::new(src.meta().min_price_increment);
    let mut profile = MultiProfile::new(src.meta().min_price_increment);
    profile.begin_session(src.meta().trade_date);
    let mut events = 0u64;
    while src
        .apply_next(&mut book, &mut profile)
        .expect("apply")
        .is_some()
    {
        events += 1;
    }
    book.check_invariants();
    (book, profile, events, src.applied_seq())
}

fn assert_state_eq(
    a_book: &Book,
    a_profile: &MultiProfile,
    b_book: &Book,
    b_profile: &MultiProfile,
) {
    assert_eq!(a_book.serialize_book(), b_book.serialize_book());
    assert_eq!(a_book.serialize_flow(), b_book.serialize_flow());
    assert_eq!(a_book.serialize_refresh(), b_book.serialize_refresh());
    assert_eq!(a_profile.serialize(), b_profile.serialize());
}

fn checkpoint_targets(path: &std::path::Path) -> Vec<u64> {
    let (reader, _) = LogReader::open(path).expect("open dst");
    reader
        .index()
        .iter()
        .enumerate()
        .filter(|(_, entry)| entry.kind == KIND_CHECKPOINT)
        .map(|(idx, _)| reader.frame_header(idx).expect("checkpoint header").last_ts)
        .collect()
}

#[test]
fn dst_forward_matches_src_event_identical_and_state() {
    let src = temp_path("ckpt-src-ident");
    let dst = temp_path("ckpt-dst-ident");
    // 5-minute span @ 1 s/event → several 60 s checkpoints.
    write_event_only_log(src.path(), 300, 1_000_000_000);

    let summary = write_checkpointed_copy(src.path(), dst.path()).expect("checkpoint pass");
    assert_eq!(summary.events, 300);
    assert!(
        summary.checkpoints >= 4,
        "expected ~5 min / 60s checkpoints"
    );
    assert_eq!(collect_events(src.path()), collect_events(dst.path()));

    let (src_book, src_profile, src_n, src_seq) = forward_all(src.path());
    let (dst_book, dst_profile, dst_n, dst_seq) = forward_all(dst.path());
    assert_eq!(src_n, dst_n);
    assert_eq!(src_seq, dst_seq);
    assert_state_eq(&src_book, &src_profile, &dst_book, &dst_profile);
}

#[test]
fn restore_plus_tail_matches_forward_order_exact_at_every_checkpoint() {
    let src = temp_path("ckpt-src-seek");
    let dst = temp_path("ckpt-dst-seek");
    // Dense enough for book activity; 2 s steps → checkpoints every 30 events.
    write_event_only_log(src.path(), 180, 2_000_000_000);

    let summary = write_checkpointed_copy(src.path(), dst.path()).expect("checkpoint pass");
    assert!(summary.checkpoints > 0);
    let targets = checkpoint_targets(dst.path());
    assert_eq!(targets.len() as u64, summary.checkpoints);

    for target in targets {
        let mut forward_src = ReplaySource::open(dst.path()).expect("open forward");
        let mut forward_book = Book::new(forward_src.meta().min_price_increment);
        let mut forward_profile = MultiProfile::new(forward_src.meta().min_price_increment);
        forward_profile.begin_session(forward_src.meta().trade_date);
        // Mirror fft-replay seek_differential: apply all events with ts <= target.
        while let Some(next) = forward_src.peek_event().expect("peek") {
            if next.ts.0 > target {
                break;
            }
            forward_src
                .apply_next(&mut forward_book, &mut forward_profile)
                .expect("apply")
                .expect("present");
        }
        forward_book.check_invariants();

        let mut seek_src = ReplaySource::open(dst.path()).expect("open seek");
        let mut seek_book = Book::new(seek_src.meta().min_price_increment);
        let mut seek_profile = MultiProfile::new(seek_src.meta().min_price_increment);
        seek_profile.begin_session(seek_src.meta().trade_date);
        let report = seek_src
            .seek(target, &mut seek_book, &mut seek_profile, || false)
            .expect("seek");
        assert!(!report.cancelled);
        assert!(
            report.checkpoint_frame.is_some(),
            "checkpointed log must restore, not replay-from-start, at {target}"
        );
        assert_eq!(seek_src.applied_seq(), forward_src.applied_seq());
        assert_eq!(seek_src.applied_ts(), forward_src.applied_ts());
        assert_state_eq(&seek_book, &seek_profile, &forward_book, &forward_profile);
    }
}

#[test]
fn zero_event_src_yields_valid_empty_dst() {
    let src = temp_path("ckpt-src-empty");
    let dst = temp_path("ckpt-dst-empty");
    write_event_only_log(src.path(), 0, CHECKPOINT_EVENT_CADENCE_NS);

    let summary = write_checkpointed_copy(src.path(), dst.path()).expect("empty pass");
    assert_eq!(summary.events, 0);
    assert_eq!(summary.checkpoints, 0);
    assert!(collect_events(dst.path()).is_empty());
    // Openable closed log with footer.
    let (reader, report) = LogReader::open(dst.path()).expect("open empty dst");
    assert!(report.warnings.is_empty());
    assert_eq!(reader.frame_count(), 0);
}

#[test]
fn short_span_under_cadence_writes_zero_checkpoints() {
    let src = temp_path("ckpt-src-short");
    let dst = temp_path("ckpt-dst-short");
    // 30 events × 1 s = 29 s span < 60 s cadence.
    write_event_only_log(src.path(), 30, 1_000_000_000);

    let summary = write_checkpointed_copy(src.path(), dst.path()).expect("short pass");
    assert_eq!(summary.events, 30);
    assert_eq!(
        summary.checkpoints, 0,
        "span < 60s must not emit checkpoints"
    );
    assert_eq!(collect_events(src.path()), collect_events(dst.path()));
    assert!(checkpoint_targets(dst.path()).is_empty());
}
