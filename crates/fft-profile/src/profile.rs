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

    /// Insert a **completed** earlier-dated session while keeping ascending
    /// trade-date order. The current (replay/live) session remains last.
    ///
    /// Used by `EngineCmd::LoadPriorSession` after an offline prior-day build
    /// finishes (`docs/ENGINE.md` §2). Panics if there is no current session,
    /// if `session.trade_date()` is not strictly older than the current, if the
    /// date already exists, or if the tick disagrees.
    pub fn insert_prior_session(&mut self, session: SessionProfile) {
        assert_eq!(
            session.tick(),
            self.tick,
            "prior session tick {:?} disagrees with MultiProfile tick {:?}",
            session.tick(),
            self.tick
        );
        let current = self
            .sessions
            .last()
            .expect("insert_prior_session requires a current session");
        let date = session.trade_date();
        assert!(
            date < current.trade_date(),
            "prior session {date} must be older than current {}",
            current.trade_date()
        );
        match self
            .sessions
            .binary_search_by_key(&date, SessionProfile::trade_date)
        {
            Ok(_) => panic!("prior session {date} already present"),
            Err(index) => {
                // date < current ⇒ insertion index is strictly before last.
                debug_assert!(index < self.sessions.len());
                self.sessions.insert(index, session);
            }
        }
    }

    /// Drain every completed prior session, leaving only the current session
    /// (if any). Used by `SetSource` when the new source shares the trade date
    /// and completed priors must be retained (`docs/ENGINE.md` §2 rule 4).
    pub fn drain_prior_sessions(&mut self) -> Vec<SessionProfile> {
        if self.sessions.len() <= 1 {
            return Vec::new();
        }
        let current = self.sessions.pop().expect("len > 1");
        let priors = std::mem::take(&mut self.sessions);
        self.sessions.push(current);
        priors
    }

    /// Seed completed prior sessions before [`begin_session`]. Dates must be
    /// strictly ascending and the container must still be empty of a current
    /// session (i.e. `sessions` is empty). Used to reinstall retained priors
    /// across a same-date `SetSource`.
    pub fn seed_prior_sessions(&mut self, priors: Vec<SessionProfile>) {
        assert!(
            self.sessions.is_empty(),
            "seed_prior_sessions requires an empty MultiProfile"
        );
        let mut prev = None;
        for session in priors {
            assert_eq!(
                session.tick(),
                self.tick,
                "prior session tick {:?} disagrees with MultiProfile tick {:?}",
                session.tick(),
                self.tick
            );
            let date = session.trade_date();
            if let Some(p) = prev {
                assert!(date > p, "seeded prior sessions must strictly ascend");
            }
            prev = Some(date);
            self.sessions.push(session);
        }
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
