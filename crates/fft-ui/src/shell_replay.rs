//! Replay / sim-live engine spawn (SetSource / Seek / Play / priors).
//! Split from `shell` for size.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;

use fft_engine::{EngineCmd, EngineConfig, EngineHandle, EngineService, SnapshotSlot, Source};
use fft_replay::ReplaySource;

use crate::prior_discovery::{PriorOptions, auto_ingest_missing, discover_prior_sessions};

/// Startup feed chosen by the CLI (`--replay` / `--sim-live` / none).
#[derive(Debug, Clone)]
pub enum StartupSource {
    /// Blank window — no engine thread.
    None,
    /// Historical replay; optional `--replay-at` seek before Play.
    Replay {
        path: PathBuf,
        replay_at: Option<u64>,
    },
    /// Sim-live join: wall-clock head snapped to an exact in-log event ts.
    SimLive {
        path: PathBuf,
        head_ts: u64,
        live_out: PathBuf,
    },
}

impl StartupSource {
    /// Path recorded in gate `RunMeta.replay` (sim-live uses the source log path).
    pub fn meta_path(&self) -> Option<PathBuf> {
        match self {
            Self::None => None,
            Self::Replay { path, .. } | Self::SimLive { path, .. } => Some(path.clone()),
        }
    }

    /// True when an engine thread will be requested after first paint.
    pub fn starts_engine(&self) -> bool {
        !matches!(self, Self::None)
    }
}

/// Open `path` and snap a wall-clock head to the last in-log event ts ≤ `head_ts`.
///
/// Engine `SetSource(SimLive)` requires an exact event timestamp (ENGINE.md §5);
/// wall-clock CLI heads are a harness convenience and must snap before SetSource.
/// Fallible form for tests; CLI path stays loud via [`snap_sim_live_head`].
pub(crate) fn snap_sim_live_head_result(path: &Path, head_ts: u64) -> Result<u64, String> {
    let mut source = ReplaySource::open(path).map_err(|err| {
        format!(
            "failed to open --sim-live log for head snap ({}): {err}",
            path.display()
        )
    })?;
    let mut snapped = None;
    loop {
        match source.next_event() {
            Ok(Some(event)) => {
                if event.ts.0 <= head_ts {
                    snapped = Some(event.ts.0);
                } else {
                    break;
                }
            }
            Ok(None) => break,
            Err(err) => {
                return Err(format!(
                    "failed reading --sim-live log for head snap ({}): {err}",
                    path.display()
                ));
            }
        }
    }
    snapped.ok_or_else(|| {
        format!(
            "--head {head_ts} has no in-log event at or before that timestamp in {}",
            path.display()
        )
    })
}

/// CLI wrapper: same snap as [`snap_sim_live_head_result`], exits 1 on error.
pub(crate) fn snap_sim_live_head(path: &Path, head_ts: u64) -> u64 {
    snap_sim_live_head_result(path, head_ts).unwrap_or_else(|err| {
        eprintln!("fft: {err}");
        std::process::exit(1);
    })
}

fn spawn_engine_thread(wake_dirty: Arc<AtomicBool>) -> EngineHandle {
    let wake = Arc::clone(&wake_dirty);
    EngineService::spawn(
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
    })
}

pub(crate) fn spawn_replay_engine(
    path: PathBuf,
    replay_at: Option<u64>,
    prior_sessions: &[PathBuf],
    prior_options: PriorOptions,
    speed: f64,
) -> (EngineHandle, SnapshotSlot, Arc<AtomicBool>) {
    let wake_dirty = Arc::new(AtomicBool::new(false));
    let handle = spawn_engine_thread(Arc::clone(&wake_dirty));
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

/// Spawn sim-live without blocking the UI thread on head snap I/O.
///
/// Engine thread is installed immediately; a dedicated OS worker snaps the wall
/// head and issues `SetSource(SimLive)` (+ optional `SetSpeed`). No Seek/Play —
/// the engine starts playing on SetSource (ENGINE.md §5).
pub(crate) fn spawn_sim_live_engine(
    path: PathBuf,
    head_ts: u64,
    live_out: PathBuf,
    speed: f64,
) -> (EngineHandle, SnapshotSlot, Arc<AtomicBool>) {
    let wake_dirty = Arc::new(AtomicBool::new(false));
    let handle = spawn_engine_thread(Arc::clone(&wake_dirty));
    let snapshots = handle.snapshots();
    let tx = handle.command_sender();
    thread::Builder::new()
        .name("fft-sim-live-snap".into())
        .spawn(move || {
            // Full-log scan stays off the GPUI thread (doctrine: UI never blocks on I/O).
            let head_ts = snap_sim_live_head(&path, head_ts);
            if tx
                .send(EngineCmd::SetSource(Source::SimLive {
                    path,
                    head_ts,
                    live_out,
                }))
                .is_err()
            {
                eprintln!("fft: SetSource(SimLive) failed: engine stopped");
                std::process::exit(1);
            }
            if (speed - 1.0).abs() > f64::EPSILON && tx.send(EngineCmd::SetSpeed(speed)).is_err() {
                eprintln!("fft: SetSpeed failed: engine stopped");
                std::process::exit(1);
            }
        })
        .unwrap_or_else(|err| {
            eprintln!("fft: failed to spawn sim-live head-snap thread: {err}");
            std::process::exit(1);
        });
    (handle, snapshots, wake_dirty)
}

#[cfg(test)]
#[path = "shell_replay_tests.rs"]
mod tests;
