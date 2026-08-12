//! Engine-thread runtime (`docs/ENGINE.md` §2/§5).

use crate::command::EngineCmd;
use crate::prior::{self, PriorBuild};
use crate::service::{EngineConfig, EngineExit};
use crate::sim_live::SimLiveState;
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
}

pub(crate) fn drain(rx: &Receiver<EngineCmd>, commands: &mut Vec<EngineCmd>) {
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
