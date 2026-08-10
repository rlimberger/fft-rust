//! Command protocol: latest-wins seeks, stale discard, shutdown, watermarks.

mod common;

use common::*;
use fft_engine::{EngineCmd, Source};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::thread;
use std::time::Duration;

#[test]
fn latest_wins_coalesces_seek_batch() {
    let tmp = temp_path("latest-wins");
    write_checkpointed_log(tmp.path(), 300, 100);
    let wakes = Arc::new(AtomicU64::new(0));
    let handle = spawn_engine(wakes.clone());

    handle
        .send(EngineCmd::SetSource(Source::Replay {
            path: tmp.path().to_path_buf(),
        }))
        .unwrap();
    thread::sleep(Duration::from_millis(40));

    let target = SESSION_OPEN_NS + 250 * 1_000_000;
    for generation in 1..=32 {
        handle
            .send(EngineCmd::Seek {
                ts: target,
                generation,
            })
            .unwrap();
    }

    let snap = wait_for_seek(&handle, 32);
    assert_eq!(snap.seek_generation, 32);
    assert!(snap.generation >= 1);
    // Publish order is slot-then-wake, so the wake can trail the observable snapshot.
    wait_until(Duration::from_secs(5), || wakes.load(Ordering::SeqCst) >= 1);
    assert_eq!(wakes.load(Ordering::SeqCst), 1);

    let exit = handle.shutdown().expect("join");
    assert_eq!(exit.seeks_executed, 1);
    assert_eq!(exit.publications, 1);
    assert_eq!(exit.watermarks.published_seq, exit.watermarks.applied_seq);
}

#[test]
fn stale_completed_seek_is_discarded_before_publish() {
    let tmp = temp_path("stale-discard");
    write_checkpointed_log(tmp.path(), 20_000, 5_000);
    let wakes = Arc::new(AtomicU64::new(0));
    let handle = spawn_engine(wakes.clone());

    handle
        .send(EngineCmd::SetSource(Source::Replay {
            path: tmp.path().to_path_buf(),
        }))
        .unwrap();
    thread::sleep(Duration::from_millis(40));

    let far = SESSION_OPEN_NS + 19_999 * 1_000_000;
    handle
        .send(EngineCmd::Seek {
            ts: far,
            generation: 1,
        })
        .unwrap();
    thread::sleep(Duration::from_millis(2));
    handle
        .send(EngineCmd::Seek {
            ts: far,
            generation: 2,
        })
        .unwrap();

    let snap = wait_for_seek(&handle, 2);
    assert_eq!(snap.seek_generation, 2);
    assert_eq!(wakes.load(Ordering::SeqCst), 1);

    let exit = handle.shutdown().expect("join");
    assert_eq!(exit.publications, 1);
    assert_eq!(exit.seeks_executed, 1);
}

#[test]
fn shutdown_flushes_and_joins() {
    let tmp = temp_path("shutdown");
    write_checkpointed_log(tmp.path(), 80, 40);
    let wakes = Arc::new(AtomicU64::new(0));
    let handle = spawn_engine(wakes);

    handle
        .send(EngineCmd::SetSource(Source::Replay {
            path: tmp.path().to_path_buf(),
        }))
        .unwrap();
    handle
        .send(EngineCmd::Seek {
            ts: SESSION_OPEN_NS + 40 * 1_000_000,
            generation: 1,
        })
        .unwrap();
    wait_for_seek(&handle, 1);

    let exit = handle.shutdown().expect("join");
    assert!(exit.book_bytes.is_some());
    assert!(exit.profile_bytes.is_some());
    assert!(exit.publications >= 1);
    assert!(exit.seeks_executed >= 1);
}

#[test]
fn watermark_invariants_hold_after_play() {
    let tmp = temp_path("watermarks");
    write_checkpointed_log(tmp.path(), 120, 60);
    let wakes = Arc::new(AtomicU64::new(0));
    let handle = spawn_engine(wakes.clone());

    handle
        .send(EngineCmd::SetSource(Source::Replay {
            path: tmp.path().to_path_buf(),
        }))
        .unwrap();
    handle.send(EngineCmd::SetSpeed(1_000_000.0)).unwrap();
    handle.send(EngineCmd::Play).unwrap();

    wait_until(Duration::from_secs(5), || wakes.load(Ordering::SeqCst) >= 1);
    wait_until(Duration::from_secs(5), || {
        let snap = handle.snapshots().load();
        snap.applied_seq > 0 && snap.seek_generation == 0
    });

    // Coverage counters: forward flow reads and applies every event exactly once.
    let snap = handle.snapshots().load();
    assert_eq!(snap.coverage.events_read, snap.coverage.events_applied);
    assert!(snap.coverage.events_read > 0);
    assert_eq!(snap.coverage.gap_records, 0);

    let exit = handle.shutdown().expect("join");
    let w = exit.watermarks;
    assert_eq!(w.received_seq, w.decoded_seq);
    assert_eq!(w.decoded_seq, w.applied_seq);
    assert_eq!(w.applied_seq, w.logged_seq);
    assert!(w.published_seq <= w.applied_seq);
    assert!(w.published_seq > 0);
    assert!(exit.publications >= 1);
}

#[test]
fn set_source_resets_seek_generations() {
    let tmp = temp_path("source-switch");
    write_checkpointed_log(tmp.path(), 80, 40);
    let wakes = Arc::new(AtomicU64::new(0));
    let handle = spawn_engine(wakes);

    handle
        .send(EngineCmd::SetSource(Source::Replay {
            path: tmp.path().to_path_buf(),
        }))
        .unwrap();
    handle
        .send(EngineCmd::Seek {
            ts: SESSION_OPEN_NS + 40 * 1_000_000,
            generation: 5,
        })
        .unwrap();
    wait_for_seek(&handle, 5);

    // ENGINE.md §4: SetSource resets latest_seek to 0, so a fresh UI generation
    // counter starting at 1 must be accepted, not a "generation regressed" panic.
    handle
        .send(EngineCmd::SetSource(Source::Replay {
            path: tmp.path().to_path_buf(),
        }))
        .unwrap();
    handle
        .send(EngineCmd::Seek {
            ts: SESSION_OPEN_NS + 20 * 1_000_000,
            generation: 1,
        })
        .unwrap();
    wait_for_seek(&handle, 1);

    let exit = handle.shutdown().expect("join");
    assert!(exit.seeks_executed >= 1);
}

#[test]
fn seek_then_backward_seek_resets_watermarks() {
    let tmp = temp_path("seek-back");
    write_checkpointed_log(tmp.path(), 200, 50);
    let wakes = Arc::new(AtomicU64::new(0));
    let handle = spawn_engine(wakes);

    handle
        .send(EngineCmd::SetSource(Source::Replay {
            path: tmp.path().to_path_buf(),
        }))
        .unwrap();
    handle
        .send(EngineCmd::Seek {
            ts: SESSION_OPEN_NS + 180 * 1_000_000,
            generation: 1,
        })
        .unwrap();
    let late = wait_for_seek(&handle, 1);
    assert!(late.applied_seq > 50);

    handle
        .send(EngineCmd::Seek {
            ts: SESSION_OPEN_NS + 20 * 1_000_000,
            generation: 2,
        })
        .unwrap();
    let early = wait_for_seek(&handle, 2);
    assert!(early.applied_seq < late.applied_seq);
    assert_eq!(early.seek_generation, 2);

    let exit = handle.shutdown().expect("join");
    assert_eq!(exit.watermarks.applied_seq, early.applied_seq);
    assert_eq!(exit.watermarks.published_seq, early.applied_seq);
    assert_eq!(exit.seeks_executed, 2);
    assert_eq!(exit.publications, 2);
}
