//! Differential-correctness spine: a random event stream applied chunked vs
//! one-shot yields identical profiles, and checkpoint-restore-plus-tail is
//! identical to forward application (never replay-as-events).

mod common;

use common::*;
use fft_core::{CanonicalEvent, Price, Side};
use fft_profile::MultiProfile;
use proptest::prelude::*;

/// `(dt_seconds, tick_offset, size, kind)` → event stream inside the Wed
/// session. Worst case 150 × 300 s = 12.5 h, well inside the 23 h session.
fn stream(raw: Vec<(u64, i64, u32, u8)>) -> Vec<CanonicalEvent> {
    let mut ts = SESSION_OPEN_NS;
    raw.into_iter()
        .map(|(dt, off, size, kind)| {
            ts += dt * 1_000_000_000;
            match kind {
                0 => gap(ts),
                k if k % 3 == 1 => trade(20_000 + off, size, Side::Bid, ts),
                k if k % 3 == 2 => trade(20_000 + off, size, Side::Ask, ts),
                _ => trade(20_000 + off, size, Side::None, ts),
            }
        })
        .collect()
}

fn apply_all(events: &[CanonicalEvent]) -> MultiProfile {
    let mut p = MultiProfile::new(Price(TICK));
    p.begin_session(TRADE_DATE);
    for ev in events {
        p.apply(ev);
    }
    p
}

proptest! {
    #[test]
    fn chunked_equals_oneshot(
        raw in prop::collection::vec((0u64..300, 0i64..50, 1u32..50, 0u8..10), 1..150),
        chunk in 1usize..20,
    ) {
        let events = stream(raw);
        let oneshot = apply_all(&events);

        let mut chunked = MultiProfile::new(Price(TICK));
        chunked.begin_session(TRADE_DATE);
        for slice in events.chunks(chunk) {
            for ev in slice {
                chunked.apply(ev);
            }
        }

        prop_assert_eq!(&chunked, &oneshot);
        prop_assert_eq!(chunked.serialize(), oneshot.serialize());
    }

    #[test]
    fn restore_plus_tail_equals_forward(
        raw in prop::collection::vec((0u64..300, 0i64..50, 1u32..50, 0u8..10), 1..150),
        split in 0usize..150,
    ) {
        let events = stream(raw);
        let split = split % (events.len() + 1);
        let forward = apply_all(&events);

        // Checkpoint mid-stream, restore, then apply only the tail.
        let head = apply_all(&events[..split]);
        let secs = head.serialize();
        let mut resumed = MultiProfile::restore(&secs.profile, &secs.cvd, &secs.session)
            .expect("restore own serialization");
        // serialize(restore(x)) must already be byte-identical.
        prop_assert_eq!(secs, resumed.serialize());
        for ev in &events[split..] {
            resumed.apply(ev);
        }

        prop_assert_eq!(&resumed, &forward);
        prop_assert_eq!(resumed.serialize(), forward.serialize());
    }
}
