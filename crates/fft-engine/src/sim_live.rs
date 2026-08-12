//! Sim-live source state (`docs/ENGINE.md` §5).

use crate::live_log::{LiveLog, LiveLogCommit};
use crate::pacing;
use fft_book::Book;
use fft_core::CanonicalEvent;
use fft_profile::MultiProfile;
use std::time::Instant;

/// Join / pin / scrub transport phases for [`Source::SimLive`](crate::Source::SimLive).
#[derive(Debug, Clone, Copy)]
pub(crate) enum SimLivePhase {
    /// Unpaced catch-up from session open through `head_ts` (clause 1).
    CatchingUp { head_ts: u64 },
    /// Absolute wall pin at the join head (clause 2).
    WallPinned { wall_at_head: Instant },
    /// Scrubbed/paused behind the tip; `SetSpeed` paces over the already-streamed range.
    ScrubbedBack { wall_at_head: Instant },
    /// `GoLive` unpaced catch-up of the interim to `target_ts` (clause 3).
    CatchingToWall {
        wall_at_head: Instant,
        target_ts: u64,
    },
}

/// Engine-owned sim-live transport + live-log append state.
pub(crate) struct SimLiveState {
    pub(crate) head_ts: u64,
    pub(crate) phase: SimLivePhase,
    /// Highest event-ts already appended to the live log (transport seek bound).
    pub(crate) tip_ts: u64,
    /// Number of source events appended at the durable tip. Re-application after a
    /// scrub suppresses exactly this ordinal prefix, including a same-ts suffix.
    pub(crate) tip_ordinal: u64,
    /// Source-event ordinal in the current replay traversal.
    pub(crate) cursor_ordinal: u64,
    /// Exact ordinal frontier sealed before a scrub / GoLive re-catch.
    pub(crate) sealed_tip_ordinal: Option<u64>,
    pub(crate) live_log: Option<LiveLog>,
}

impl SimLiveState {
    pub(crate) fn new(head_ts: u64, live_log: LiveLog) -> Self {
        Self {
            head_ts,
            phase: SimLivePhase::CatchingUp { head_ts },
            tip_ts: 0,
            tip_ordinal: 0,
            cursor_ordinal: 0,
            sealed_tip_ordinal: None,
            live_log: Some(live_log),
        }
    }

    pub(crate) fn wall_at_head(&self) -> Option<Instant> {
        match self.phase {
            SimLivePhase::WallPinned { wall_at_head }
            | SimLivePhase::ScrubbedBack { wall_at_head }
            | SimLivePhase::CatchingToWall { wall_at_head, .. } => Some(wall_at_head),
            SimLivePhase::CatchingUp { .. } => None,
        }
    }

    /// Freeze tip before scrub / GoLive so re-apply cannot re-append.
    pub(crate) fn seal_tip(&mut self) {
        self.sealed_tip_ordinal = Some(self.tip_ordinal);
    }

    /// A checkpoint seek places the cursor at `SeekReport::event_ordinal`.
    pub(crate) fn reset_cursor_ordinal(&mut self, ordinal: u64) {
        self.cursor_ordinal = ordinal;
    }

    /// Scrub-Play stays inside the sealed tip (§5.3). Ordinal is authoritative so a
    /// wall-pin tip that stopped mid same-ts burst cannot re-apply the unsealed
    /// suffix without a live-log append; `tip_ts` is the secondary bound.
    pub(crate) fn scrub_transport_allows(&self, next_ts: u64) -> bool {
        let tip_ordinal = self.sealed_tip_ordinal.unwrap_or(self.tip_ordinal);
        if self.cursor_ordinal >= tip_ordinal {
            return false;
        }
        next_ts <= self.tip_ts
    }

    pub(crate) fn head_lag_ns(&self, applied_ts: u64, now: Instant) -> i64 {
        match self.phase {
            SimLivePhase::WallPinned { wall_at_head }
            | SimLivePhase::CatchingToWall { wall_at_head, .. } => {
                pacing::head_lag_ns(self.head_ts, wall_at_head, applied_ts, now)
            }
            SimLivePhase::CatchingUp { .. } | SimLivePhase::ScrubbedBack { .. } => 0,
        }
    }

    /// Append applied events (`docs/ENGINE.md` §5.4). Scrubbed replay never
    /// appends. GoLive re-catch suppresses appends until the sealed tip is
    /// crossed, then appends every applied event — including snapshot records
    /// whose order-entry timestamps can regress below `tip_ts`.
    pub(crate) fn note_applied(
        &mut self,
        event: &CanonicalEvent,
        book: &Book,
        profile: &MultiProfile,
        now: Instant,
    ) -> LiveLogCommit {
        self.cursor_ordinal = self.cursor_ordinal.saturating_add(1);
        match self.phase {
            SimLivePhase::ScrubbedBack { .. } => return LiveLogCommit::default(),
            SimLivePhase::CatchingToWall { .. } => {
                if self
                    .sealed_tip_ordinal
                    .is_some_and(|tip| self.cursor_ordinal <= tip)
                {
                    return LiveLogCommit::default();
                }
                self.sealed_tip_ordinal = None;
            }
            SimLivePhase::CatchingUp { .. } | SimLivePhase::WallPinned { .. } => {}
        }
        self.tip_ts = self.tip_ts.max(event.ts.0);
        self.tip_ordinal = self.cursor_ordinal;
        let log = self
            .live_log
            .as_mut()
            .expect("sim-live append after live log closed");
        log.append(event, book, profile, now)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scrubbed_state(tip_ts: u64, tip_ordinal: u64, cursor_ordinal: u64) -> SimLiveState {
        let mut state = SimLiveState {
            head_ts: tip_ts,
            phase: SimLivePhase::ScrubbedBack {
                wall_at_head: Instant::now(),
            },
            tip_ts,
            tip_ordinal,
            cursor_ordinal,
            sealed_tip_ordinal: None,
            live_log: None,
        };
        state.seal_tip();
        state
    }

    #[test]
    fn scrub_transport_refuses_unsealed_same_ts_suffix() {
        // Wall-pin stopped mid burst (tip_ordinal=7); later events share tip_ts.
        // At the sealed cursor, ts-only gating would wrongly allow Play.
        let state = scrubbed_state(1_000, 7, 7);
        assert!(!state.scrub_transport_allows(1_000));
        assert!(!state.scrub_transport_allows(999));
    }

    #[test]
    fn scrub_transport_allows_sealed_prefix_then_stops() {
        let mut state = scrubbed_state(1_000, 7, 5);
        assert!(state.scrub_transport_allows(1_000));
        state.cursor_ordinal = 6;
        assert!(state.scrub_transport_allows(1_000));
        state.cursor_ordinal = 7;
        assert!(!state.scrub_transport_allows(1_000));
    }

    #[test]
    fn scrub_transport_keeps_tip_ts_as_secondary_bound() {
        let state = scrubbed_state(1_000, 7, 3);
        assert!(state.scrub_transport_allows(1_000));
        assert!(!state.scrub_transport_allows(1_001));
    }
}
