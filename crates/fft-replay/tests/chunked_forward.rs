//! Chunked apply_forward must match a single oneshot drain.

mod common;

use common::*;
use fft_book::Book;
use fft_profile::MultiProfile;
use fft_replay::ReplaySource;
use std::time::Duration;

#[test]
fn chunked_forward_matches_oneshot() {
    let tmp = temp_path("chunked");
    write_checkpointed_log(tmp.path(), 800, 200);

    let mut oneshot = ReplaySource::open(tmp.path()).unwrap();
    let mut oneshot_book = Book::new(oneshot.meta().min_price_increment);
    let mut oneshot_profile = MultiProfile::new(oneshot.meta().min_price_increment);
    oneshot_profile.begin_session(oneshot.meta().trade_date);
    let oneshot_progress = oneshot
        .apply_forward(
            &mut oneshot_book,
            &mut oneshot_profile,
            usize::MAX,
            Duration::from_secs(60),
        )
        .unwrap();
    assert!(oneshot_progress.eof);
    oneshot_book.check_invariants();

    let mut chunked = ReplaySource::open(tmp.path()).unwrap();
    let mut chunked_book = Book::new(chunked.meta().min_price_increment);
    let mut chunked_profile = MultiProfile::new(chunked.meta().min_price_increment);
    chunked_profile.begin_session(chunked.meta().trade_date);
    let mut total = 0u64;
    loop {
        let progress = chunked
            .apply_forward(
                &mut chunked_book,
                &mut chunked_profile,
                64,
                Duration::from_millis(5),
            )
            .unwrap();
        total += progress.events;
        if progress.eof {
            break;
        }
        assert!(
            progress.events > 0 || progress.eof,
            "chunked forward must make progress or hit eof"
        );
    }
    chunked_book.check_invariants();

    assert_eq!(total, oneshot_progress.events);
    assert_eq!(chunked_book.serialize(), oneshot_book.serialize());
    assert_eq!(chunked_profile.serialize(), oneshot_profile.serialize());
    assert_eq!(chunked.applied_seq(), oneshot.applied_seq());
    assert_eq!(chunked.applied_ts(), oneshot.applied_ts());
}
