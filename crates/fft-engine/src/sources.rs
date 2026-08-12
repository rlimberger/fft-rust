//! Source install / live-log lifecycle for the engine runtime.

use crate::live_log::LiveLog;
use crate::pacing;
use crate::runtime::{Runtime, replay_panic};
use crate::sim_live::{SimLivePhase, SimLiveState};
use crate::snapshot::CoverageCounters;
use crate::watermarks::Watermarks;
use fft_book::Book;
use fft_profile::MultiProfile;
use fft_replay::ReplaySource;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;

impl Runtime {
    pub(crate) fn close_live_log(&mut self) {
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

    pub(crate) fn set_replay_source(&mut self, path: PathBuf) {
        self.close_live_log();
        self.sim_live = None;
        self.prior_build = None;
        let source = ReplaySource::open(&path).unwrap_or_else(|e| replay_panic(e));
        self.install_source(source, path);
        self.playing = false;
    }

    pub(crate) fn set_sim_live_source(&mut self, path: PathBuf, head_ts: u64, live_out: PathBuf) {
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
        let live_log = LiveLog::create(&live_out, &meta, Instant::now());
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

    pub(crate) fn go_live(&mut self) {
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
}
