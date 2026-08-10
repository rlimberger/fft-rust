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
