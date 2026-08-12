//! Seek execution for the engine runtime.

use crate::command::EngineCmd;
use crate::runtime::{Runtime, drain, replay_panic};
use crate::sim_live::SimLivePhase;
use std::sync::mpsc::Receiver;

impl Runtime {
    pub(crate) fn execute_seek(
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
