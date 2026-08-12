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
use std::time::{Duration, Instant};

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
    /// Absolute wall deadline for the next CHECKPOINT (`create_now + 60s`, then `+= 60s`).
    next_checkpoint_wall: Instant,
}

impl LiveLog {
    pub(crate) fn create(path: &Path, meta: &InstrumentMeta, now: Instant) -> Self {
        let writer = LogWriter::create(path, meta)
            .unwrap_or_else(|e| panic!("fft-engine live-log create {}: {e}", path.display()));
        Self {
            writer,
            batch: Vec::with_capacity(EVENT_BATCH_SIZE),
            pending_logged_seq: 0,
            pending_gap_reanchor: false,
            next_checkpoint_wall: now + Duration::from_nanos(CHECKPOINT_EVENT_CADENCE_NS),
        }
    }

    /// Buffer one tip-advancing event. `logged_seq` advances only when this
    /// call commits a flush (§5.4 / §7).
    pub(crate) fn append(
        &mut self,
        event: &CanonicalEvent,
        book: &Book,
        profile: &MultiProfile,
        now: Instant,
    ) -> LiveLogCommit {
        if event.kind == fft_core::EventKind::Gap {
            // Commit the pre-gap frontier separately. Otherwise a flush containing
            // both sides of a Gap loses which sequence the re-anchor governs.
            let before_gap = self.flush_batch();
            self.pending_gap_reanchor = true;
            self.batch.push(*event);
            let gap_commit = self.flush_batch();
            if now >= self.next_checkpoint_wall {
                write_state_checkpoint(&mut self.writer, book, profile)
                    .unwrap_or_else(|e| panic!("fft-engine live-log checkpoint: {e}"));
                self.next_checkpoint_wall += Duration::from_nanos(CHECKPOINT_EVENT_CADENCE_NS);
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
        if now >= self.next_checkpoint_wall {
            let checkpoint_commit = self.flush_batch();
            if checkpoint_commit.committed_logged_seq.is_some() {
                commit.committed_logged_seq = checkpoint_commit.committed_logged_seq;
            }
            commit.gap_reanchor |= checkpoint_commit.gap_reanchor;
            write_state_checkpoint(&mut self.writer, book, profile)
                .unwrap_or_else(|e| panic!("fft-engine live-log checkpoint: {e}"));
            self.next_checkpoint_wall += Duration::from_nanos(CHECKPOINT_EVENT_CADENCE_NS);
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

#[cfg(test)]
mod tests {
    use super::*;
    use fft_core::{CanonicalEvent, EventKind, OrderId, Price, Seq, Side, Ts};
    use fft_log::{KIND_CHECKPOINT, KIND_EVENTS, LogReader};
    use fft_profile::MultiProfile;
    use std::sync::atomic::{AtomicU64, Ordering};

    const TICK: i64 = 250_000_000;
    const TRADE_DATE: u32 = 20_663;
    const DAY_S: u64 = 86_400;
    const SESSION_OPEN_NS: u64 = (20_662 * DAY_S + 22 * 3_600) * 1_000_000_000;
    const CADENCE: Duration = Duration::from_nanos(CHECKPOINT_EVENT_CADENCE_NS);

    fn temp_path(name: &str) -> std::path::PathBuf {
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        std::env::temp_dir().join(format!(
            "fft-engine-live-log-{}-{n}-{name}.fftlog",
            std::process::id()
        ))
    }

    fn es_meta() -> InstrumentMeta {
        InstrumentMeta {
            symbol: "ESU6".into(),
            instrument_id: 42,
            dataset: "GLBX.MDP3".into(),
            min_price_increment: Price(TICK),
            unit_of_measure_qty: 50_000_000_000,
            display_factor: 1,
            trade_date: TRADE_DATE,
            session_open: Ts(SESSION_OPEN_NS),
        }
    }

    fn add(seq: u32, ts: u64) -> CanonicalEvent {
        CanonicalEvent {
            kind: EventKind::Add,
            side: Side::Bid,
            flags: 0,
            size: 1,
            ts: Ts(ts),
            seq: Seq(seq),
            price: Price(20_000 * TICK),
            order_id: OrderId(u64::from(seq)),
        }
    }

    struct Fixture {
        path: std::path::PathBuf,
        log: LiveLog,
        book: Book,
        profile: MultiProfile,
        t0: Instant,
    }

    impl Fixture {
        fn new(name: &str) -> Self {
            let path = temp_path(name);
            let meta = es_meta();
            let t0 = Instant::now();
            let log = LiveLog::create(&path, &meta, t0);
            let book = Book::new(meta.min_price_increment);
            let mut profile = MultiProfile::new(meta.min_price_increment);
            profile.begin_session(meta.trade_date);
            Self {
                path,
                log,
                book,
                profile,
                t0,
            }
        }

        fn append(&mut self, event: &CanonicalEvent, now: Instant) {
            let _ = self.log.append(event, &self.book, &self.profile, now);
        }

        fn checkpoint_count_after_close(self) -> (usize, Vec<u8>) {
            let path = self.path;
            let _ = self.log.close();
            let (reader, _) = LogReader::open(&path).expect("open live log");
            let kinds: Vec<u8> = reader.index().iter().map(|e| e.kind).collect();
            let checkpoints = kinds.iter().filter(|&&k| k == KIND_CHECKPOINT).count();
            let _ = std::fs::remove_file(&path);
            (checkpoints, kinds)
        }
    }

    #[test]
    fn no_checkpoint_before_60s() {
        let mut fx = Fixture::new("before");
        fx.append(
            &add(1, SESSION_OPEN_NS),
            fx.t0 + CADENCE - Duration::from_nanos(1),
        );
        let (checkpoints, _) = fx.checkpoint_count_after_close();
        assert_eq!(checkpoints, 0);
    }

    #[test]
    fn checkpoint_at_60s_boundary() {
        let mut fx = Fixture::new("boundary");
        fx.append(&add(1, SESSION_OPEN_NS), fx.t0 + CADENCE);
        let (checkpoints, _) = fx.checkpoint_count_after_close();
        assert_eq!(checkpoints, 1);
    }

    #[test]
    fn absolute_anchor_across_delayed_boundary() {
        let mut fx = Fixture::new("anchor");
        // First deadline is t0+60s; service it late at t0+90s.
        fx.append(&add(1, SESSION_OPEN_NS), fx.t0 + Duration::from_secs(90));
        // Absolute next is (t0+60)+60 = t0+120, not now+60 = t0+150.
        fx.append(
            &add(2, SESSION_OPEN_NS + 1),
            fx.t0 + Duration::from_secs(119),
        );
        let (checkpoints, _) = fx.checkpoint_count_after_close();
        assert_eq!(checkpoints, 1);

        let mut fx = Fixture::new("anchor-fire");
        fx.append(&add(1, SESSION_OPEN_NS), fx.t0 + Duration::from_secs(90));
        fx.append(
            &add(2, SESSION_OPEN_NS + 1),
            fx.t0 + Duration::from_secs(120),
        );
        let (checkpoints, _) = fx.checkpoint_count_after_close();
        assert_eq!(checkpoints, 2);
    }

    #[test]
    fn pending_events_flush_before_checkpoint() {
        let mut fx = Fixture::new("flush-order");
        fx.append(&add(1, SESSION_OPEN_NS), fx.t0);
        fx.append(&add(2, SESSION_OPEN_NS + 1), fx.t0 + CADENCE);
        let (checkpoints, kinds) = fx.checkpoint_count_after_close();
        assert_eq!(checkpoints, 1);
        assert_eq!(kinds, vec![KIND_EVENTS, KIND_CHECKPOINT]);
    }
}
