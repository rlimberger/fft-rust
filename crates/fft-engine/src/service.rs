use crate::command::{EngineCmd, Source};
use crate::snapshot::{CoverageCounters, RenderSnapshot, SnapshotSlot, build_snapshot};
use crate::watermarks::Watermarks;
use fft_book::Book;
use fft_core::{EventKind, InstrumentMeta};
use fft_profile::{MultiProfile, SessionProfile};
use fft_replay::{ReplayError, ReplaySource};
use std::fmt;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::mpsc::{self, Receiver, SyncSender, TryRecvError};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

const COMMAND_CAPACITY: usize = 64;
const APPLY_BUDGET: Duration = Duration::from_millis(4);
/// Time-budgeted slice for one prior-session build step (doctrine rule 4: time,
/// never event counts). Interleaved with forward work so playback is never blocked.
const PRIOR_BUILD_BUDGET: Duration = Duration::from_millis(2);
const IDLE_WAIT: Duration = Duration::from_millis(10);

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
    /// Final deterministic PROFILE/CVD/SESSION section payloads.
    pub profile_bytes: Option<fft_profile::ProfileSections>,
    /// Final integrity watermarks.
    pub watermarks: Watermarks,
    /// Number of snapshots published.
    pub publications: u64,
    /// Number of non-cancelled seeks executed.
    pub seeks_executed: u64,
    /// Final event-coverage counters. The authoritative drop evidence for a run: it counts
    /// everything applied after the last publication, which the last snapshot cannot.
    pub coverage: CoverageCounters,
    /// Visible log-open warnings encountered while switching sources.
    pub source_warnings: Vec<String>,
    /// Prior-session loads that were skipped with a loud stderr report
    /// (`docs/ENGINE.md` §2 rule 3: missing file / wrong instrument / date ≥ current).
    pub prior_skips: u64,
    /// Prior sessions that completed and were inserted into the profile.
    pub priors_completed: u64,
}

/// In-progress profile-only build of an earlier trade date (`docs/ENGINE.md` §2).
/// Lives only on the engine thread; never published until complete.
struct PriorBuild {
    path: PathBuf,
    source: ReplaySource,
    /// Throwaway book used only as the apply target for stream/tail events.
    /// Prior sessions are profile-only; the book is discarded on completion.
    book: Book,
    profile: MultiProfile,
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
    ///
    /// A failed `send` means the engine thread already died (panicked): the
    /// join below returns that panic as `Err` for the caller to record.
    /// Panicking here instead would destroy gate evidence — the exact failure
    /// mode ENGINE-DEFECT-WAVE exists to prevent (evidence is written first,
    /// verdict FAIL, then the process fails).
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

struct Runtime {
    config: EngineConfig,
    snapshots: SnapshotSlot,
    wake: Box<dyn Fn() + Send>,
    source: Option<ReplaySource>,
    book: Option<Book>,
    profile: Option<MultiProfile>,
    /// Current source instrument identity, used to validate prior-session loads.
    source_meta: Option<InstrumentMeta>,
    /// Contract symbol cloned once at `SetSource` for snapshot publication.
    symbol: Arc<str>,
    /// In-progress prior-day profile build; at most one at a time.
    prior_build: Option<PriorBuild>,
    playing: bool,
    speed: f64,
    pace_event_origin: u64,
    pace_wall_origin: Instant,
    generation: u64,
    watermarks: Watermarks,
    coverage: CoverageCounters,
    applied_ts: u64,
    latest_seek: u64,
    publications: u64,
    seeks_executed: u64,
    source_warnings: Vec<String>,
    source_path: Option<PathBuf>,
    prior_skips: u64,
    priors_completed: u64,
}

impl Runtime {
    fn new(config: EngineConfig, snapshots: SnapshotSlot, wake: Box<dyn Fn() + Send>) -> Self {
        Self {
            config,
            snapshots,
            wake,
            source: None,
            book: None,
            profile: None,
            source_meta: None,
            symbol: Arc::from(""),
            prior_build: None,
            playing: false,
            speed: 1.0,
            pace_event_origin: 0,
            pace_wall_origin: Instant::now(),
            generation: 0,
            watermarks: Watermarks::default(),
            coverage: CoverageCounters::default(),
            applied_ts: 0,
            latest_seek: 0,
            publications: 0,
            seeks_executed: 0,
            source_warnings: Vec::new(),
            source_path: None,
            prior_skips: 0,
            priors_completed: 0,
        }
    }

    fn run(mut self, rx: Receiver<EngineCmd>) -> EngineExit {
        let mut backlog = Vec::new();
        let mut shutdown = false;
        while !shutdown {
            if backlog.is_empty() {
                match rx.recv_timeout(if self.playing || self.prior_build.is_some() {
                    Duration::from_millis(1)
                } else {
                    IDLE_WAIT
                }) {
                    Ok(command) => backlog.push(command),
                    Err(mpsc::RecvTimeoutError::Disconnected) => {
                        panic!("fft-engine command channel disconnected without Shutdown")
                    }
                    Err(mpsc::RecvTimeoutError::Timeout) => {}
                }
            }
            drain(&rx, &mut backlog);
            if !backlog.is_empty() {
                shutdown = self.process_commands(std::mem::take(&mut backlog), &rx, &mut backlog);
                if shutdown {
                    break;
                }
            }
            if self.playing && self.forward_work().unwrap_or_else(|e| replay_panic(e)) {
                self.publish(0);
            }
            // Prior builds advance in time-budgeted slices after forward work so
            // playback latency is never blocked by a prior-day load (ENGINE.md §2).
            if self.prior_build.is_some() {
                let done = self
                    .advance_prior_build()
                    .unwrap_or_else(|e| replay_panic(e));
                if done {
                    self.finish_prior_build();
                }
            }
        }
        EngineExit {
            book_bytes: self.book.as_ref().map(Book::serialize_book),
            profile_bytes: self.profile.as_ref().map(MultiProfile::serialize),
            watermarks: self.watermarks,
            publications: self.publications,
            seeks_executed: self.seeks_executed,
            coverage: self.coverage,
            source_warnings: self.source_warnings,
            prior_skips: self.prior_skips,
            priors_completed: self.priors_completed,
        }
    }

    fn process_commands(
        &mut self,
        commands: Vec<EngineCmd>,
        rx: &Receiver<EngineCmd>,
        backlog: &mut Vec<EngineCmd>,
    ) -> bool {
        let mut selected_seek: Option<(u64, u64)> = None;
        // Batch order matters: `[Seek, Play]` means "seek, then play from there", but the
        // coalesced seek only executes after this loop — so a Play seen *after* the seek
        // is remembered and re-applied once the seek lands (otherwise execute_seek's
        // pause would silently swallow it).
        let mut play_after_seek = false;
        for command in commands {
            match command {
                EngineCmd::SetSource(Source::Replay { path }) => {
                    self.set_replay_source(path);
                    // ENGINE.md §4: a seek batched before this SetSource targeted the
                    // old source — drop it, and let seek generations restart.
                    selected_seek = None;
                    self.latest_seek = 0;
                }
                EngineCmd::SetSource(Source::Live { config }) => {
                    panic!(
                        "fft-engine live source {:?} is unavailable before M6",
                        config.name
                    )
                }
                EngineCmd::LoadPriorSession { path } => {
                    self.start_prior_build(path);
                }
                EngineCmd::Play => {
                    assert!(self.source.is_some(), "fft-engine Play without a source");
                    self.playing = true;
                    play_after_seek = selected_seek.is_some();
                    self.reset_pacing();
                }
                EngineCmd::Pause => {
                    self.playing = false;
                    play_after_seek = false;
                }
                EngineCmd::SetSpeed(speed) => {
                    assert!(
                        speed.is_finite() && speed > 0.0,
                        "fft-engine invalid replay speed {speed}"
                    );
                    assert!(
                        self.source.is_some(),
                        "fft-engine SetSpeed without replay source"
                    );
                    self.speed = speed;
                    self.reset_pacing();
                }
                EngineCmd::Seek { ts, generation } => {
                    assert!(
                        generation > 0,
                        "fft-engine seek generation zero is reserved"
                    );
                    assert!(
                        generation >= self.latest_seek,
                        "fft-engine seek generation regressed: {generation} < {}",
                        self.latest_seek
                    );
                    self.latest_seek = generation;
                    if selected_seek.is_none_or(|(_, selected)| generation > selected) {
                        selected_seek = Some((ts, generation));
                    }
                    // Seek pauses *at its position in the batch order*: a Play that
                    // follows in the same batch resumes after the seek executes below.
                    self.playing = false;
                }
                EngineCmd::GoLive => {
                    panic!("fft-engine GoLive requires an active live source")
                }
                EngineCmd::Shutdown => return true,
            }
        }
        if let Some((ts, generation)) = selected_seek {
            self.execute_seek(ts, generation, rx, backlog);
            // `[.., Seek, Play]` batch order: resume from the seek target. Skipped when
            // the seek was superseded — the newer seek (now in the backlog) decides.
            if play_after_seek && generation == self.latest_seek {
                self.playing = true;
                self.reset_pacing();
            }
        }
        false
    }

    /// `SetSource(Replay)`: drop any in-progress prior build; keep completed
    /// priors iff the new source shares the trade date (`docs/ENGINE.md` §2 r4).
    fn set_replay_source(&mut self, path: PathBuf) {
        // In-progress prior build is always dropped on source switch.
        self.prior_build = None;

        let source = ReplaySource::open(&path).unwrap_or_else(|e| replay_panic(e));
        self.source_warnings
            .extend(source.open_report().warnings.iter().cloned());
        let meta = source.meta().clone();

        // Retain completed priors only when the trade date is unchanged.
        let retained = match (&self.source_meta, self.profile.as_mut()) {
            (Some(prev), Some(profile)) if prev.trade_date == meta.trade_date => {
                profile.drain_prior_sessions()
            }
            _ => Vec::new(),
        };

        let mut profile = MultiProfile::new(meta.min_price_increment);
        if !retained.is_empty() {
            profile.seed_prior_sessions(retained);
        }
        profile.begin_session(meta.trade_date);
        self.book = Some(Book::new(meta.min_price_increment));
        self.profile = Some(profile);
        self.source = Some(source);
        // One heap alloc for the header symbol; publications only Arc-clone it.
        self.symbol = Arc::<str>::from(meta.symbol.as_str());
        self.source_meta = Some(meta);
        self.source_path = Some(path);
        self.playing = false;
        self.watermarks = Watermarks::default();
        self.coverage = CoverageCounters::default();
        self.applied_ts = 0;
    }

    /// Begin a profile-only prior-session build. Errors (missing file, wrong
    /// instrument, trade date ≥ current) are loud stderr + a counted skip —
    /// never a panic of the forward path (`docs/ENGINE.md` §2 rule 3).
    fn start_prior_build(&mut self, path: PathBuf) {
        // A new LoadPriorSession replaces any in-progress build (UI issues
        // oldest-first; mid-build replacement is still a drop, not a queue).
        self.prior_build = None;

        let Some(current_meta) = self.source_meta.as_ref() else {
            self.skip_prior(
                &path,
                "no current source (LoadPriorSession requires SetSource first)",
            );
            return;
        };

        let mut source = match ReplaySource::open(&path) {
            Ok(source) => source,
            Err(err) => {
                self.skip_prior(&path, &format!("open failed: {err}"));
                return;
            }
        };
        self.source_warnings
            .extend(source.open_report().warnings.iter().cloned());

        let prior_meta = source.meta().clone();
        if prior_meta.instrument_id != current_meta.instrument_id
            || prior_meta.symbol != current_meta.symbol
        {
            self.skip_prior(
                &path,
                &format!(
                    "instrument mismatch: prior {}/{} vs current {}/{}",
                    prior_meta.symbol,
                    prior_meta.instrument_id,
                    current_meta.symbol,
                    current_meta.instrument_id
                ),
            );
            return;
        }
        if prior_meta.trade_date >= current_meta.trade_date {
            self.skip_prior(
                &path,
                &format!(
                    "trade date {} is not earlier than current {}",
                    prior_meta.trade_date, current_meta.trade_date
                ),
            );
            return;
        }
        if self
            .profile
            .as_ref()
            .is_some_and(|p| p.session(prior_meta.trade_date).is_some())
        {
            self.skip_prior(
                &path,
                &format!(
                    "trade date {} already present in profile",
                    prior_meta.trade_date
                ),
            );
            return;
        }

        let (book, profile) = match source.prepare_prior_build() {
            Ok(state) => state,
            Err(err) => {
                self.skip_prior(&path, &format!("checkpoint restore failed: {err}"));
                return;
            }
        };

        self.prior_build = Some(PriorBuild {
            path,
            source,
            book,
            profile,
        });
    }

    fn skip_prior(&mut self, path: &std::path::Path, reason: &str) {
        self.prior_skips += 1;
        eprintln!(
            "fft-engine LoadPriorSession skipped {}: {reason}",
            path.display()
        );
    }

    /// Advance the in-progress prior build under `PRIOR_BUILD_BUDGET`.
    /// Returns `true` when the prior log is fully applied (ready to insert).
    /// Does **not** touch watermarks or `CoverageCounters` (forward-flow only).
    fn advance_prior_build(&mut self) -> std::result::Result<bool, ReplayError> {
        let build = self
            .prior_build
            .as_mut()
            .expect("advance_prior_build without a build");
        // Large event ceiling; the time budget is the real stop condition.
        let progress = build.source.apply_forward(
            &mut build.book,
            &mut build.profile,
            usize::MAX,
            PRIOR_BUILD_BUDGET,
        )?;
        Ok(progress.eof)
    }

    /// Insert the completed prior session and publish once. Complete-or-invisible:
    /// no partial publications ever reached the slot.
    fn finish_prior_build(&mut self) {
        let build = self
            .prior_build
            .take()
            .expect("finish_prior_build without a build");
        let path = build.path;
        // A prior log is a single trade-date session; take its completed profile.
        let mut sessions = build.profile.sessions().to_vec();
        if sessions.is_empty() {
            self.skip_prior(&path, "prior log produced zero sessions");
            return;
        }
        if sessions.len() > 1 {
            // Loud but non-fatal: insert only the last (developing) session of the
            // prior log — multi-day priors are not in the M4 UI contract.
            eprintln!(
                "fft-engine LoadPriorSession {}: prior log has {} sessions; using last only",
                path.display(),
                sessions.len()
            );
        }
        let session: SessionProfile = sessions.pop().expect("non-empty");
        let date = session.trade_date();

        // Validate without holding a mutable borrow across skip_prior.
        let reject = match self.profile.as_ref() {
            None => Some("no current profile at completion".to_string()),
            Some(profile) if profile.session(date).is_some() => {
                Some(format!("trade date {date} already present at completion"))
            }
            Some(profile) => match profile.current() {
                None => Some("no current session at completion".to_string()),
                Some(current) if date >= current.trade_date() => Some(format!(
                    "trade date {date} is not earlier than current {} at completion",
                    current.trade_date()
                )),
                Some(_) => None,
            },
        };
        if let Some(reason) = reject {
            self.skip_prior(&path, &reason);
            return;
        }

        self.profile
            .as_mut()
            .expect("validated present")
            .insert_prior_session(session);
        self.priors_completed += 1;
        // One normal publication so the UI sees the new session
        // (complete-or-invisible: never published partial).
        if self.book.is_some() {
            self.publish(0);
        }
    }

    fn execute_seek(
        &mut self,
        ts: u64,
        generation: u64,
        rx: &Receiver<EngineCmd>,
        backlog: &mut Vec<EngineCmd>,
    ) {
        self.playing = false;
        self.assert_seekable();
        // ReplaySource::seek replaces the profile before its first cancellation
        // poll. Detach engine-owned completed priors, then restore them onto the
        // resulting current session whether the seek succeeds or is cancelled.
        let retained_priors = self
            .profile
            .as_mut()
            .expect("fft-engine source missing profile")
            .drain_prior_sessions();
        let source = self
            .source
            .as_mut()
            .expect("fft-engine Seek without replay source");
        let book = self.book.as_mut().expect("fft-engine source missing Book");
        let profile = self
            .profile
            .as_mut()
            .expect("fft-engine source missing profile");
        let mut interrupted = Vec::new();
        let report = source
            .seek(ts, book, profile, || {
                drain(rx, &mut interrupted);
                interrupted.iter().any(|command| {
                    matches!(
                        command,
                        EngineCmd::Seek {
                            generation: newer,
                            ..
                        } if *newer > generation
                    ) || matches!(command, EngineCmd::Shutdown | EngineCmd::SetSource(_))
                })
            })
            .unwrap_or_else(|e| replay_panic(e));
        for prior in retained_priors {
            profile.insert_prior_session(prior);
        }
        drain(rx, &mut interrupted);
        let newest = interrupted
            .iter()
            .filter_map(|command| match command {
                EngineCmd::Seek { generation, .. } => Some(*generation),
                _ => None,
            })
            .max()
            .unwrap_or(generation);
        self.latest_seek = self.latest_seek.max(newest);
        backlog.extend(interrupted);
        if report.cancelled || generation < self.latest_seek {
            return;
        }
        self.watermarks.set_applied(report.applied_seq);
        self.applied_ts = report.applied_ts;
        self.seeks_executed += 1;
        self.publish(generation);
    }

    /// ENGINE.md §4: seeking a log with zero CHECKPOINT frames can only be served by
    /// replaying from frame zero — a silent degraded path, so it is refused loudly.
    fn assert_seekable(&self) {
        let source = self
            .source
            .as_ref()
            .expect("fft-engine Seek without replay source");
        if source.checkpoint_count() > 0 {
            return;
        }
        let path = self
            .source_path
            .as_ref()
            .map_or_else(|| "<unknown log>".to_string(), |p| p.display().to_string());
        panic!(
            "fft-engine Seek against a log with zero checkpoints: {path}. \
             Run `fft-checkpoint {path} <checkpointed.fftlog>` and replay that copy — \
             serving the seek by replaying from frame zero is a forbidden degraded path \
             (docs/ENGINE.md §4)"
        );
    }

    fn forward_work(&mut self) -> std::result::Result<bool, ReplayError> {
        let start = Instant::now();
        let mut applied = false;
        while start.elapsed() < APPLY_BUDGET {
            let source = self.source.as_mut().expect("playing without replay source");
            let Some(next) = source.peek_event()? else {
                self.playing = false;
                break;
            };
            let event_delta = next.ts.0.saturating_sub(self.pace_event_origin);
            let due = self.pace_wall_origin
                + Duration::from_nanos((event_delta as f64 / self.speed) as u64);
            if Instant::now() < due {
                break;
            }
            self.coverage.events_read += 1;
            let event = source
                .apply_next(
                    self.book.as_mut().expect("source missing Book"),
                    self.profile.as_mut().expect("source missing profile"),
                )?
                .expect("peeked replay event disappeared");
            self.coverage.events_applied += 1;
            // Snapshot records carry original order-entry seqs (FFTLOG-V2 §4) — non-channel
            // and freely regressing — so they retain the prior watermark, exactly like
            // seq-0 records; neither advances nor consumes sequence accounting.
            if event.seq.0 != 0 && !event.is_snapshot() {
                self.watermarks.apply_forward(u64::from(event.seq.0));
            }
            if event.kind == EventKind::Gap {
                self.coverage.gap_records += 1;
                // Gap re-anchors sequence accounting (FFTLOG-V2 §4): the post-gap
                // channel seq may be below the pre-gap watermark, same as Book::do_gap.
                self.watermarks.gap();
            }
            self.applied_ts = event.ts.0;
            applied = true;
        }
        Ok(applied)
    }

    fn reset_pacing(&mut self) {
        // Anchor to the next event (not absolute applied_ts) so a cold Play at
        // session-open timestamps does not schedule a multi-year wall wait.
        let next = match self.source.as_mut() {
            // An empty source is legal; an unreadable one is not — never pace over it.
            Some(source) => source.peek_event().unwrap_or_else(|e| replay_panic(e)),
            None => None,
        };
        let origin = next.map(|event| event.ts.0).unwrap_or(self.applied_ts);
        self.pace_event_origin = origin;
        self.pace_wall_origin = Instant::now();
    }

    fn publish(&mut self, seek_generation: u64) {
        self.generation += 1;
        self.watermarks.publish();
        debug_assert_eq!(
            self.coverage.events_read, self.coverage.events_applied,
            "fft-engine coverage invariant: every decoded event applies exactly once"
        );
        let mut snapshot = build_snapshot(
            self.generation,
            self.watermarks.applied_seq,
            self.applied_ts,
            seek_generation,
            self.config.visible_tick_span,
            self.book.as_ref().expect("publication without Book"),
            self.profile.as_ref().expect("publication without profile"),
        );
        snapshot.symbol = self.symbol.clone();
        snapshot.coverage = self.coverage;
        assert!(
            snapshot.estimated_heap_bytes() <= 8 * 1024 * 1024,
            "fft-engine snapshot heap {} exceeds 8 MiB",
            snapshot.estimated_heap_bytes()
        );
        self.snapshots.publish(snapshot);
        self.publications += 1;
        (self.wake)();
    }
}

fn drain(rx: &Receiver<EngineCmd>, commands: &mut Vec<EngineCmd>) {
    loop {
        match rx.try_recv() {
            Ok(command) => commands.push(command),
            Err(TryRecvError::Empty) => return,
            Err(TryRecvError::Disconnected) => return,
        }
    }
}

fn replay_panic(error: impl fmt::Display) -> ! {
    panic!("fft-engine replay failure: {error}")
}
