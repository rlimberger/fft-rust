//! Forward apply / pacing / publication for the engine runtime.

use crate::pacing::{self, APPLY_BUDGET};
use crate::runtime::{Runtime, replay_panic};
use crate::sim_live::SimLivePhase;
use crate::snapshot::{LiveTransportPhase, build_snapshot};
use fft_core::{CanonicalEvent, EventKind};
use fft_replay::ReplayError;
use std::time::Instant;

impl Runtime {
    pub(crate) fn forward_work(&mut self) -> Result<bool, ReplayError> {
        let start = Instant::now();
        let mut applied = false;
        while start.elapsed() < APPLY_BUDGET {
            if !self.due_next()? {
                break;
            }
            if self.apply_one()?.is_none() {
                self.playing = false;
                break;
            }
            applied = true;
        }
        Ok(applied)
    }

    fn due_next(&mut self) -> Result<bool, ReplayError> {
        let source = self.source.as_mut().expect("playing without source");
        let Some(next) = source.peek_event()? else {
            if matches!(
                self.sim_live.as_ref().map(|live| live.phase),
                Some(SimLivePhase::CatchingUp { .. })
            ) {
                panic!("fft-engine SimLive head_ts was not reached before source EOF");
            }
            self.playing = false;
            return Ok(false);
        };
        let Some(live) = self.sim_live.as_ref() else {
            let due = pacing::speed_due(
                self.pace_event_origin,
                self.pace_wall_origin,
                self.speed,
                next.ts.0,
                Instant::now(),
            );
            return Ok(due);
        };
        match live.phase {
            SimLivePhase::CatchingUp { head_ts } => {
                if next.ts.0 > head_ts {
                    assert_eq!(
                        self.applied_ts, head_ts,
                        "fft-engine SimLive head_ts is not an event timestamp in the source"
                    );
                    let live = self.sim_live.as_mut().expect("sim-live");
                    live.phase = SimLivePhase::WallPinned {
                        wall_at_head: Instant::now(),
                    };
                    self.coverage.head_lag_ns = 0;
                    self.speed = 1.0;
                    let live = self.sim_live.as_ref().expect("sim-live");
                    let SimLivePhase::WallPinned { wall_at_head } = live.phase else {
                        unreachable!()
                    };
                    Ok(pacing::wall_pin_due(
                        live.head_ts,
                        wall_at_head,
                        next.ts.0,
                        Instant::now(),
                    ))
                } else {
                    Ok(true)
                }
            }
            SimLivePhase::WallPinned { wall_at_head } => Ok(pacing::wall_pin_due(
                live.head_ts,
                wall_at_head,
                next.ts.0,
                Instant::now(),
            )),
            SimLivePhase::CatchingToWall {
                wall_at_head,
                target_ts,
            } => {
                if next.ts.0 > target_ts {
                    let live = self.sim_live.as_mut().expect("sim-live");
                    live.phase = SimLivePhase::WallPinned { wall_at_head };
                    self.coverage.head_lag_ns = 0;
                    self.speed = 1.0;
                    Ok(pacing::wall_pin_due(
                        live.head_ts,
                        wall_at_head,
                        next.ts.0,
                        Instant::now(),
                    ))
                } else {
                    Ok(true)
                }
            }
            SimLivePhase::ScrubbedBack { .. } => {
                // Ordinal is authoritative for same-ts bursts at the sealed tip
                // (§5.3 / scrub-Play must not apply past live_out without append).
                if !live.scrub_transport_allows(next.ts.0) {
                    return Ok(false);
                }
                Ok(pacing::speed_due(
                    self.pace_event_origin,
                    self.pace_wall_origin,
                    self.speed,
                    next.ts.0,
                    Instant::now(),
                ))
            }
        }
    }

    fn apply_one(&mut self) -> Result<Option<CanonicalEvent>, ReplayError> {
        let source = self.source.as_mut().expect("apply without source");
        let Some(peeked) = source.peek_event()? else {
            return Ok(None);
        };
        let sim = self.sim_live.is_some();
        if sim && peeked.seq.0 != 0 && !peeked.is_snapshot() {
            self.watermarks.receive_decoded(u64::from(peeked.seq.0));
        }
        self.coverage.events_read += 1;
        let event = source
            .apply_next(
                self.book.as_mut().expect("source missing Book"),
                self.profile.as_mut().expect("source missing profile"),
            )?
            .expect("peeked replay event disappeared");
        self.coverage.events_applied += 1;
        if event.seq.0 != 0 && !event.is_snapshot() {
            if sim {
                self.watermarks.apply_live(u64::from(event.seq.0));
            } else {
                self.watermarks.apply_forward(u64::from(event.seq.0));
            }
        }
        if event.kind == EventKind::Gap {
            self.coverage.gap_records += 1;
            self.watermarks.gap();
        }
        self.applied_ts = event.ts.0;
        if let Some(live) = self.sim_live.as_mut() {
            let book = self.book.as_ref().expect("live append needs Book");
            let profile = self.profile.as_ref().expect("live append needs profile");
            let now = Instant::now();
            let commit = live.note_applied(&event, book, profile, now);
            if let Some(seq) = commit.committed_logged_seq {
                self.watermarks.set_logged(seq);
            }
            if commit.gap_reanchor {
                self.watermarks.note_logged_gap();
            }
            self.coverage.head_lag_ns = live.head_lag_ns(self.applied_ts, now);
            match live.phase {
                SimLivePhase::CatchingUp { head_ts } if self.applied_ts >= head_ts => {
                    // Pin only when the next event is past head so same-ts bursts
                    // at head_ts still append under CatchingUp.
                    let past_head = self
                        .source
                        .as_mut()
                        .and_then(|s| s.peek_event().ok().flatten())
                        .is_none_or(|n| n.ts.0 > head_ts);
                    if past_head {
                        assert_eq!(
                            self.applied_ts, head_ts,
                            "fft-engine SimLive head_ts is not an event timestamp in the source"
                        );
                        live.phase = SimLivePhase::WallPinned {
                            wall_at_head: Instant::now(),
                        };
                        self.coverage.head_lag_ns = 0;
                        self.speed = 1.0;
                    }
                }
                SimLivePhase::CatchingToWall {
                    wall_at_head,
                    target_ts,
                } if self.applied_ts >= target_ts => {
                    live.phase = SimLivePhase::WallPinned { wall_at_head };
                    self.coverage.head_lag_ns = 0;
                    self.speed = 1.0;
                }
                _ => {}
            }
        }
        Ok(Some(event))
    }

    pub(crate) fn reset_pacing(&mut self) {
        let next = match self.source.as_mut() {
            Some(source) => source.peek_event().unwrap_or_else(|e| replay_panic(e)),
            None => None,
        };
        let origin = next.map(|event| event.ts.0).unwrap_or(self.applied_ts);
        self.pace_event_origin = origin;
        self.pace_wall_origin = Instant::now();
    }

    pub(crate) fn publish(&mut self, seek_generation: u64) {
        self.generation += 1;
        self.watermarks.publish();
        debug_assert_eq!(
            self.coverage.events_read, self.coverage.events_applied,
            "fft-engine coverage invariant: every decoded event applies exactly once"
        );
        if let Some(live) = self.sim_live.as_ref() {
            self.coverage.head_lag_ns = live.head_lag_ns(self.applied_ts, Instant::now());
        }
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
        snapshot.live_phase = match self.sim_live.as_ref().map(|live| live.phase) {
            None => LiveTransportPhase::Inactive,
            Some(SimLivePhase::CatchingUp { .. }) => LiveTransportPhase::CatchingUp,
            Some(SimLivePhase::WallPinned { .. }) => LiveTransportPhase::Live,
            Some(SimLivePhase::ScrubbedBack { .. }) => LiveTransportPhase::Scrubbed,
            Some(SimLivePhase::CatchingToWall { .. }) => LiveTransportPhase::CatchingToWall,
        };
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::service::EngineConfig;
    use crate::sim_live::SimLiveState;
    use crate::snapshot::{RenderSnapshot, SnapshotSlot};
    use fft_book::Book;
    use fft_core::Price;
    use fft_profile::MultiProfile;
    use std::sync::Arc;
    use std::time::Instant;

    fn publish_runtime(sim_live: Option<SimLiveState>) -> Arc<RenderSnapshot> {
        let snapshots = SnapshotSlot::new(Arc::new(RenderSnapshot::default()));
        let mut rt = Runtime::new(
            EngineConfig {
                visible_tick_span: 8,
            },
            snapshots.clone(),
            Box::new(|| {}),
        );
        let tick = Price(250_000_000);
        rt.book = Some(Book::new(tick));
        let mut profile = MultiProfile::new(tick);
        profile.begin_session(20_663);
        rt.profile = Some(profile);
        rt.sim_live = sim_live;
        rt.publish(0);
        snapshots.load()
    }

    #[test]
    fn publish_maps_sim_live_phase_to_live_transport_phase() {
        let inactive = publish_runtime(None);
        assert_eq!(inactive.live_phase, LiveTransportPhase::Inactive);

        let catching = publish_runtime(Some(SimLiveState {
            head_ts: 1,
            phase: SimLivePhase::CatchingUp { head_ts: 1 },
            tip_ts: 0,
            tip_ordinal: 0,
            cursor_ordinal: 0,
            sealed_tip_ordinal: None,
            live_log: None,
        }));
        assert_eq!(catching.live_phase, LiveTransportPhase::CatchingUp);

        let live = publish_runtime(Some(SimLiveState {
            head_ts: 1,
            phase: SimLivePhase::WallPinned {
                wall_at_head: Instant::now(),
            },
            tip_ts: 0,
            tip_ordinal: 0,
            cursor_ordinal: 0,
            sealed_tip_ordinal: None,
            live_log: None,
        }));
        assert_eq!(live.live_phase, LiveTransportPhase::Live);
    }
}
