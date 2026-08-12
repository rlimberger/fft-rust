//! Engine-thread runtime (`docs/ENGINE.md` §2/§5).

use crate::command::{EngineCmd, Source};
use crate::live_log::LiveLog;
use crate::pacing;
use crate::prior::{self, PriorBuild};
use crate::service::{EngineConfig, EngineExit};
use crate::sim_live::{SimLivePhase, SimLiveState};
use crate::snapshot::{CoverageCounters, SnapshotSlot};
use crate::watermarks::Watermarks;
use fft_book::Book;
use fft_core::InstrumentMeta;
use fft_profile::MultiProfile;
use fft_replay::ReplaySource;
use std::fmt;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::mpsc::{Receiver, RecvTimeoutError, TryRecvError};
use std::time::{Duration, Instant};

const IDLE_WAIT: Duration = Duration::from_millis(10);

pub(crate) struct Runtime {
    pub(crate) config: EngineConfig,
    pub(crate) snapshots: SnapshotSlot,
    pub(crate) wake: Box<dyn Fn() + Send>,
    pub(crate) source: Option<ReplaySource>,
    pub(crate) book: Option<Book>,
    pub(crate) profile: Option<MultiProfile>,
    pub(crate) source_meta: Option<InstrumentMeta>,
    pub(crate) symbol: Arc<str>,
    pub(crate) prior_build: Option<PriorBuild>,
    pub(crate) sim_live: Option<SimLiveState>,
    pub(crate) playing: bool,
    pub(crate) speed: f64,
    pub(crate) pace_event_origin: u64,
    pub(crate) pace_wall_origin: Instant,
    pub(crate) generation: u64,
    pub(crate) watermarks: Watermarks,
    pub(crate) coverage: CoverageCounters,
    pub(crate) applied_ts: u64,
    pub(crate) latest_seek: u64,
    pub(crate) publications: u64,
    pub(crate) seeks_executed: u64,
    pub(crate) source_warnings: Vec<String>,
    pub(crate) source_path: Option<PathBuf>,
    pub(crate) prior_skips: u64,
    pub(crate) priors_completed: u64,
}

impl Runtime {
    pub(crate) fn new(
        config: EngineConfig,
        snapshots: SnapshotSlot,
        wake: Box<dyn Fn() + Send>,
    ) -> Self {
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
            sim_live: None,
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

    pub(crate) fn run(mut self, rx: Receiver<EngineCmd>) -> EngineExit {
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
                    Err(RecvTimeoutError::Disconnected) => {
                        panic!("fft-engine command channel disconnected without Shutdown")
                    }
                    Err(RecvTimeoutError::Timeout) => {}
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
            if self.prior_build.is_some() {
                let done = self.advance_prior().unwrap_or_else(|e| replay_panic(e));
                if done {
                    self.finish_prior();
                }
            }
        }
        self.close_live_log();
        EngineExit {
            book_bytes: self.book.as_ref().map(Book::serialize_book),
            flow_bytes: self.book.as_ref().map(Book::serialize_flow),
            refresh_bytes: self.book.as_ref().map(Book::serialize_refresh),
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

    fn close_live_log(&mut self) {
        if let Some(mut live) = self.sim_live.take()
            && let Some(log) = live.live_log.take()
        {
            let commit = log.close();
            if let Some(seq) = commit.committed_logged_seq {
                self.watermarks.set_logged(seq);
            }
            if commit.gap_reanchor {
                self.watermarks.note_logged_gap();
            }
        }
    }

    fn process_commands(
        &mut self,
        commands: Vec<EngineCmd>,
        rx: &Receiver<EngineCmd>,
        backlog: &mut Vec<EngineCmd>,
    ) -> bool {
        #[derive(Clone, Copy)]
        enum AfterSeek {
            Paused,
            Play,
            GoLive,
        }

        let mut selected_seek: Option<(u64, u64)> = None;
        let mut after_seek = AfterSeek::Paused;
        for command in commands {
            match command {
                EngineCmd::SetSource(Source::Replay { path }) => {
                    self.set_replay_source(path);
                    selected_seek = None;
                    after_seek = AfterSeek::Paused;
                    self.latest_seek = 0;
                }
                EngineCmd::SetSource(Source::SimLive {
                    path,
                    head_ts,
                    live_out,
                }) => {
                    self.set_sim_live_source(path, head_ts, live_out);
                    selected_seek = None;
                    after_seek = AfterSeek::Paused;
                    self.latest_seek = 0;
                }
                EngineCmd::SetSource(Source::Live { config }) => {
                    panic!(
                        "fft-engine live source {:?} is unavailable before M6",
                        config.name
                    )
                }
                EngineCmd::LoadPriorSession { path } => {
                    self.prior_build = None;
                    self.prior_build = prior::start_prior_build(
                        path,
                        self.source_meta.as_ref(),
                        self.profile.as_ref(),
                        &mut self.source_warnings,
                        &mut self.prior_skips,
                    );
                }
                EngineCmd::Play => {
                    assert!(self.source.is_some(), "fft-engine Play without a source");
                    self.playing = true;
                    if selected_seek.is_some() {
                        after_seek = AfterSeek::Play;
                    }
                    self.reset_pacing();
                }
                EngineCmd::Pause => {
                    self.playing = false;
                    if selected_seek.is_some() {
                        after_seek = AfterSeek::Paused;
                    }
                }
                EngineCmd::SetSpeed(speed) => {
                    assert!(
                        speed.is_finite() && speed > 0.0,
                        "fft-engine invalid replay speed {speed}"
                    );
                    assert!(
                        self.source.is_some(),
                        "fft-engine SetSpeed without a source"
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
                    self.playing = false;
                    after_seek = AfterSeek::Paused;
                }
                EngineCmd::GoLive => {
                    if selected_seek.is_some() {
                        after_seek = AfterSeek::GoLive;
                    } else {
                        self.go_live();
                    }
                }
                EngineCmd::Shutdown => return true,
            }
        }
        if let Some((ts, generation)) = selected_seek {
            self.execute_seek(ts, generation, rx, backlog);
            if generation == self.latest_seek {
                match after_seek {
                    AfterSeek::Paused => {}
                    AfterSeek::Play => {
                        self.playing = true;
                        self.reset_pacing();
                    }
                    AfterSeek::GoLive => self.go_live(),
                }
            }
        }
        false
    }

    fn set_replay_source(&mut self, path: PathBuf) {
        self.close_live_log();
        self.sim_live = None;
        self.prior_build = None;
        let source = ReplaySource::open(&path).unwrap_or_else(|e| replay_panic(e));
        self.install_source(source, path);
        self.playing = false;
    }

    fn set_sim_live_source(&mut self, path: PathBuf, head_ts: u64, live_out: PathBuf) {
        self.close_live_log();
        self.prior_build = None;
        let mut source = ReplaySource::open(&path).unwrap_or_else(|e| replay_panic(e));
        let first_ts = source
            .peek_event()
            .unwrap_or_else(|e| replay_panic(e))
            .unwrap_or_else(|| panic!("fft-engine SimLive source is empty"))
            .ts
            .0;
        assert!(
            head_ts >= first_ts,
            "fft-engine SimLive head_ts {head_ts} precedes source open {first_ts}"
        );
        let mut found_head = false;
        while let Some(event) = source.next_event().unwrap_or_else(|e| replay_panic(e)) {
            if event.ts.0 == head_ts {
                found_head = true;
            } else if found_head && event.ts.0 > head_ts {
                break;
            }
        }
        assert!(
            found_head,
            "fft-engine SimLive head_ts {head_ts} is not an event timestamp in the source"
        );
        let source = ReplaySource::open(&path).unwrap_or_else(|e| replay_panic(e));
        let meta = source.meta().clone();
        // Validation is intentionally complete before create: an invalid head must
        // never truncate an existing live_out.
        let live_log = LiveLog::create(&live_out, &meta);
        self.install_source(source, path);
        self.sim_live = Some(SimLiveState::new(head_ts, live_log));
        self.playing = true;
        self.speed = 1.0;
        self.reset_pacing();
    }

    fn install_source(&mut self, source: ReplaySource, path: PathBuf) {
        self.source_warnings
            .extend(source.open_report().warnings.iter().cloned());
        let meta = source.meta().clone();
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
        self.symbol = Arc::<str>::from(meta.symbol.as_str());
        self.source_meta = Some(meta);
        self.source_path = Some(path);
        self.watermarks = Watermarks::default();
        self.coverage = CoverageCounters::default();
        self.applied_ts = 0;
        // SetSource drops any in-flight seek accounting (ENGINE.md §5.1 join path).
        self.seeks_executed = 0;
    }

    fn go_live(&mut self) {
        let Some(live) = self.sim_live.as_mut() else {
            panic!("fft-engine GoLive requires an active live source")
        };
        let wall_at_head = live
            .wall_at_head()
            .unwrap_or_else(|| panic!("fft-engine GoLive during catch-up; wait for the head pin"));
        let target_ts = pacing::wall_head_ts(live.head_ts, wall_at_head, Instant::now());
        live.seal_tip();
        live.phase = SimLivePhase::CatchingToWall {
            wall_at_head,
            target_ts,
        };
        self.speed = 1.0;
        self.playing = true;
        self.reset_pacing();
        let _ = self.forward_work().unwrap_or_else(|e| replay_panic(e));
        if self.book.is_some() {
            self.publish(0);
        }
    }

    fn advance_prior(&mut self) -> Result<bool, fft_replay::ReplayError> {
        let build = self
            .prior_build
            .as_mut()
            .expect("advance_prior without a build");
        prior::advance_prior_build(build)
    }

    fn finish_prior(&mut self) {
        let build = self
            .prior_build
            .take()
            .expect("finish_prior without a build");
        if prior::finish_prior_build(
            build,
            &mut self.profile,
            &mut self.prior_skips,
            &mut self.priors_completed,
        ) && self.book.is_some()
        {
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
        if let Some(live) = self.sim_live.as_ref() {
            match live.phase {
                SimLivePhase::CatchingUp { .. } => {
                    panic!(
                        "fft-engine SimLive Seek during catch-up is forbidden \
                         (join never checkpoint-skips; docs/ENGINE.md §5.1)"
                    )
                }
                SimLivePhase::WallPinned { .. }
                | SimLivePhase::ScrubbedBack { .. }
                | SimLivePhase::CatchingToWall { .. } => {
                    if ts > live.tip_ts {
                        panic!(
                            "fft-engine SimLive Seek past live tip \
                             (ts={ts} tip={}); use GoLive",
                            live.tip_ts
                        )
                    }
                }
            }
        }
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
        if self.sim_live.is_some() {
            self.watermarks.set_applied_keep_logged(report.applied_seq);
        } else {
            self.watermarks.set_applied(report.applied_seq);
        }
        self.applied_ts = report.applied_ts;
        if self.sim_live.is_some() {
            let Some(live) = self.sim_live.as_mut() else {
                unreachable!("sim-live presence changed without yielding")
            };
            let wall = live
                .wall_at_head()
                .expect("fft-engine SimLive seek without wall pin");
            live.seal_tip();
            live.reset_cursor_ordinal(report.event_ordinal);
            live.phase = SimLivePhase::ScrubbedBack { wall_at_head: wall };
            self.coverage.head_lag_ns = 0;
        }
        self.seeks_executed += 1;
        self.publish(generation);
    }

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

pub(crate) fn replay_panic(error: impl fmt::Display) -> ! {
    panic!("fft-engine replay failure: {error}")
}
