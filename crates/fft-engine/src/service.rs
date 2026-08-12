use crate::command::EngineCmd;
use crate::runtime::Runtime;
use crate::snapshot::{CoverageCounters, RenderSnapshot, SnapshotSlot};
use crate::watermarks::Watermarks;
use std::fmt;
use std::sync::Arc;
use std::sync::mpsc::{self, SyncSender};
use std::thread::{self, JoinHandle};

const COMMAND_CAPACITY: usize = 64;

/// Engine startup configuration.
#[derive(Debug, Clone)]
pub struct EngineConfig {
    /// Number of contiguous ticks carried in each DOM snapshot.
    pub visible_tick_span: usize,
}

impl Default for EngineConfig {
    fn default() -> Self {
        Self {
            visible_tick_span: 512,
        }
    }
}

/// Clean engine-thread result returned after shutdown.
#[derive(Debug)]
pub struct EngineExit {
    /// Final deterministic BOOK bytes, when a source was loaded.
    pub book_bytes: Option<Vec<u8>>,
    /// Final FLOW section bytes.
    pub flow_bytes: Option<Vec<u8>>,
    /// Final REFRESH section bytes.
    pub refresh_bytes: Option<Vec<u8>>,
    /// Final deterministic PROFILE/CVD/SESSION section payloads.
    pub profile_bytes: Option<fft_profile::ProfileSections>,
    /// Final integrity watermarks.
    pub watermarks: Watermarks,
    /// Number of snapshots published.
    pub publications: u64,
    /// Number of non-cancelled seeks executed.
    pub seeks_executed: u64,
    /// Final event-coverage counters.
    pub coverage: CoverageCounters,
    /// Visible log-open warnings encountered while switching sources.
    pub source_warnings: Vec<String>,
    /// Prior-session loads skipped with a loud stderr report.
    pub prior_skips: u64,
    /// Prior sessions completed and inserted into the profile.
    pub priors_completed: u64,
}

/// API misuse before a command reaches the engine.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EngineStateError {
    /// The engine receiver has already terminated.
    Stopped,
    /// The engine join handle was already consumed.
    AlreadyJoined,
}

impl fmt::Display for EngineStateError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Stopped => write!(f, "fft-engine command receiver stopped"),
            Self::AlreadyJoined => write!(f, "fft-engine thread already joined"),
        }
    }
}

impl std::error::Error for EngineStateError {}

/// Running engine control handle.
pub struct EngineHandle {
    tx: SyncSender<EngineCmd>,
    snapshots: SnapshotSlot,
    join: Option<JoinHandle<EngineExit>>,
}

impl EngineHandle {
    /// Bounded command sender. `send` blocks when all 64 slots are occupied.
    pub fn command_sender(&self) -> SyncSender<EngineCmd> {
        self.tx.clone()
    }

    /// Latest coherent render slot.
    pub fn snapshots(&self) -> SnapshotSlot {
        self.snapshots.clone()
    }

    /// Send one command on the bounded protocol channel.
    pub fn send(&self, command: EngineCmd) -> Result<(), EngineStateError> {
        self.tx.send(command).map_err(|_| EngineStateError::Stopped)
    }

    /// Send shutdown and join the dedicated thread.
    pub fn shutdown(mut self) -> thread::Result<EngineExit> {
        let _ = self.send(EngineCmd::Shutdown);
        self.join
            .take()
            .expect("fft-engine join handle missing")
            .join()
    }

    /// Join after the caller has already sent [`EngineCmd::Shutdown`].
    pub fn join(&mut self) -> Result<thread::Result<EngineExit>, EngineStateError> {
        self.join
            .take()
            .map(JoinHandle::join)
            .ok_or(EngineStateError::AlreadyJoined)
    }
}

/// Factory for the dedicated single-writer service.
pub struct EngineService;

impl EngineService {
    /// Spawn the named `fft-engine` OS thread. `wake` is payloadless and runs
    /// at most once for each publication.
    pub fn spawn(
        config: EngineConfig,
        wake: Box<dyn Fn() + Send>,
    ) -> std::io::Result<EngineHandle> {
        assert!(
            config.visible_tick_span > 0,
            "engine visible tick span must be non-zero"
        );
        let (tx, rx) = mpsc::sync_channel(COMMAND_CAPACITY);
        let snapshots = SnapshotSlot::new(Arc::new(RenderSnapshot::default()));
        let thread_slot = snapshots.clone();
        let join = thread::Builder::new()
            .name("fft-engine".into())
            .spawn(move || Runtime::new(config, thread_slot, wake).run(rx))?;
        Ok(EngineHandle {
            tx,
            snapshots,
            join: Some(join),
        })
    }
}
