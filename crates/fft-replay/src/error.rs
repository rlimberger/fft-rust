use std::fmt;

/// File replay failure with enough context to fail loudly.
#[derive(Debug)]
pub enum ReplayError {
    /// The underlying fftlog reader rejected the file or frame.
    Log(fft_log::LogError),
    /// A required checkpoint section was absent.
    MissingSection(&'static str),
    /// A checkpoint section carried an unexpected version.
    SectionVersion {
        /// Human-readable section name.
        section: &'static str,
        /// Version found in the section header.
        found: u16,
        /// Version required by the state crate.
        expected: u16,
    },
    /// Harness splice input carried log-open recovery or rebuild warnings.
    SpliceOpenWarnings(Vec<String>),
    /// Harness splice input timestamps moved backwards.
    NonMonotonicSpliceInput {
        /// Zero-based event position in the source.
        event_index: u64,
        /// Timestamp of the preceding event.
        previous_ts: u64,
        /// Timestamp that violated monotonic order.
        observed_ts: u64,
    },
    /// No event in the copied range followed the requested splice boundary.
    SpliceAnchorNotFound {
        /// Timestamp after which the Gap was requested.
        inject_after_ts: u64,
        /// Inclusive end of the copied range.
        copy_through_ts: u64,
    },
    /// Gap sequence bounds did not describe a forward discontinuity.
    InvalidGapSequences {
        /// First missing source sequence.
        expected: u64,
        /// First source sequence observed after the discontinuity.
        observed: u64,
    },
    /// A book-owned checkpoint payload was malformed.
    BookRestore(fft_book::RestoreError),
    /// The profile checkpoint payload was malformed.
    ProfileRestore(fft_profile::RestoreError),
}

impl fmt::Display for ReplayError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Log(error) => write!(f, "fft-replay log error: {error}"),
            Self::MissingSection(section) => {
                write!(
                    f,
                    "fft-replay checkpoint missing required {section} section"
                )
            }
            Self::SectionVersion {
                section,
                found,
                expected,
            } => write!(
                f,
                "fft-replay {section} section version {found}, expected {expected}"
            ),
            Self::SpliceOpenWarnings(warnings) => write!(
                f,
                "fft-replay splice rejected source open warnings: {}",
                warnings.join("; ")
            ),
            Self::NonMonotonicSpliceInput {
                event_index,
                previous_ts,
                observed_ts,
            } => write!(
                f,
                "fft-replay splice source event {event_index} timestamp {observed_ts} is before {previous_ts}"
            ),
            Self::SpliceAnchorNotFound {
                inject_after_ts,
                copy_through_ts,
            } => write!(
                f,
                "fft-replay splice found no event after {inject_after_ts} through {copy_through_ts}"
            ),
            Self::InvalidGapSequences { expected, observed } => write!(
                f,
                "fft-replay splice Gap observed sequence {observed} must be greater than expected {expected}"
            ),
            Self::BookRestore(error) => write!(f, "fft-replay book restore: {error}"),
            Self::ProfileRestore(error) => write!(f, "fft-replay profile restore: {error}"),
        }
    }
}

impl std::error::Error for ReplayError {}

impl From<fft_log::LogError> for ReplayError {
    fn from(value: fft_log::LogError) -> Self {
        Self::Log(value)
    }
}

impl From<fft_profile::RestoreError> for ReplayError {
    fn from(value: fft_profile::RestoreError) -> Self {
        Self::ProfileRestore(value)
    }
}

impl From<fft_book::RestoreError> for ReplayError {
    fn from(value: fft_book::RestoreError) -> Self {
        Self::BookRestore(value)
    }
}

/// Replay result alias.
pub type Result<T> = std::result::Result<T, ReplayError>;
