use crate::{ReplayError, Result};
use fft_book::{
    BOOK_SECTION_ID, BOOK_SECTION_VERSION, Book, FLOW_SECTION_ID, FLOW_SECTION_VERSION,
    REFRESH_SECTION_ID, REFRESH_SECTION_VERSION,
};
use fft_core::{CanonicalEvent, InstrumentMeta};
use fft_log::{KIND_CHECKPOINT, LogReader, OpenReport, Section};
use fft_profile::{
    CVD_SECTION_ID, CVD_SECTION_VERSION, MultiProfile, PROFILE_SECTION_ID, PROFILE_SECTION_VERSION,
};
use std::path::Path;
use std::time::{Duration, Instant};

const CANCEL_BUDGET: Duration = Duration::from_millis(5);

/// Result of applying forward events.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ForwardProgress {
    /// Number of canonical events applied.
    pub events: u64,
    /// Last applied source sequence, retaining the prior value across a Gap.
    pub applied_seq: u64,
    /// Event timestamp of the last applied event.
    pub applied_ts: u64,
    /// True when the cursor reached end of file.
    pub eof: bool,
}

/// Outcome of a checkpoint restore plus tail replay.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SeekReport {
    /// Target event timestamp.
    pub target_ts: u64,
    /// Checkpoint frame selected, or `None` when replay started at log open.
    pub checkpoint_frame: Option<usize>,
    /// Loud report bit for the no-preceding-checkpoint path.
    pub replayed_from_start: bool,
    /// Tail events applied after restoration.
    pub tail_events: u64,
    /// Last applied source sequence.
    pub applied_seq: u64,
    /// Last applied event timestamp.
    pub applied_ts: u64,
    /// True when a newer seek cancelled this operation.
    pub cancelled: bool,
}

/// An mmap-backed fftlog source and its single forward cursor.
pub struct ReplaySource {
    reader: LogReader,
    open_report: OpenReport,
    frame: usize,
    frame_events: Vec<CanonicalEvent>,
    event: usize,
    applied_seq: u64,
    applied_ts: u64,
}

impl ReplaySource {
    /// Open a log. Recovery and index-rebuild warnings remain visible through
    /// [`ReplaySource::open_report`].
    pub fn open(path: &Path) -> Result<Self> {
        let (reader, open_report) = LogReader::open(path)?;
        Ok(Self {
            reader,
            open_report,
            frame: 0,
            frame_events: Vec::new(),
            event: 0,
            applied_seq: 0,
            applied_ts: 0,
        })
    }

    /// Instrument metadata from the log header.
    pub fn meta(&self) -> &InstrumentMeta {
        self.reader.meta()
    }

    /// Log-open recovery/index report. Callers must surface its warnings.
    pub fn open_report(&self) -> &OpenReport {
        &self.open_report
    }

    /// Number of indexed committed frames.
    pub fn frame_count(&self) -> usize {
        self.reader.frame_count()
    }

    /// Last source sequence applied by this cursor.
    pub fn applied_seq(&self) -> u64 {
        self.applied_seq
    }

    /// Last event timestamp applied by this cursor.
    pub fn applied_ts(&self) -> u64 {
        self.applied_ts
    }

    fn load_frame(&mut self) -> Result<bool> {
        while self.frame < self.reader.frame_count() {
            let frame = self.frame;
            self.frame += 1;
            self.frame_events = self
                .reader
                .events(frame..frame + 1)
                .collect::<std::result::Result<Vec<_>, _>>()?;
            self.event = 0;
            if !self.frame_events.is_empty() {
                return Ok(true);
            }
        }
        Ok(false)
    }

    fn ensure_event(&mut self) -> Result<bool> {
        if self.event < self.frame_events.len() {
            return Ok(true);
        }
        self.load_frame()
    }

    /// Peek at the next event without advancing the cursor.
    pub fn peek_event(&mut self) -> Result<Option<CanonicalEvent>> {
        Ok(self.ensure_event()?.then(|| self.frame_events[self.event]))
    }

    /// Apply the next event to book and profile, returning `None` at EOF.
    pub fn apply_next(
        &mut self,
        book: &mut Book,
        profile: &mut MultiProfile,
    ) -> Result<Option<CanonicalEvent>> {
        let Some(event) = self.peek_event()? else {
            return Ok(None);
        };
        self.event += 1;
        apply(event, book, profile);
        if event.seq.0 != 0 {
            self.applied_seq = u64::from(event.seq.0);
        }
        self.applied_ts = event.ts.0;
        Ok(Some(event))
    }

    /// Apply up to `max_events`, stopping once `work_budget` elapses.
    pub fn apply_forward(
        &mut self,
        book: &mut Book,
        profile: &mut MultiProfile,
        max_events: usize,
        work_budget: Duration,
    ) -> Result<ForwardProgress> {
        let start = Instant::now();
        let mut events = 0u64;
        while events < max_events as u64 && start.elapsed() < work_budget {
            if self.apply_next(book, profile)?.is_none() {
                return Ok(ForwardProgress {
                    events,
                    applied_seq: self.applied_seq,
                    applied_ts: self.applied_ts,
                    eof: true,
                });
            }
            events += 1;
        }
        Ok(ForwardProgress {
            events,
            applied_seq: self.applied_seq,
            applied_ts: self.applied_ts,
            eof: self.peek_event()?.is_none(),
        })
    }

    /// Restore the nearest checkpoint at-or-before `target_ts`, then apply
    /// tail events through the target. `cancel` is polled between bounded
    /// batches and cancellation leaves no publishable result.
    pub fn seek(
        &mut self,
        target_ts: u64,
        book: &mut Book,
        profile: &mut MultiProfile,
        mut cancel: impl FnMut() -> bool,
    ) -> Result<SeekReport> {
        let checkpoint = self.nearest_checkpoint(target_ts);
        if let Some(frame) = checkpoint {
            let sections = self.reader.read_checkpoint(frame)?;
            (*book, *profile) = restore_sections(&sections)?;
            book.check_invariants();
            let header = self.reader.frame_header(frame)?;
            self.applied_seq = header.last_seq;
            self.applied_ts = header.last_ts;
            self.reset_cursor(frame + 1);
        } else {
            *book = Book::new(self.reader.meta().min_price_increment);
            *profile = initial_profile(self.reader.meta());
            self.applied_seq = 0;
            self.applied_ts = 0;
            self.reset_cursor(0);
        }

        let mut tail_events = 0u64;
        let mut batch_start = Instant::now();
        loop {
            if cancel() {
                return Ok(self.seek_report(target_ts, checkpoint, tail_events, true));
            }
            let Some(next) = self.peek_event()? else {
                break;
            };
            if next.ts.0 > target_ts {
                break;
            }
            self.apply_next(book, profile)?;
            tail_events += 1;
            if batch_start.elapsed() >= CANCEL_BUDGET || tail_events.is_multiple_of(256) {
                if cancel() {
                    return Ok(self.seek_report(target_ts, checkpoint, tail_events, true));
                }
                batch_start = Instant::now();
            }
        }
        book.check_invariants();
        Ok(self.seek_report(target_ts, checkpoint, tail_events, false))
    }

    fn seek_report(
        &self,
        target_ts: u64,
        checkpoint: Option<usize>,
        tail_events: u64,
        cancelled: bool,
    ) -> SeekReport {
        SeekReport {
            target_ts,
            checkpoint_frame: checkpoint,
            replayed_from_start: checkpoint.is_none(),
            tail_events,
            applied_seq: self.applied_seq,
            applied_ts: self.applied_ts,
            cancelled,
        }
    }

    fn nearest_checkpoint(&self, target_ts: u64) -> Option<usize> {
        self.reader
            .index()
            .iter()
            .enumerate()
            .rev()
            .find(|(_, entry)| entry.kind == KIND_CHECKPOINT && entry.first_ts <= target_ts)
            .map(|(index, _)| index)
    }

    fn reset_cursor(&mut self, frame: usize) {
        self.frame = frame;
        self.frame_events.clear();
        self.event = 0;
    }
}

fn initial_profile(meta: &InstrumentMeta) -> MultiProfile {
    let mut profile = MultiProfile::new(meta.min_price_increment);
    profile.begin_session(meta.trade_date);
    profile
}

fn apply(event: CanonicalEvent, book: &mut Book, profile: &mut MultiProfile) {
    book.apply(&event);
    profile.apply(&event);
}

fn required<'a>(sections: &'a [Section], id: u16, name: &'static str) -> Result<&'a Section> {
    sections
        .iter()
        .find(|section| section.id == id)
        .ok_or(ReplayError::MissingSection(name))
}

fn restore_sections(sections: &[Section]) -> Result<(Book, MultiProfile)> {
    let book = required(sections, BOOK_SECTION_ID, "BOOK")?;
    if book.version != BOOK_SECTION_VERSION {
        return Err(ReplayError::SectionVersion {
            section: "BOOK",
            found: book.version,
            expected: BOOK_SECTION_VERSION,
        });
    }
    let flow = required(sections, FLOW_SECTION_ID, "FLOW")?;
    if flow.version != FLOW_SECTION_VERSION {
        return Err(ReplayError::SectionVersion {
            section: "FLOW",
            found: flow.version,
            expected: FLOW_SECTION_VERSION,
        });
    }
    let profile = required(sections, PROFILE_SECTION_ID, "PROFILE")?;
    let cvd = required(sections, CVD_SECTION_ID, "CVD")?;
    let refresh = required(sections, REFRESH_SECTION_ID, "REFRESH")?;
    if profile.version != PROFILE_SECTION_VERSION {
        return Err(ReplayError::SectionVersion {
            section: "PROFILE",
            found: profile.version,
            expected: PROFILE_SECTION_VERSION,
        });
    }
    if cvd.version != CVD_SECTION_VERSION {
        return Err(ReplayError::SectionVersion {
            section: "CVD",
            found: cvd.version,
            expected: CVD_SECTION_VERSION,
        });
    }
    if refresh.version != REFRESH_SECTION_VERSION {
        return Err(ReplayError::SectionVersion {
            section: "REFRESH",
            found: refresh.version,
            expected: REFRESH_SECTION_VERSION,
        });
    }
    Ok((
        Book::restore(&book.bytes, &flow.bytes, &refresh.bytes)?,
        MultiProfile::restore(profile.version, &profile.bytes, cvd.version, &cvd.bytes)?,
    ))
}
