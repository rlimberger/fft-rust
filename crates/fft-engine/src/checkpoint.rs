//! Offline historical checkpoint pass (`docs/ENGINE.md` §4 materialization item 2).
//!
//! Reads an ingest-produced fftlog, applies every event through the same
//! [`ReplaySource::apply_next`] path used by live replay, and writes a
//! checkpointed copy at 60 s **event-time** cadence with all six sections in
//! ascending id order.

use fft_book::{
    BOOK_SECTION_ID, BOOK_SECTION_VERSION, Book, FLOW_SECTION_ID, FLOW_SECTION_VERSION,
    REFRESH_SECTION_ID, REFRESH_SECTION_VERSION,
};
use fft_core::CanonicalEvent;
use fft_log::{LogError, LogWriter, SectionRef};
use fft_profile::{
    CVD_SECTION_ID, CVD_SECTION_VERSION, MultiProfile, PROFILE_SECTION_ID, PROFILE_SECTION_VERSION,
    SESSION_SECTION_ID, SESSION_SECTION_VERSION,
};
use fft_replay::{ReplayError, ReplaySource};
use std::fmt;
use std::path::Path;

/// 60 s event-time cadence (FFTLOG-V2 §5 / ENGINE.md §4 historical path).
pub const CHECKPOINT_EVENT_CADENCE_NS: u64 = 60 * 1_000_000_000;

const EVENT_BATCH_SIZE: usize = 8_192;

/// Summary printed by `fft-checkpoint` and returned to callers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CheckpointSummary {
    /// Canonical events copied from `src` into `dst`.
    pub events: u64,
    /// CHECKPOINT frames written into `dst`.
    pub checkpoints: u64,
    /// Byte length of `src`.
    pub src_bytes: u64,
    /// Byte length of `dst` after clean close.
    pub dst_bytes: u64,
}

/// Loud failure from the offline checkpoint pass.
#[derive(Debug)]
pub enum CheckpointError {
    /// Source open/apply failed through the shared replay path.
    Replay(ReplayError),
    /// Destination append/close failed.
    Log(LogError),
    /// Filesystem metadata / sizing failed.
    Io {
        /// Path that failed.
        path: String,
        /// Underlying OS error.
        source: std::io::Error,
    },
}

impl fmt::Display for CheckpointError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Replay(err) => write!(f, "fft-checkpoint replay: {err}"),
            Self::Log(err) => write!(f, "fft-checkpoint log: {err}"),
            Self::Io { path, source } => write!(f, "fft-checkpoint io ({path}): {source}"),
        }
    }
}

impl std::error::Error for CheckpointError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Replay(err) => Some(err),
            Self::Log(err) => Some(err),
            Self::Io { source, .. } => Some(source),
        }
    }
}

impl From<ReplayError> for CheckpointError {
    fn from(value: ReplayError) -> Self {
        Self::Replay(value)
    }
}

impl From<LogError> for CheckpointError {
    fn from(value: LogError) -> Self {
        Self::Log(value)
    }
}

fn file_len(path: &Path) -> Result<u64, CheckpointError> {
    std::fs::metadata(path)
        .map(|meta| meta.len())
        .map_err(|source| CheckpointError::Io {
            path: path.display().to_string(),
            source,
        })
}

fn write_state_checkpoint(
    writer: &mut LogWriter,
    book: &Book,
    profile: &MultiProfile,
) -> Result<(), CheckpointError> {
    let book_bytes = book.serialize_book();
    let flow_bytes = book.serialize_flow();
    let refresh_bytes = book.serialize_refresh();
    let secs = profile.serialize();
    writer.write_checkpoint([
        SectionRef {
            id: BOOK_SECTION_ID,
            version: BOOK_SECTION_VERSION,
            flags: 0,
            bytes: &book_bytes,
        },
        SectionRef {
            id: FLOW_SECTION_ID,
            version: FLOW_SECTION_VERSION,
            flags: 0,
            bytes: &flow_bytes,
        },
        SectionRef {
            id: PROFILE_SECTION_ID,
            version: PROFILE_SECTION_VERSION,
            flags: 0,
            bytes: &secs.profile,
        },
        SectionRef {
            id: CVD_SECTION_ID,
            version: CVD_SECTION_VERSION,
            flags: 0,
            bytes: &secs.cvd,
        },
        SectionRef {
            id: REFRESH_SECTION_ID,
            version: REFRESH_SECTION_VERSION,
            flags: 0,
            bytes: &refresh_bytes,
        },
        SectionRef {
            id: SESSION_SECTION_ID,
            version: SESSION_SECTION_VERSION,
            flags: 0,
            bytes: &secs.session,
        },
    ])?;
    Ok(())
}

fn flush_batch(
    writer: &mut LogWriter,
    batch: &mut Vec<CanonicalEvent>,
) -> Result<(), CheckpointError> {
    if batch.is_empty() {
        return Ok(());
    }
    writer.append_events(batch)?;
    batch.clear();
    Ok(())
}

/// Stream `src` through the shared apply path and write a checkpointed copy to `dst`.
///
/// The destination event stream is event-identical to `src` (same events, same order;
/// EVENTS frame re-batching is allowed). A CHECKPOINT frame with all six sections is
/// emitted whenever 60 s of event time has elapsed since the previous checkpoint
/// anchor (the first event, then each subsequent checkpoint's observing event).
pub fn write_checkpointed_copy(
    src: &Path,
    dst: &Path,
) -> Result<CheckpointSummary, CheckpointError> {
    let src_bytes = file_len(src)?;
    let mut source = ReplaySource::open(src)?;
    for warning in &source.open_report().warnings {
        eprintln!("fft-checkpoint: source warning: {warning}");
    }

    let meta = source.meta().clone();
    let mut book = Book::new(meta.min_price_increment);
    let mut profile = MultiProfile::new(meta.min_price_increment);
    profile.begin_session(meta.trade_date);

    let mut writer = LogWriter::create(dst, &meta)?;
    let mut batch: Vec<CanonicalEvent> = Vec::with_capacity(EVENT_BATCH_SIZE);
    let mut events = 0u64;
    let mut checkpoints = 0u64;
    // Anchor for the next cadence window: first applied event ts, then each
    // checkpoint's observing-event ts.
    let mut cadence_anchor: Option<u64> = None;

    while let Some(event) = source.apply_next(&mut book, &mut profile)? {
        events += 1;
        batch.push(event);

        let due = match cadence_anchor {
            None => {
                cadence_anchor = Some(event.ts.0);
                false
            }
            Some(anchor) => event.ts.0.saturating_sub(anchor) >= CHECKPOINT_EVENT_CADENCE_NS,
        };

        if due || batch.len() >= EVENT_BATCH_SIZE {
            flush_batch(&mut writer, &mut batch)?;
        }
        if due {
            // Flush is guaranteed above when `due` — checkpoint stamps last event ts/seq.
            write_state_checkpoint(&mut writer, &book, &profile)?;
            checkpoints += 1;
            cadence_anchor = Some(event.ts.0);
        }
    }

    flush_batch(&mut writer, &mut batch)?;
    writer.close()?;

    Ok(CheckpointSummary {
        events,
        checkpoints,
        src_bytes,
        dst_bytes: file_len(dst)?,
    })
}
