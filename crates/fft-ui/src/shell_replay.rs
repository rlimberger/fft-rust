//! Replay engine spawn (SetSource / Seek / Play / priors). Split from `shell` for size.

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use fft_engine::{EngineCmd, EngineConfig, EngineHandle, EngineService, SnapshotSlot, Source};

pub(crate) fn spawn_replay_engine(
    path: PathBuf,
    replay_at: Option<u64>,
    prior_sessions: &[PathBuf],
    speed: f64,
) -> (EngineHandle, SnapshotSlot, Arc<AtomicBool>) {
    let wake_dirty = Arc::new(AtomicBool::new(false));
    let wake = Arc::clone(&wake_dirty);
    let handle = EngineService::spawn(
        EngineConfig {
            visible_tick_span: 256,
        },
        Box::new(move || {
            wake.store(true, Ordering::Release);
        }),
    )
    .unwrap_or_else(|err| panic!("fft: failed to spawn engine thread: {err}"));
    handle
        .send(EngineCmd::SetSource(Source::Replay { path }))
        .unwrap_or_else(|err| panic!("fft: SetSource failed: {err}"));
    // Seek pauses; Play follows. Gen 1 is the UI's first seek after SetSource (0).
    // Transport scrub/step starts at 2 (`FIRST_UI_SEEK_GENERATION`).
    if let Some(ts) = replay_at {
        handle
            .send(EngineCmd::Seek { ts, generation: 1 })
            .unwrap_or_else(|err| panic!("fft: Seek failed: {err}"));
    }
    handle
        .send(EngineCmd::Play)
        .unwrap_or_else(|err| panic!("fft: Play failed: {err}"));
    if (speed - 1.0).abs() > f64::EPSILON {
        handle
            .send(EngineCmd::SetSpeed(speed))
            .unwrap_or_else(|err| panic!("fft: SetSpeed failed: {err}"));
    }
    // ENGINE.md §2: one LoadPriorSession per prior, oldest-first (CLI order).
    for prior in prior_sessions {
        handle
            .send(EngineCmd::LoadPriorSession {
                path: prior.clone(),
            })
            .unwrap_or_else(|err| panic!("fft: LoadPriorSession failed: {err}"));
    }
    let snapshots = handle.snapshots();
    (handle, snapshots, wake_dirty)
}
