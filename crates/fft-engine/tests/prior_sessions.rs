//! `EngineCmd::LoadPriorSession` — profile-only prior-day builds
//! (`docs/ENGINE.md` §2).

mod common;

use common::*;
use fft_engine::{EngineCmd, Source};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::AtomicU64;
use std::time::Duration;

/// Snapshot session_volume total for a trade date, or None if absent.
fn session_volume(snap: &fft_engine::RenderSnapshot, trade_date: u32) -> Option<u64> {
    snap.profile
        .sessions
        .iter()
        .find(|s| s.trade_date == trade_date)
        .map(|s| s.rows.iter().map(|r| r.session_volume).sum())
}

#[test]
fn prior_loads_while_playing_two_sessions_ascending_coverage_unchanged() {
    let main = temp_path("prior-main");
    let prior = temp_path("prior-day");
    write_checkpointed_log(main.path(), 400, 100);
    write_checkpointed_log_for(prior.path(), PRIOR_TRADE_DATE, 200, 50);
    let expected_prior_vol = offline_profile_volume(prior.path());
    assert!(expected_prior_vol > 0, "fixture must produce volume");

    // Baseline: same main log, no prior load.
    let baseline_wakes = Arc::new(AtomicU64::new(0));
    let baseline = spawn_engine(baseline_wakes);
    baseline
        .send(EngineCmd::SetSource(Source::Replay {
            path: main.path().to_path_buf(),
        }))
        .unwrap();
    baseline.send(EngineCmd::SetSpeed(1_000_000.0)).unwrap();
    baseline.send(EngineCmd::Play).unwrap();
    wait_until(Duration::from_secs(5), || {
        baseline.snapshots().load().coverage.events_applied == 400
    });
    let baseline_exit = baseline.shutdown().expect("join");
    assert_eq!(baseline_exit.coverage.events_applied, 400);
    assert_eq!(baseline_exit.coverage.events_read, 400);

    // With prior load interleaved at high speed.
    let wakes = Arc::new(AtomicU64::new(0));
    let handle = spawn_engine(wakes);
    handle
        .send(EngineCmd::SetSource(Source::Replay {
            path: main.path().to_path_buf(),
        }))
        .unwrap();
    handle
        .send(EngineCmd::LoadPriorSession {
            path: prior.path().to_path_buf(),
        })
        .unwrap();
    handle.send(EngineCmd::SetSpeed(1_000_000.0)).unwrap();
    handle.send(EngineCmd::Play).unwrap();

    wait_until(Duration::from_secs(10), || {
        let snap = handle.snapshots().load();
        snap.coverage.events_applied == 400 && snap.profile.sessions.len() == 2
    });

    let snap = handle.snapshots().load();
    assert_eq!(snap.profile.sessions.len(), 2);
    assert_eq!(snap.profile.sessions[0].trade_date, PRIOR_TRADE_DATE);
    assert_eq!(snap.profile.sessions[1].trade_date, TRADE_DATE);
    // Current session is always last.
    assert_eq!(
        snap.profile.sessions.last().map(|s| s.trade_date),
        Some(TRADE_DATE)
    );
    assert_eq!(
        session_volume(&snap, PRIOR_TRADE_DATE),
        Some(expected_prior_vol)
    );

    let exit = handle.shutdown().expect("join");
    assert_eq!(
        exit.coverage.events_applied,
        baseline_exit.coverage.events_applied
    );
    assert_eq!(
        exit.coverage.events_read,
        baseline_exit.coverage.events_read
    );
    assert_eq!(
        exit.coverage.gap_records,
        baseline_exit.coverage.gap_records
    );
    assert_eq!(exit.priors_completed, 1);
    assert_eq!(exit.prior_skips, 0);
    // Prior work must not move watermarks past the forward path.
    assert_eq!(
        exit.watermarks.applied_seq,
        baseline_exit.watermarks.applied_seq
    );
}

#[test]
fn prior_load_missing_file_is_counted_skip_engine_keeps_playing() {
    let main = temp_path("prior-skip-main");
    write_checkpointed_log(main.path(), 120, 60);
    let wakes = Arc::new(AtomicU64::new(0));
    let handle = spawn_engine(wakes);

    handle
        .send(EngineCmd::SetSource(Source::Replay {
            path: main.path().to_path_buf(),
        }))
        .unwrap();
    handle
        .send(EngineCmd::LoadPriorSession {
            path: PathBuf::from("/nonexistent/fft-prior-no-such.fftlog"),
        })
        .unwrap();
    handle.send(EngineCmd::SetSpeed(1_000_000.0)).unwrap();
    handle.send(EngineCmd::Play).unwrap();

    wait_until(Duration::from_secs(5), || {
        handle.snapshots().load().coverage.events_applied == 120
    });

    let snap = handle.snapshots().load();
    assert_eq!(snap.profile.sessions.len(), 1);
    assert_eq!(snap.profile.sessions[0].trade_date, TRADE_DATE);

    let exit = handle.shutdown().expect("join");
    assert_eq!(exit.prior_skips, 1);
    assert_eq!(exit.priors_completed, 0);
    assert_eq!(exit.coverage.events_applied, 120);
}

#[test]
fn set_source_mid_build_drops_in_progress_prior() {
    let main = temp_path("prior-drop-main");
    let main2 = temp_path("prior-drop-main2");
    let prior = temp_path("prior-drop-day");
    // Large prior so the 2 ms slices cannot finish before SetSource lands.
    write_checkpointed_log(main.path(), 80, 40);
    write_checkpointed_log(main2.path(), 80, 40);
    write_checkpointed_log_for(prior.path(), PRIOR_TRADE_DATE, 80_000, 10_000);

    let wakes = Arc::new(AtomicU64::new(0));
    let handle = spawn_engine(wakes);
    handle
        .send(EngineCmd::SetSource(Source::Replay {
            path: main.path().to_path_buf(),
        }))
        .unwrap();
    // Start a heavy prior build, then immediately re-source — build must die.
    handle
        .send(EngineCmd::LoadPriorSession {
            path: prior.path().to_path_buf(),
        })
        .unwrap();
    handle
        .send(EngineCmd::SetSource(Source::Replay {
            path: main2.path().to_path_buf(),
        }))
        .unwrap();
    handle.send(EngineCmd::SetSpeed(1_000_000.0)).unwrap();
    handle.send(EngineCmd::Play).unwrap();

    wait_until(Duration::from_secs(5), || {
        handle.snapshots().load().coverage.events_applied == 80
    });
    // Give residual prior slices a chance to complete if the drop failed.
    std::thread::sleep(Duration::from_millis(100));

    let snap = handle.snapshots().load();
    assert_eq!(
        snap.profile.sessions.len(),
        1,
        "dropped mid-build prior must never appear"
    );
    assert_eq!(snap.profile.sessions[0].trade_date, TRADE_DATE);

    let exit = handle.shutdown().expect("join");
    assert_eq!(exit.priors_completed, 0);
}

#[test]
fn completed_prior_survives_set_source_same_trade_date() {
    let main = temp_path("prior-keep-main");
    let main2 = temp_path("prior-keep-main2");
    let prior = temp_path("prior-keep-day");
    write_checkpointed_log(main.path(), 100, 50);
    write_checkpointed_log(main2.path(), 100, 50);
    write_checkpointed_log_for(prior.path(), PRIOR_TRADE_DATE, 80, 40);
    let expected_prior_vol = offline_profile_volume(prior.path());

    let wakes = Arc::new(AtomicU64::new(0));
    let handle = spawn_engine(wakes);
    handle
        .send(EngineCmd::SetSource(Source::Replay {
            path: main.path().to_path_buf(),
        }))
        .unwrap();
    handle
        .send(EngineCmd::LoadPriorSession {
            path: prior.path().to_path_buf(),
        })
        .unwrap();

    // Wait for the prior to complete and publish.
    wait_until(Duration::from_secs(10), || {
        handle.snapshots().load().profile.sessions.len() == 2
    });
    assert_eq!(
        session_volume(&handle.snapshots().load(), PRIOR_TRADE_DATE),
        Some(expected_prior_vol)
    );

    // Same trade date: completed prior must survive.
    handle
        .send(EngineCmd::SetSource(Source::Replay {
            path: main2.path().to_path_buf(),
        }))
        .unwrap();
    // SetSource publishes nothing until forward work; wait for a post-switch
    // publication via Play, then check retained priors.
    handle.send(EngineCmd::SetSpeed(1_000_000.0)).unwrap();
    handle.send(EngineCmd::Play).unwrap();
    wait_until(Duration::from_secs(5), || {
        handle.snapshots().load().coverage.events_applied == 100
    });

    let snap = handle.snapshots().load();
    assert_eq!(snap.profile.sessions.len(), 2);
    assert_eq!(snap.profile.sessions[0].trade_date, PRIOR_TRADE_DATE);
    assert_eq!(snap.profile.sessions[1].trade_date, TRADE_DATE);
    assert_eq!(
        session_volume(&snap, PRIOR_TRADE_DATE),
        Some(expected_prior_vol)
    );

    let exit = handle.shutdown().expect("join");
    assert_eq!(exit.priors_completed, 1);
}

#[test]
fn no_partial_prior_publication_volume_matches_offline() {
    let main = temp_path("prior-partial-main");
    let prior = temp_path("prior-partial-day");
    write_checkpointed_log(main.path(), 60, 30);
    // Medium prior: several slices, easy to sample mid-build.
    write_checkpointed_log_for(prior.path(), PRIOR_TRADE_DATE, 15_000, 2_000);
    let expected_prior_vol = offline_profile_volume(prior.path());
    assert!(expected_prior_vol > 0);

    let wakes = Arc::new(AtomicU64::new(0));
    let handle = spawn_engine(wakes);
    handle
        .send(EngineCmd::SetSource(Source::Replay {
            path: main.path().to_path_buf(),
        }))
        .unwrap();
    // Pause so forward work does not dominate sampling; prior still advances.
    handle
        .send(EngineCmd::LoadPriorSession {
            path: prior.path().to_path_buf(),
        })
        .unwrap();

    let mut saw_absent = false;
    let mut complete_vol = None;
    wait_until(Duration::from_secs(15), || {
        let snap = handle.snapshots().load();
        match session_volume(&snap, PRIOR_TRADE_DATE) {
            None => {
                saw_absent = true;
                false
            }
            Some(vol) => {
                // Complete-or-invisible: the first time a prior appears it must
                // already match the offline full-apply volume — never a partial.
                assert_eq!(
                    vol, expected_prior_vol,
                    "partial prior publication: got {vol}, expected {expected_prior_vol}"
                );
                complete_vol = Some(vol);
                true
            }
        }
    });
    assert!(
        saw_absent,
        "expected at least one snapshot without the prior (complete-or-invisible)"
    );
    assert_eq!(complete_vol, Some(expected_prior_vol));

    let exit = handle.shutdown().expect("join");
    assert_eq!(exit.priors_completed, 1);
    assert_eq!(exit.prior_skips, 0);
}
