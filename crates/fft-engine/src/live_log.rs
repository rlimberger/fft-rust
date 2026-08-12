//! Live-log append for SimLive (`docs/ENGINE.md` §5.4 / FFTLOG-V2 §7).
//!
//! Engine-thread only. Event frames commit on flush (§7); CHECKPOINT every
//! 60 s wall-clock through the same serialize path as `checkpoint.rs`.
//! I/O errors are loud panics (doctrine rule 7).

use crate::checkpoint::{CHECKPOINT_EVENT_CADENCE_NS, write_state_checkpoint};
use fft_book::Book;
use fft_core::{CanonicalEvent, InstrumentMeta};
use fft_log::LogWriter;
use fft_profile::MultiProfile;
use std::path::Path;
use std::time::Instant;

const EVENT_BATCH_SIZE: usize = 4_096;

/// Result of buffering one tip-advancing event into the live log.
#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct LiveLogCommit {
    /// Last channel seq made durable by a flush in this call, if any.
    pub(crate) committed_logged_seq: Option<u64>,
    /// A committed Gap permits `logged_seq` to re-anchor at the committed seq.
    pub(crate) gap_reanchor: bool,
}

/// Single-writer LIVE fftlog owned by the engine thread.
pub(crate) struct LiveLog {
    writer: LogWriter,
    batch: Vec<CanonicalEvent>,
    /// Last channel seq buffered since the last committed flush (0 = none).
    pending_logged_seq: u64,
    /// Whether the buffered range contains a Gap before `pending_logged_seq`.
    pending_gap_reanchor: bool,
    last_checkpoint_wall: Instant,
}

impl LiveLog {
    pub(crate) fn create(path: &Path, meta: &InstrumentMeta) -> Self {
        let writer = LogWriter::create(path, meta)
            .unwrap_or_else(|e| panic!("fft-engine live-log create {}: {e}", path.display()));
        Self {
            writer,
            batch: Vec::with_capacity(EVENT_BATCH_SIZE),
            pending_logged_seq: 0,
            pending_gap_reanchor: false,
            last_checkpoint_wall: Instant::now(),
        }
    }

    /// Buffer one tip-advancing event. `logged_seq` advances only when this
    /// call commits a flush (§5.4 / §7).
    pub(crate) fn append(
        &mut self,
        event: &CanonicalEvent,
        book: &Book,
        profile: &MultiProfile,
    ) -> LiveLogCommit {
        if event.kind == fft_core::EventKind::Gap {
            // Commit the pre-gap frontier separately. Otherwise a flush containing
            // both sides of a Gap loses which sequence the re-anchor governs.
            let before_gap = self.flush_batch();
            self.pending_gap_reanchor = true;
            self.batch.push(*event);
            let gap_commit = self.flush_batch();
            if self.last_checkpoint_wall.elapsed().as_nanos() as u64 >= CHECKPOINT_EVENT_CADENCE_NS
            {
                write_state_checkpoint(&mut self.writer, book, profile)
                    .unwrap_or_else(|e| panic!("fft-engine live-log checkpoint: {e}"));
                self.last_checkpoint_wall = Instant::now();
            }
            return LiveLogCommit {
                committed_logged_seq: before_gap.committed_logged_seq,
                gap_reanchor: gap_commit.gap_reanchor,
            };
        }
        if event.seq.0 != 0 && !event.is_snapshot() {
            self.pending_logged_seq = u64::from(event.seq.0);
        }
        self.batch.push(*event);
        let mut commit = LiveLogCommit::default();
        if self.batch.len() >= EVENT_BATCH_SIZE {
            commit = self.flush_batch();
        }
        if self.last_checkpoint_wall.elapsed().as_nanos() as u64 >= CHECKPOINT_EVENT_CADENCE_NS {
            let checkpoint_commit = self.flush_batch();
            if checkpoint_commit.committed_logged_seq.is_some() {
                commit.committed_logged_seq = checkpoint_commit.committed_logged_seq;
            }
            commit.gap_reanchor |= checkpoint_commit.gap_reanchor;
            write_state_checkpoint(&mut self.writer, book, profile)
                .unwrap_or_else(|e| panic!("fft-engine live-log checkpoint: {e}"));
            self.last_checkpoint_wall = Instant::now();
        }
        commit
    }

    fn flush_batch(&mut self) -> LiveLogCommit {
        if self.batch.is_empty() {
            return LiveLogCommit::default();
        }
        self.writer
            .append_events(&self.batch)
            .unwrap_or_else(|e| panic!("fft-engine live-log append: {e}"));
        self.batch.clear();
        let seq = self.pending_logged_seq;
        self.pending_logged_seq = 0;
        let gap_reanchor = std::mem::take(&mut self.pending_gap_reanchor);
        LiveLogCommit {
            committed_logged_seq: (seq != 0).then_some(seq),
            gap_reanchor,
        }
    }

    /// Flush remaining events and clear LIVE (§6).
    pub(crate) fn close(mut self) -> LiveLogCommit {
        let commit = self.flush_batch();
        self.writer
            .close()
            .unwrap_or_else(|e| panic!("fft-engine live-log close: {e}"));
        commit
    }
}
