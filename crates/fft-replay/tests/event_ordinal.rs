//! Source-event ordinal exactness across forward apply, checkpoint restore, and same-ts bursts.

mod common;

use common::*;
use fft_book::Book;
use fft_core::Side;
use fft_log::LogWriter;
use fft_profile::MultiProfile;
use fft_replay::ReplaySource;
use std::time::Duration;

#[test]
fn ordinal_advances_exactly_on_forward_consume() {
    let tmp = temp_path("ordinal-forward");
    write_checkpointed_log(tmp.path(), 80, 40);

    let mut src = ReplaySource::open(tmp.path()).expect("open");
    assert_eq!(src.event_ordinal(), 0);
    let mut book = Book::new(src.meta().min_price_increment);
    let mut profile = MultiProfile::new(src.meta().min_price_increment);
    profile.begin_session(src.meta().trade_date);

    // peek must not advance the ordinal
    let _ = src.peek_event().expect("peek");
    assert_eq!(src.event_ordinal(), 0);

    for expected in 1u64..=8 {
        src.apply_next(&mut book, &mut profile)
            .expect("apply")
            .expect("event");
        assert_eq!(src.event_ordinal(), expected);
    }
    assert_eq!(src.event_ordinal(), 8);

    let progress = src
        .apply_forward(&mut book, &mut profile, 25, Duration::from_secs(60))
        .expect("forward");
    assert_eq!(progress.events, 25);
    assert_eq!(src.event_ordinal(), 33);

    // next_event (no apply) still advances the ordinal — it consumes source events.
    src.next_event().expect("next").expect("event");
    assert_eq!(src.event_ordinal(), 34);
}

#[test]
fn ordinal_exact_across_seek_with_checkpoint_restore() {
    let tmp = temp_path("ordinal-seek");
    write_checkpointed_log(tmp.path(), 400, 100);

    let targets = [
        SESSION_OPEN_NS + 50 * 1_000_000,  // before first checkpoint
        SESSION_OPEN_NS + 150 * 1_000_000, // after first checkpoint
        SESSION_OPEN_NS + 275 * 1_000_000, // mid later segment
        SESSION_OPEN_NS + 399 * 1_000_000, // EOF target
    ];

    for target in targets {
        let mut forward = ReplaySource::open(tmp.path()).expect("open forward");
        let mut fbook = Book::new(forward.meta().min_price_increment);
        let mut fprofile = MultiProfile::new(forward.meta().min_price_increment);
        fprofile.begin_session(forward.meta().trade_date);
        forward_to(&mut forward, &mut fbook, &mut fprofile, target);
        let expected = forward.event_ordinal();

        let mut seek = ReplaySource::open(tmp.path()).expect("open seek");
        let mut sbook = Book::new(seek.meta().min_price_increment);
        let mut sprofile = MultiProfile::new(seek.meta().min_price_increment);
        sprofile.begin_session(seek.meta().trade_date);
        let report = seek
            .seek(target, &mut sbook, &mut sprofile, || false)
            .expect("seek");

        assert!(!report.cancelled);
        assert_eq!(seek.event_ordinal(), expected);
        assert_eq!(report.event_ordinal, expected);
        assert_eq!(seek.applied_seq(), forward.applied_seq());
        assert_eq!(seek.applied_ts(), forward.applied_ts());
    }
}

#[test]
fn ordinal_exact_after_prepare_prior_build_restore() {
    let tmp = temp_path("ordinal-prior");
    write_checkpointed_log(tmp.path(), 250, 100);

    let mut prior = ReplaySource::open(tmp.path()).expect("open prior");
    assert!(prior.checkpoint_count() > 0);
    let (mut pbook, mut pprofile) = prior.prepare_prior_build().expect("prepare");
    let restored_ordinal = prior.event_ordinal();
    let next_after_restore = prior.peek_event().expect("peek").expect("has more");

    // Independent ground truth: consume from open until the same next event.
    let mut forward = ReplaySource::open(tmp.path()).expect("open forward");
    let mut fbook = Book::new(forward.meta().min_price_increment);
    let mut fprofile = MultiProfile::new(forward.meta().min_price_increment);
    fprofile.begin_session(forward.meta().trade_date);
    while let Some(next) = forward.peek_event().expect("peek") {
        if next.ts.0 == next_after_restore.ts.0
            && next.seq.0 == next_after_restore.seq.0
            && next.order_id.0 == next_after_restore.order_id.0
            && next.kind == next_after_restore.kind
        {
            break;
        }
        forward
            .apply_next(&mut fbook, &mut fprofile)
            .expect("apply")
            .expect("event");
    }
    assert_eq!(forward.event_ordinal(), restored_ordinal);

    prior
        .apply_next(&mut pbook, &mut pprofile)
        .expect("apply")
        .expect("event");
    assert_eq!(prior.event_ordinal(), restored_ordinal + 1);
}

#[test]
fn ordinal_exact_across_same_timestamp_bursts() {
    let tmp = temp_path("ordinal-same-ts");
    let meta = es_meta();
    let mut writer = LogWriter::create(tmp.path(), &meta).expect("create");

    // Burst A: 5 events at T0, then checkpoint, then burst B: 5 more at the same T0,
    // then burst C: 5 at T0+1ms. Seeks to T0 must consume A+B (all ts <= target).
    let t0 = SESSION_OPEN_NS;
    let t1 = SESSION_OPEN_NS + 1_000_000;
    let mut book = Book::new(meta.min_price_increment);
    let mut profile = MultiProfile::new(meta.min_price_increment);
    profile.begin_session(meta.trade_date);

    let mut burst_a = Vec::new();
    for i in 0..5 {
        let e = add((i + 1) as u64, Side::Bid, 20_000 - i, 1, t0, (i + 1) as u32);
        book.apply(&e);
        profile.apply(&e);
        burst_a.push(e);
    }
    writer.append_events(&burst_a).expect("append A");
    write_state_checkpoint(&mut writer, &book, &profile);

    let mut burst_b = Vec::new();
    for i in 0..5 {
        let e = add((i + 6) as u64, Side::Ask, 20_000 + i, 1, t0, (i + 6) as u32);
        book.apply(&e);
        profile.apply(&e);
        burst_b.push(e);
    }
    writer.append_events(&burst_b).expect("append B");

    let mut burst_c = Vec::new();
    for i in 0..5 {
        let e = add(
            (i + 11) as u64,
            Side::Bid,
            20_000 - i,
            1,
            t1,
            (i + 11) as u32,
        );
        book.apply(&e);
        profile.apply(&e);
        burst_c.push(e);
    }
    writer.append_events(&burst_c).expect("append C");
    writer.close().expect("close");

    // Forward through all events at t0: ordinal must be 10 (A+B), not stop at the
    // checkpoint boundary or first same-ts hit.
    let mut forward = ReplaySource::open(tmp.path()).expect("open forward");
    let mut fbook = Book::new(forward.meta().min_price_increment);
    let mut fprofile = MultiProfile::new(forward.meta().min_price_increment);
    fprofile.begin_session(forward.meta().trade_date);
    forward_to(&mut forward, &mut fbook, &mut fprofile, t0);
    assert_eq!(forward.event_ordinal(), 10);

    let mut seek = ReplaySource::open(tmp.path()).expect("open seek");
    let mut sbook = Book::new(seek.meta().min_price_increment);
    let mut sprofile = MultiProfile::new(seek.meta().min_price_increment);
    sprofile.begin_session(seek.meta().trade_date);
    let report = seek
        .seek(t0, &mut sbook, &mut sprofile, || false)
        .expect("seek");
    assert_eq!(report.checkpoint_frame, Some(1));
    assert_eq!(seek.event_ordinal(), 10);
    assert_eq!(report.event_ordinal, 10);
    assert_eq!(report.tail_events, 5); // burst B only; A is under the checkpoint

    // After seek, next event is the first t1 event; ordinal continues.
    let next = seek.peek_event().expect("peek").expect("t1 event");
    assert_eq!(next.ts.0, t1);
    seek.apply_next(&mut sbook, &mut sprofile)
        .expect("apply")
        .expect("event");
    assert_eq!(seek.event_ordinal(), 11);
}
