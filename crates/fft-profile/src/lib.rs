//! Session Market Profile engine consuming [`fft_core::CanonicalEvent`].
//!
//! Per CT-trade-date session: dense per-price arrays for TPO letter marks (dual
//! ETH/RTH lettering — ETH letters run A… from the Globex open across the whole
//! session, RTH restarts at A at 08:30 CT; the pane chooses), volume-at-price,
//! developing-period volume (PV feed), session volume spectrum (SV feed);
//! derived VPOC / value area / VAH / VAL / Initial Balance / session range;
//! CVD candles per 30-minute period plus Grady cB/cA traded-at-touch counters
//! that reset on price change.
//!
//! Checkpointing: this crate owns the PROFILE (id 3), CVD (id 4), and SESSION
//! (id 6) checkpoint section byte layouts (`docs/FFTLOG-V2.md` §5,
//! `docs/ENGINE.md` §4). [`MultiProfile::restore`] reconstructs complete state
//! directly from section bytes — never by replaying synthetic events.

#![forbid(unsafe_code)]

mod cvd;
mod profile;
mod serialize;
mod session;
mod tpo;
mod value_area;

pub use cvd::{Cvd, CvdCandle, TouchCounter};
pub use profile::MultiProfile;
pub use serialize::{
    CVD_SECTION_ID, CVD_SECTION_VERSION, PROFILE_SECTION_ID, PROFILE_SECTION_VERSION,
    ProfileSections, RestoreError, SESSION_SECTION_ID, SESSION_SECTION_VERSION,
};
pub use session::{ProfileRow, SessionProfile};
pub use tpo::{
    ETH_PERIOD_COUNT, PERIOD_NS, RTH_PERIOD_COUNT, SessionClock, period_letter, tpo_letters,
};
