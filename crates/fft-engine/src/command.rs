use std::path::PathBuf;

/// Live-source configuration reserved for M6.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LiveConfig {
    /// Operator-visible source name.
    pub name: String,
}

/// Engine input source.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Source {
    /// Replay an fftlog v2 file.
    Replay {
        /// Path to the log.
        path: PathBuf,
    },
    /// Recorded-week stand-in for Databento live (`docs/ENGINE.md` §5).
    ///
    /// Join = unpaced catch-up from session open to `head_ts`, then absolute
    /// wall-pin streaming. Every applied event is appended to `live_out`
    /// (LIVE-flagged fftlog v2, §7 commit protocol).
    SimLive {
        /// Path to the recorded (usually checkpointed) source log.
        path: PathBuf,
        /// Stream-head event timestamp, nanoseconds UTC, must exist in-log.
        head_ts: u64,
        /// Destination for the continuously appended LIVE log (clause 4).
        /// Additive to the §5 freeze listing — required by the M1.5 gate's
        /// `--live-out` and by the live-append contract; same-commit doc edit.
        live_out: PathBuf,
    },
    /// Consume a live source. Connection wiring lands in M6.
    Live {
        /// Live connection configuration.
        config: LiveConfig,
    },
}

/// The complete bounded UI-to-engine command protocol.
#[derive(Debug)]
pub enum EngineCmd {
    /// Replace the active replay or live source.
    SetSource(Source),
    /// Async profile-only build of an earlier trade date (`docs/ENGINE.md` §2).
    /// Path to a prior-day fftlog; the engine inserts the completed session into
    /// `ProfileRenderState.sessions` keeping ascending trade-date order.
    LoadPriorSession {
        /// Path to the prior-day fftlog.
        path: PathBuf,
    },
    /// Start event-time-paced replay.
    Play,
    /// Pause replay.
    Pause,
    /// Set replay speed; must be finite and greater than zero.
    SetSpeed(f64),
    /// Seek to an event timestamp using a monotonic UI generation.
    Seek {
        /// Nanoseconds UTC.
        ts: u64,
        /// Monotonic latest-wins generation.
        generation: u64,
    },
    /// Jump a live source to its current head.
    GoLive,
    /// Flush owned resources and terminate the engine thread.
    Shutdown,
}
