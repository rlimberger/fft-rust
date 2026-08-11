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
    .unwrap_or_else(|err| {
        eprintln!("fft: failed to spawn engine thread: {err}");
        std::process::exit(1);
    });
    let discovery_path = path.clone();
    if let Err(err) = handle.send(EngineCmd::SetSource(Source::Replay { path })) {
        eprintln!("fft: SetSource failed: {err}");
        std::process::exit(1);
    }
    // Seek pauses; Play follows. Gen 1 is the UI's first seek after SetSource (0).
    // Transport scrub/step starts at 2 (`FIRST_UI_SEEK_GENERATION`).
    if let Some(ts) = replay_at
        && let Err(err) = handle.send(EngineCmd::Seek { ts, generation: 1 })
    {
        eprintln!("fft: Seek failed: {err}");
        std::process::exit(1);
    }
    if let Err(err) = handle.send(EngineCmd::Play) {
        eprintln!("fft: Play failed: {err}");
        std::process::exit(1);
    }
    if (speed - 1.0).abs() > f64::EPSILON
        && let Err(err) = handle.send(EngineCmd::SetSpeed(speed))
    {
        eprintln!("fft: SetSpeed failed: {err}");
        std::process::exit(1);
    }
    // ENGINE.md §2: one LoadPriorSession per explicit prior, oldest-first (CLI order).
    for prior in prior_sessions {
        if let Err(err) = handle.send(EngineCmd::LoadPriorSession {
            path: prior.clone(),
        }) {
            eprintln!("fft: LoadPriorSession failed: {err}");
            std::process::exit(1);
        }
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
            .map(|_handle| ())
            .unwrap_or_else(|err| {
                eprintln!(
                    "fft: WARNING failed to spawn prior discovery thread ({err}); continuing without auto-priors"
                );
            });
    }
    let snapshots = handle.snapshots();
    (handle, snapshots, wake_dirty)
}
