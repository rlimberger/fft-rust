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
