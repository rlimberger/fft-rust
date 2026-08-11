//! Replay engine spawn (SetSource / Seek / Play / priors). Split from `shell` for size.

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;

use fft_engine::{EngineCmd, EngineConfig, EngineHandle, EngineService, SnapshotSlot, Source};

use crate::prior_discovery::{PriorOptions, auto_ingest_missing, discover_prior_sessions};

pub(crate) fn spawn_replay_engine(
    path: PathBuf,
    replay_at: Option<u64>,
    prior_sessions: &[PathBuf],
    prior_options: PriorOptions,
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
    let discovery_path = path.clone();
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
    // ENGINE.md §2: one LoadPriorSession per explicit prior, oldest-first (CLI order).
    for prior in prior_sessions {
        handle
            .send(EngineCmd::LoadPriorSession {
                path: prior.clone(),
            })
            .unwrap_or_else(|err| panic!("fft: LoadPriorSession failed: {err}"));
    }
    if prior_options.discover {
        let explicit = prior_sessions.to_vec();
        let tx = handle.command_sender();
        thread::Builder::new()
            .name("fft-prior-discovery".into())
            .spawn(move || {
                let Some(discovery) = discover_prior_sessions(&discovery_path, &explicit) else {
                    return;
                };
                for prior in &discovery.sessions {
                    let (year, month, day) =
                        crate::datetime::civil_from_days(i64::from(prior.trade_date));
                    eprintln!(
                        "fft: discovered prior session {year:04}-{month:02}-{day:02} at {}",
                        prior.path.display()
                    );
                    if tx
                        .send(EngineCmd::LoadPriorSession {
                            path: prior.path.clone(),
                        })
                        .is_err()
                    {
                        return;
                    }
                }
                if prior_options.auto_ingest {
                    auto_ingest_missing(
                        &discovery,
                        prior_options.dbn_dir.as_deref(),
                        |prior, _events| {
                            let (year, month, day) =
                                crate::datetime::civil_from_days(i64::from(prior.trade_date));
                            eprintln!(
                                "fft: discovered prior session {year:04}-{month:02}-{day:02} at {}",
                                prior.path.display()
                            );
                            let _ = tx.send(EngineCmd::LoadPriorSession { path: prior.path });
                        },
                    );
                }
            })
            .unwrap_or_else(|err| panic!("fft: failed to spawn prior discovery thread: {err}"));
    }
    let snapshots = handle.snapshots();
    (handle, snapshots, wake_dirty)
}
