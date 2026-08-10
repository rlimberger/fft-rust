//! Multi-session container: profiles keyed by CT trade date, current session
//! mutable, prior sessions frozen. Array-dense (a sorted `Vec`), sized to
//! tolerate a full fixture week without map overhead.

use crate::session::SessionProfile;
use crate::tpo::SessionClock;
use fft_core::{CanonicalEvent, Price};

/// Profiles for a run of sessions, ascending by CT trade date. Only the most
/// recent session accepts events; prior sessions are frozen by API shape
/// (shared references only).
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct MultiProfile {
    pub(crate) tick: Price,
    pub(crate) sessions: Vec<SessionProfile>,
}

impl MultiProfile {
    /// `tick` = instrument `min_price_increment` (1e-9 price units).
    pub fn new(tick: Price) -> MultiProfile {
        assert!(tick.0 > 0, "tick must be positive, got {tick:?}");
        MultiProfile {
            tick,
            sessions: Vec::new(),
        }
    }

    /// Open the session for `trade_date` (CT days since epoch, per
    /// [`fft_core::InstrumentMeta::trade_date`]) and freeze the prior one.
    /// Trade dates must strictly ascend.
    pub fn begin_session(&mut self, trade_date: u32) {
        if let Some(last) = self.sessions.last() {
            assert!(
                trade_date > last.trade_date(),
                "session {trade_date} must follow {}",
                last.trade_date()
            );
        }
        self.sessions.push(SessionProfile::new(
            SessionClock::for_trade_date(trade_date),
            self.tick,
        ));
    }

    /// Apply one canonical event to the current session. Panics if no session
    /// has been begun — routing events without a session is an engine bug.
    pub fn apply(&mut self, ev: &CanonicalEvent) {
        self.sessions
            .last_mut()
            .expect("apply before begin_session")
            .apply(ev);
    }

    /// The developing session, if any.
    pub fn current(&self) -> Option<&SessionProfile> {
        self.sessions.last()
    }

    /// Session by CT trade date.
    pub fn session(&self, trade_date: u32) -> Option<&SessionProfile> {
        self.sessions
            .binary_search_by_key(&trade_date, SessionProfile::trade_date)
            .ok()
            .map(|i| &self.sessions[i])
    }

    /// All sessions, ascending by trade date.
    pub fn sessions(&self) -> &[SessionProfile] {
        &self.sessions
    }

    pub fn tick(&self) -> Price {
        self.tick
    }
}
