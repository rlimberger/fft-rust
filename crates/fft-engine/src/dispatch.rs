//! Command-batch dispatch for the engine runtime.

use crate::command::{EngineCmd, Source};
use crate::prior;
use crate::runtime::Runtime;
use std::sync::mpsc::Receiver;

impl Runtime {
    pub(crate) fn process_commands(
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
                    // Live-out checkpoints serialize the full MultiProfile; priors would
                    // break §5 bit-identity of the live log. UI forbids this; fail loud.
                    assert!(
                        self.sim_live.is_none(),
                        "fft-engine LoadPriorSession is forbidden under SimLive ({})",
                        path.display()
                    );
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
}
