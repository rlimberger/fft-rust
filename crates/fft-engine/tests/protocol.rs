//! Command protocol: latest-wins seeks, stale discard, shutdown, watermarks.

mod common;

use common::*;
use fft_core::{CanonicalEvent, EventKind, OrderId, Price, Seq, Side, Ts};
use fft_engine::{EngineCmd, Source};
use fft_log::LogWriter;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::thread;
use std::time::Duration;

/// Panic payload as text, for asserting on a panic that crossed the engine-thread join.
fn panic_message(payload: &(dyn std::any::Any + Send)) -> String {
    payload
        .downcast_ref::<&str>()
        .map(|msg| (*msg).to_string())
        .or_else(|| payload.downcast_ref::<String>().cloned())
        .unwrap_or_else(|| panic!("engine panic payload was neither &str nor String"))
}

/// Six Adds: channel seqs 10, 11, then a snapshot block replaying original order-entry
/// seqs 500, 499, 498 (regressing, as Databento delivers them), then channel seq 12.
fn write_snapshot_block_log(path: &Path) {
    let meta = es_meta();
    let mut writer = LogWriter::create(path, &meta).expect("create log");
    let add = |order_id: u64, seq: u32, index: u64, flags: u16| CanonicalEvent {
        kind: EventKind::Add,
        side: if order_id.is_multiple_of(2) {
            Side::Bid
        } else {
            Side::Ask
        },
        flags,
        size: 3,
        ts: Ts(SESSION_OPEN_NS + index * 1_000_000),
        seq: Seq(seq),
        price: Price(if order_id.is_multiple_of(2) {
            (20_000 - 1) * TICK
        } else {
            (20_000 + 1) * TICK
        }),
        order_id: OrderId(order_id),
    };
    let snapshot = fft_core::DATABENTO_SNAPSHOT_FLAG;
    let events = [
        add(10, 10, 0, 0),
        add(11, 11, 1, 0),
        add(500, 500, 2, snapshot),
        add(499, 499, 3, snapshot),
        add(498, 498, 4, snapshot),
        add(12, 12, 5, 0),
    ];
    writer.append_events(&events).expect("append");
    writer.close().expect("close");
}

#[test]
fn snapshot_seqs_never_move_the_applied_watermark() {
    let tmp = temp_path("snapshot-seqs");
    write_snapshot_block_log(tmp.path());
    let wakes = Arc::new(AtomicU64::new(0));
    let handle = spawn_engine(wakes.clone());

    handle
        .send(EngineCmd::SetSource(Source::Replay {
            path: tmp.path().to_path_buf(),
        }))
        .unwrap();
    handle.send(EngineCmd::SetSpeed(1_000_000.0)).unwrap();
    handle.send(EngineCmd::Play).unwrap();

    wait_until(Duration::from_secs(5), || {
        handle.snapshots().load().coverage.events_applied == 6
    });

    let exit = handle.shutdown().expect("join");
    // FFTLOG-V2 §4: the regressing snapshot seqs 500/499/498 are not channel sequencing,
    // so the watermark tracks only the channel events and ends at the last of them.
    assert_eq!(exit.watermarks.applied_seq, 12);
    assert_eq!(exit.watermarks.published_seq, 12);
    assert_eq!(exit.coverage.events_read, 6);
    assert_eq!(exit.coverage.events_applied, 6);
}

#[test]
fn seek_without_checkpoints_panics_with_the_fft_checkpoint_remediation() {
    let tmp = temp_path("no-checkpoints");
    write_event_only_log(tmp.path(), 200, 1_000_000);
    let wakes = Arc::new(AtomicU64::new(0));
    let mut handle = spawn_engine(wakes);

    handle
        .send(EngineCmd::SetSource(Source::Replay {
            path: tmp.path().to_path_buf(),
        }))
        .unwrap();
    handle
        .send(EngineCmd::Seek {
            ts: SESSION_OPEN_NS + 100 * 1_000_000,
            generation: 1,
        })
        .unwrap();

    // ENGINE.md §4(3): replaying from frame zero is forbidden, so the engine thread dies.
    let payload = handle
        .join()
        .expect("engine join handle")
        .expect_err("seek against a checkpoint-less log must panic");
    let message = panic_message(&*payload);
    assert!(
        message.contains("zero checkpoints") && message.contains("fft-checkpoint"),
        "unexpected panic message: {message}"
    );
    assert!(
        message.contains(&tmp.path().display().to_string()),
        "panic message must name the log: {message}"
    );
}

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
fn published_snapshot_carries_log_header_symbol() {
    let tmp = temp_path("header-symbol");
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
    let snap = wait_for_seek(&handle, 1);
    assert_eq!(snap.symbol.as_ref(), "ESU6");

    let _ = handle.shutdown().expect("join");
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
fn shutdown_after_engine_death_returns_the_panic_instead_of_panicking() {
    let wakes = Arc::new(AtomicU64::new(0));
    let handle = spawn_engine(wakes);

    // A nonexistent log kills the engine thread on SetSource (replay_panic).
    handle
        .send(EngineCmd::SetSource(Source::Replay {
            path: PathBuf::from("/nonexistent/fft-test-no-such.fftlog"),
        }))
        .unwrap();
    // Give the thread time to die so the later send hits a closed channel.
    thread::sleep(Duration::from_millis(100));

    // shutdown() must surface the engine panic as Err — never panic itself,
    // or a 60 s gate run's evidence dies with it (main.rs writes JSON first).
    let payload = handle
        .shutdown()
        .expect_err("dead engine must surface its panic through shutdown");
    let message = panic_message(&*payload);
    assert!(
        message.contains("fft-engine replay failure"),
        "unexpected panic message: {message}"
    );
}

#[test]
fn seek_then_play_in_one_batch_resumes_from_the_anchor() {
    let tmp = temp_path("anchored-start");
    write_checkpointed_log(tmp.path(), 300, 100);
    let wakes = Arc::new(AtomicU64::new(0));
    let handle = spawn_engine(wakes);

    // The `--replay-at` shell sequence: SetSource → Seek → Play, one burst. Batch
    // order must mean "seek, then play from there" — the coalesced seek executing
    // after the drain loop must not swallow the Play that followed it.
    handle
        .send(EngineCmd::SetSource(Source::Replay {
            path: tmp.path().to_path_buf(),
        }))
        .unwrap();
    handle
        .send(EngineCmd::Seek {
            ts: SESSION_OPEN_NS + 150 * 1_000_000,
            generation: 1,
        })
        .unwrap();
    handle.send(EngineCmd::SetSpeed(1_000_000.0)).unwrap();
    handle.send(EngineCmd::Play).unwrap();

    // The seek's own publication (seek_generation 1) is transient: forward flow
    // resumes immediately and publishes with generation 0, so observe durable
    // facts instead — flow reaching the end of the log past the anchor.
    wait_until(Duration::from_secs(5), || {
        handle.snapshots().load().applied_seq == 300
    });

    let exit = handle.shutdown().expect("join");
    assert_eq!(exit.seeks_executed, 1);
    // Coverage counts only forward flow after the anchor — strictly fewer than
    // the whole log, proving Play resumed from the seek, not from frame zero.
    assert!(
        exit.coverage.events_applied > 0 && exit.coverage.events_applied < 300,
        "forward flow resumed from the anchor, applied {} events",
        exit.coverage.events_applied
    );
    assert_eq!(exit.watermarks.applied_seq, 300);
}

#[test]
fn pause_after_seek_in_one_batch_stays_paused() {
    let tmp = temp_path("seek-pause");
    write_checkpointed_log(tmp.path(), 300, 100);
    let wakes = Arc::new(AtomicU64::new(0));
    let handle = spawn_engine(wakes);

    handle
        .send(EngineCmd::SetSource(Source::Replay {
            path: tmp.path().to_path_buf(),
        }))
        .unwrap();
    handle.send(EngineCmd::SetSpeed(1_000_000.0)).unwrap();
    handle.send(EngineCmd::Play).unwrap();
    handle
        .send(EngineCmd::Seek {
            ts: SESSION_OPEN_NS + 150 * 1_000_000,
            generation: 1,
        })
        .unwrap();
    handle.send(EngineCmd::Pause).unwrap();

    let anchored = wait_for_seek(&handle, 1);
    // Pause followed the seek in batch order: no forward flow after it lands.
    std::thread::sleep(Duration::from_millis(50));
    let now = handle.snapshots().load();
    assert_eq!(
        now.applied_seq, anchored.applied_seq,
        "engine stayed paused"
    );
    assert_eq!(now.coverage.events_applied, 0, "no forward events applied");

    let exit = handle.shutdown().expect("join");
    assert_eq!(exit.seeks_executed, 1);
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
