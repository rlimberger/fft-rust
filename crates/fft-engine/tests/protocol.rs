//! Command protocol: latest-wins seeks, stale discard, shutdown, watermarks.

mod common;

use common::*;
use fft_core::{CanonicalEvent, EventKind, OrderId, Price, Seq, Side, Ts};
use fft_engine::{EngineCmd, EngineConfig, EngineHandle, EngineService, Source};
use fft_log::LogWriter;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
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

/// Hold the engine in its wake callback so a command sequence is guaranteed to
/// be present in one drained batch when the callback is released.
fn spawn_batch_controlled_engine(
    wake_count: Arc<AtomicU64>,
    block_wake: Arc<AtomicBool>,
) -> EngineHandle {
    EngineService::spawn(
        EngineConfig {
            visible_tick_span: 64,
        },
        Box::new(move || {
            wake_count.fetch_add(1, Ordering::SeqCst);
            while block_wake.load(Ordering::Acquire) {
                thread::yield_now();
            }
        }),
    )
    .expect("spawn fft-engine")
}

fn release_wake(block_wake: &AtomicBool) {
    block_wake.store(false, Ordering::Release);
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

#[test]
fn set_source_sim_live_drops_earlier_batched_seek() {
    let replay = temp_path("simlive-drop-seek-src");
    write_checkpointed_log(replay.path(), 400, 100);
    let live_out = temp_path("simlive-drop-seek-live");
    let wakes = Arc::new(AtomicU64::new(0));
    let handle = spawn_engine(wakes);

    // Seed a Replay source so a Seek can be queued, then replace with SimLive
    // in the same command batch — ENGINE.md §5: SetSource(SimLive) clears any
    // earlier batched seek (join never checkpoint-skips).
    handle
        .send(EngineCmd::SetSource(Source::Replay {
            path: replay.path().to_path_buf(),
        }))
        .unwrap();
    thread::sleep(Duration::from_millis(20));

    let head_ts = SESSION_OPEN_NS + 200 * 1_000_000;
    handle
        .send(EngineCmd::Seek {
            ts: SESSION_OPEN_NS + 350 * 1_000_000,
            generation: 7,
        })
        .unwrap();
    handle
        .send(EngineCmd::SetSource(Source::SimLive {
            path: replay.path().to_path_buf(),
            head_ts,
            live_out: live_out.path().to_path_buf(),
        }))
        .unwrap();

    wait_until(Duration::from_secs(10), || {
        let snap = handle.snapshots().load();
        snap.applied_ts >= head_ts && snap.seek_generation == 0
    });

    let exit = handle.shutdown().expect("join");
    assert_eq!(
        exit.seeks_executed, 0,
        "batched Seek before SetSource(SimLive) must be dropped"
    );
    assert!(exit.coverage.events_applied > 0);
}

#[test]
fn go_live_on_plain_replay_panics() {
    let tmp = temp_path("golive-replay");
    write_checkpointed_log(tmp.path(), 80, 40);
    let wakes = Arc::new(AtomicU64::new(0));
    let mut handle = spawn_engine(wakes);

    handle
        .send(EngineCmd::SetSource(Source::Replay {
            path: tmp.path().to_path_buf(),
        }))
        .unwrap();
    handle.send(EngineCmd::GoLive).unwrap();

    let payload = handle
        .join()
        .expect("engine join handle")
        .expect_err("GoLive on Replay must panic");
    let message = panic_message(&*payload);
    assert!(
        message.contains("GoLive") && message.contains("live source"),
        "unexpected panic message: {message}"
    );
}

#[test]
fn go_live_resumes_wall_pinned_streaming_after_catch_up() {
    let replay = temp_path("golive-resume-src");
    // Dense 1 ms steps so wall-pin has events to stream after the head.
    write_checkpointed_log(replay.path(), 2_000, 500);
    let live_out = temp_path("golive-resume-live");
    let wakes = Arc::new(AtomicU64::new(0));
    let handle = spawn_engine(wakes);

    let head_ts = SESSION_OPEN_NS + 200 * 1_000_000;
    handle
        .send(EngineCmd::SetSource(Source::SimLive {
            path: replay.path().to_path_buf(),
            head_ts,
            live_out: live_out.path().to_path_buf(),
        }))
        .unwrap();

    wait_until(Duration::from_secs(10), || {
        handle.snapshots().load().applied_ts >= head_ts
    });

    // Scrub back + slow transport, then GoLive must cancel speed to 1× and
    // catch up to the wall-pinned head.
    handle
        .send(EngineCmd::Seek {
            ts: SESSION_OPEN_NS + 50 * 1_000_000,
            generation: 1,
        })
        .unwrap();
    wait_for_seek(&handle, 1);
    handle.send(EngineCmd::SetSpeed(0.25)).unwrap();
    handle.send(EngineCmd::Play).unwrap();
    thread::sleep(Duration::from_millis(30));
    let before = handle.snapshots().load().applied_ts;
    handle.send(EngineCmd::GoLive).unwrap();

    wait_until(Duration::from_secs(10), || {
        let snap = handle.snapshots().load();
        snap.seek_generation == 0 && snap.applied_ts > before && snap.applied_ts >= head_ts
    });

    // After GoLive catch-up, wall-pin must keep streaming (applied_ts advances
    // with wall clock past the join head).
    let pinned = handle.snapshots().load().applied_ts;
    wait_until(Duration::from_secs(5), || {
        handle.snapshots().load().applied_ts > pinned
    });

    let _ = handle.shutdown().expect("join");
}

#[test]
fn seek_play_seek_batch_leaves_final_seek_paused() {
    let tmp = temp_path("seek-play-seek-order");
    write_checkpointed_log(tmp.path(), 300, 100);
    let wakes = Arc::new(AtomicU64::new(0));
    let block_wake = Arc::new(AtomicBool::new(false));
    let handle = spawn_batch_controlled_engine(wakes, block_wake.clone());

    handle
        .send(EngineCmd::SetSource(Source::Replay {
            path: tmp.path().to_path_buf(),
        }))
        .unwrap();
    block_wake.store(true, Ordering::Release);
    handle
        .send(EngineCmd::Seek {
            ts: SESSION_OPEN_NS + 50 * 1_000_000,
            generation: 1,
        })
        .unwrap();
    wait_for_seek(&handle, 1);
    handle.send(EngineCmd::SetSpeed(1_000_000.0)).unwrap();

    handle
        .send(EngineCmd::Seek {
            ts: SESSION_OPEN_NS + 100 * 1_000_000,
            generation: 2,
        })
        .unwrap();
    handle.send(EngineCmd::Play).unwrap();
    handle
        .send(EngineCmd::Seek {
            ts: SESSION_OPEN_NS + 200 * 1_000_000,
            generation: 3,
        })
        .unwrap();
    release_wake(&block_wake);

    let scrubbed = wait_for_seek(&handle, 3);
    thread::sleep(Duration::from_millis(50));
    let final_snapshot = handle.snapshots().load();
    assert_eq!(
        final_snapshot.seek_generation, 3,
        "final Seek must remain scrubbed"
    );
    assert_eq!(
        final_snapshot.applied_seq, scrubbed.applied_seq,
        "Play between two coalesced seeks must not resume after the final Seek"
    );

    let _ = handle.shutdown().expect("join");
}

#[test]
fn set_source_clears_batched_seek_resume_intent() {
    let first = temp_path("resume-source-first");
    let second = temp_path("resume-source-second");
    write_checkpointed_log(first.path(), 300, 100);
    write_checkpointed_log(second.path(), 300, 100);
    let wakes = Arc::new(AtomicU64::new(0));
    let block_wake = Arc::new(AtomicBool::new(false));
    let handle = spawn_batch_controlled_engine(wakes, block_wake.clone());

    handle
        .send(EngineCmd::SetSource(Source::Replay {
            path: first.path().to_path_buf(),
        }))
        .unwrap();
    block_wake.store(true, Ordering::Release);
    handle
        .send(EngineCmd::Seek {
            ts: SESSION_OPEN_NS + 50 * 1_000_000,
            generation: 9,
        })
        .unwrap();
    wait_for_seek(&handle, 9);
    handle.send(EngineCmd::SetSpeed(1_000_000.0)).unwrap();

    handle
        .send(EngineCmd::Seek {
            ts: SESSION_OPEN_NS + 100 * 1_000_000,
            generation: 10,
        })
        .unwrap();
    handle.send(EngineCmd::Play).unwrap();
    handle
        .send(EngineCmd::SetSource(Source::Replay {
            path: second.path().to_path_buf(),
        }))
        .unwrap();
    handle
        .send(EngineCmd::Seek {
            ts: SESSION_OPEN_NS + 200 * 1_000_000,
            generation: 1,
        })
        .unwrap();
    release_wake(&block_wake);

    let scrubbed = wait_for_seek(&handle, 1);
    thread::sleep(Duration::from_millis(50));
    let final_snapshot = handle.snapshots().load();
    assert_eq!(
        final_snapshot.seek_generation, 1,
        "new-source Seek must remain scrubbed"
    );
    assert_eq!(
        final_snapshot.applied_seq, scrubbed.applied_seq,
        "SetSource must clear resume intent from the old source"
    );

    let _ = handle.shutdown().expect("join");
}

#[test]
fn seek_then_go_live_batch_finishes_live() {
    let replay = temp_path("seek-golive-order-src");
    write_checkpointed_log(replay.path(), 2_000, 500);
    let live_out = temp_path("seek-golive-order-live");
    let wakes = Arc::new(AtomicU64::new(0));
    let block_wake = Arc::new(AtomicBool::new(false));
    let handle = spawn_batch_controlled_engine(wakes, block_wake.clone());
    let head_ts = SESSION_OPEN_NS + 200 * 1_000_000;

    handle
        .send(EngineCmd::SetSource(Source::SimLive {
            path: replay.path().to_path_buf(),
            head_ts,
            live_out: live_out.path().to_path_buf(),
        }))
        .unwrap();
    wait_until(Duration::from_secs(10), || {
        handle.snapshots().load().applied_ts >= head_ts
    });
    let live_before_batch = handle.snapshots().load().applied_ts;

    block_wake.store(true, Ordering::Release);
    handle
        .send(EngineCmd::Seek {
            ts: SESSION_OPEN_NS + 75 * 1_000_000,
            generation: 1,
        })
        .unwrap();
    wait_for_seek(&handle, 1);
    handle
        .send(EngineCmd::Seek {
            ts: SESSION_OPEN_NS + 50 * 1_000_000,
            generation: 2,
        })
        .unwrap();
    handle.send(EngineCmd::GoLive).unwrap();
    release_wake(&block_wake);

    wait_until(Duration::from_secs(5), || {
        let snapshot = handle.snapshots().load();
        snapshot.seek_generation == 0 && snapshot.applied_ts >= live_before_batch
    });
    let final_snapshot = handle.snapshots().load();
    assert_eq!(
        final_snapshot.seek_generation, 0,
        "GoLive after Seek must win the batch"
    );
    assert!(
        final_snapshot.applied_ts >= live_before_batch,
        "GoLive must finish at the live side, not at the earlier seek target"
    );

    let _ = handle.shutdown().expect("join");
}

#[test]
fn go_live_then_seek_batch_finishes_scrubbed() {
    let replay = temp_path("golive-seek-order-src");
    write_checkpointed_log(replay.path(), 2_000, 500);
    let live_out = temp_path("golive-seek-order-live");
    let wakes = Arc::new(AtomicU64::new(0));
    let block_wake = Arc::new(AtomicBool::new(false));
    let handle = spawn_batch_controlled_engine(wakes, block_wake.clone());
    let head_ts = SESSION_OPEN_NS + 200 * 1_000_000;

    handle
        .send(EngineCmd::SetSource(Source::SimLive {
            path: replay.path().to_path_buf(),
            head_ts,
            live_out: live_out.path().to_path_buf(),
        }))
        .unwrap();
    wait_until(Duration::from_secs(10), || {
        handle.snapshots().load().applied_ts >= head_ts
    });

    block_wake.store(true, Ordering::Release);
    handle
        .send(EngineCmd::Seek {
            ts: SESSION_OPEN_NS + 75 * 1_000_000,
            generation: 1,
        })
        .unwrap();
    wait_for_seek(&handle, 1);
    handle.send(EngineCmd::GoLive).unwrap();
    handle
        .send(EngineCmd::Seek {
            ts: SESSION_OPEN_NS + 50 * 1_000_000,
            generation: 2,
        })
        .unwrap();
    release_wake(&block_wake);

    let scrubbed = wait_for_seek(&handle, 2);
    thread::sleep(Duration::from_millis(50));
    let final_snapshot = handle.snapshots().load();
    assert_eq!(
        final_snapshot.seek_generation, 2,
        "Seek after GoLive must win the batch"
    );
    assert_eq!(
        final_snapshot.applied_seq, scrubbed.applied_seq,
        "final Seek must remain scrubbed"
    );

    let _ = handle.shutdown().expect("join");
}

#[test]
fn logged_seq_decoupled_from_applied_seq_on_sim_live() {
    let replay = temp_path("logged-decouple-src");
    write_checkpointed_log(replay.path(), 800, 200);
    let live_out = temp_path("logged-decouple-live");
    let wakes = Arc::new(AtomicU64::new(0));
    let handle = spawn_engine(wakes);

    let head_ts = SESSION_OPEN_NS + 600 * 1_000_000;
    handle
        .send(EngineCmd::SetSource(Source::SimLive {
            path: replay.path().to_path_buf(),
            head_ts,
            live_out: live_out.path().to_path_buf(),
        }))
        .unwrap();

    wait_until(Duration::from_secs(10), || {
        handle.snapshots().load().applied_ts >= head_ts
    });
    let tip_seq = handle.snapshots().load().applied_seq;
    assert!(tip_seq > 0);

    // Scrub back: applied_seq moves with the restore; logged_seq must stay at
    // the live tip (SimLive append owns logged_seq — ENGINE.md §5.4).
    handle
        .send(EngineCmd::Seek {
            ts: SESSION_OPEN_NS + 100 * 1_000_000,
            generation: 1,
        })
        .unwrap();
    let scrubbed = wait_for_seek(&handle, 1);
    assert!(
        scrubbed.applied_seq < tip_seq,
        "seek must move applied_seq behind the tip"
    );

    let exit = handle.shutdown().expect("join");
    assert!(
        exit.watermarks.logged_seq > exit.watermarks.applied_seq,
        "logged_seq {} must remain tip-side of scrubbed applied_seq {}",
        exit.watermarks.logged_seq,
        exit.watermarks.applied_seq
    );
    assert_eq!(exit.watermarks.applied_seq, scrubbed.applied_seq);
}
