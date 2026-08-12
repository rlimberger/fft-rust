//! Pure replay-transport state (PRD §5 chrome + M5).
//!
//! No GPUI, no engine I/O. Shell maps [`TransportCommand`] → [`fft_engine::EngineCmd`]
//! and drains scrub seeks once per frame (latest-wins).

use std::sync::atomic::AtomicBool;

use fft_engine::LiveTransportPhase;
use jiff::civil::Date;

use crate::prefs::Prefs;

#[path = "transport_clock.rs"]
mod transport_clock;
use transport_clock::{CT, warn_once};
pub use transport_clock::{ensure_tzdb_available, format_zone_clock_ns};

/// Fixed speed ladder (PRD speeds via `[` / `]`).
pub const SPEED_LADDER: &[f64] = &[0.25, 0.5, 1.0, 2.0, 4.0, 8.0, 16.0, 64.0];

/// Index of `1.0` in [`SPEED_LADDER`].
pub const DEFAULT_SPEED_INDEX: usize = 2;

/// First seek generation the UI may issue after shell's `--replay-at` Seek(gen=1).
pub const FIRST_UI_SEEK_GENERATION: u64 = 2;

/// Provisional step size until product defines engine-level step (flag in track report).
pub const STEP_NS: u64 = 1_000_000_000;

/// Strip height in logical pixels at OS scale 1.0.
pub const TRANSPORT_H: f32 = 28.0;

/// `l` when transport mode is on but the engine is not in sim-live.
pub const GO_LIVE_NEEDS_SIM_LIVE: &str = "go-live: needs sim-live";

/// Engine-facing commands produced by transport input.
#[derive(Debug, Clone, PartialEq)]
pub enum TransportCommand {
    Play,
    Pause,
    SetSpeed(f64),
    Seek { ts: u64, generation: u64 },
    GoLive,
}

/// Result of a pure input mapping.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct TransportAction {
    /// Engine commands to send (order preserved; typically 0–1).
    pub commands: Vec<TransportCommand>,
    /// Whether the shell should refresh (mode/strip/labels changed).
    pub refresh: bool,
    /// Operator-visible status line (e.g. go-live needs sim-live).
    pub status_hint: Option<&'static str>,
}

/// Scrub drag: latest-wins pending target, drained once per frame.
#[derive(Debug, Clone, Default)]
struct ScrubState {
    dragging: bool,
    /// Latest drag target (kept for marker paint while dragging).
    pending_ts: Option<u64>,
    /// Set when `pending_ts` changes; cleared by [`TransportState::take_coalesced_seek`].
    dirty: bool,
}

/// Pure transport state owned by the shell via `RefCell`.
#[derive(Debug, Clone)]
pub struct TransportState {
    /// Strip visible / transport keys armed (`r`).
    pub mode_on: bool,
    /// UI mirror of play/pause (engine starts playing after SetSource).
    pub playing: bool,
    /// Index into [`SPEED_LADDER`].
    pub speed_index: usize,
    /// Next seek generation to stamp (starts at [`FIRST_UI_SEEK_GENERATION`]).
    next_seek_generation: u64,
    scrub: ScrubState,
    /// Last status hint set by input (cleared by caller if desired).
    pub status_hint: Option<&'static str>,
}

impl Default for TransportState {
    fn default() -> Self {
        Self {
            mode_on: false,
            playing: true,
            speed_index: DEFAULT_SPEED_INDEX,
            next_seek_generation: FIRST_UI_SEEK_GENERATION,
            scrub: ScrubState::default(),
            status_hint: None,
        }
    }
}

/// Fields of [`TransportState`] that survive across runs (prefs v1).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TransportPrefsSnapshot {
    pub speed_index: usize,
}

impl TransportState {
    /// Construct transport state from loaded prefs (index already clamped).
    pub fn from_prefs(prefs: &Prefs) -> Self {
        let mut state = Self::default();
        state.apply_prefs_snapshot(&TransportPrefsSnapshot {
            speed_index: prefs.transport_speed_index,
        });
        state
    }

    /// Snapshot of persisted transport fields for quit-time write.
    pub fn prefs_snapshot(&self) -> TransportPrefsSnapshot {
        TransportPrefsSnapshot {
            speed_index: self.speed_index,
        }
    }

    /// Apply a prefs snapshot (index clamped to the ladder).
    pub fn apply_prefs_snapshot(&mut self, snap: &TransportPrefsSnapshot) {
        let max = SPEED_LADDER.len().saturating_sub(1);
        self.speed_index = snap.speed_index.min(max);
    }

    /// Current ladder speed.
    pub fn speed(&self) -> f64 {
        SPEED_LADDER[self.speed_index]
    }

    /// Next generation that will be stamped on the next issued seek (test surface).
    pub fn next_seek_generation(&self) -> u64 {
        self.next_seek_generation
    }

    /// Whether a scrub drag is active.
    pub fn is_scrubbing(&self) -> bool {
        self.scrub.dragging
    }

    /// Pending scrub target (for marker paint while dragging), if any.
    pub fn pending_scrub_ts(&self) -> Option<u64> {
        self.scrub.pending_ts
    }

    /// `r`: toggle strip. Does **not** pause/play the engine.
    pub fn toggle_mode(&mut self) -> TransportAction {
        self.mode_on = !self.mode_on;
        TransportAction {
            commands: Vec::new(),
            refresh: true,
            status_hint: None,
        }
    }

    /// Space: play/pause when mode is on.
    pub fn toggle_play(&mut self) -> TransportAction {
        if !self.mode_on {
            return TransportAction::default();
        }
        self.playing = !self.playing;
        let cmd = if self.playing {
            TransportCommand::Play
        } else {
            TransportCommand::Pause
        };
        TransportAction {
            commands: vec![cmd],
            refresh: true,
            status_hint: None,
        }
    }

    /// `]`: speed up one ladder step (clamped).
    pub fn speed_up(&mut self) -> TransportAction {
        self.nudge_speed(1)
    }

    /// `[`: speed down one ladder step (clamped).
    pub fn speed_down(&mut self) -> TransportAction {
        self.nudge_speed(-1)
    }

    fn nudge_speed(&mut self, delta: i8) -> TransportAction {
        if !self.mode_on {
            return TransportAction::default();
        }
        let max = SPEED_LADDER.len() - 1;
        let next = match delta {
            d if d > 0 => self.speed_index.saturating_add(1).min(max),
            d if d < 0 => self.speed_index.saturating_sub(1),
            _ => self.speed_index,
        };
        if next == self.speed_index {
            return TransportAction {
                commands: Vec::new(),
                refresh: false,
                status_hint: None,
            };
        }
        self.speed_index = next;
        TransportAction {
            commands: vec![TransportCommand::SetSpeed(self.speed())],
            refresh: true,
            status_hint: None,
        }
    }

    /// Arrow step: provisional `applied_ts ± STEP_NS`, clamped to `[first, last]`.
    pub fn step(
        &mut self,
        applied_ts: u64,
        first_ts: u64,
        last_ts: u64,
        forward: bool,
    ) -> TransportAction {
        if !self.mode_on {
            return TransportAction::default();
        }
        let (lo, hi) = ordered_range(first_ts, last_ts);
        let target = if forward {
            applied_ts.saturating_add(STEP_NS).min(hi)
        } else {
            applied_ts.saturating_sub(STEP_NS).max(lo)
        };
        if target == applied_ts {
            return TransportAction::default();
        }
        let cmd = self.issue_seek(target);
        TransportAction {
            commands: vec![cmd],
            refresh: true,
            status_hint: None,
        }
    }

    /// `l`: emit GoLive when sim-live is active; otherwise a status hint.
    pub fn go_live(&mut self, live_phase: LiveTransportPhase) -> TransportAction {
        if !self.mode_on {
            return TransportAction::default();
        }
        if live_phase == LiveTransportPhase::Inactive {
            self.status_hint = Some(GO_LIVE_NEEDS_SIM_LIVE);
            return TransportAction {
                commands: Vec::new(),
                refresh: true,
                status_hint: Some(GO_LIVE_NEEDS_SIM_LIVE),
            };
        }
        self.status_hint = None;
        TransportAction {
            commands: vec![TransportCommand::GoLive],
            refresh: true,
            status_hint: None,
        }
    }

    /// Begin scrub drag at window-space `x` on a track of width `track_w` at `track_x`.
    pub fn begin_scrub(
        &mut self,
        x: f32,
        track_x: f32,
        track_w: f32,
        first_ts: u64,
        last_ts: u64,
    ) -> TransportAction {
        if !self.mode_on {
            return TransportAction::default();
        }
        self.scrub.dragging = true;
        self.scrub.pending_ts = Some(scrub_ts_from_x(x, track_x, track_w, first_ts, last_ts));
        self.scrub.dirty = true;
        TransportAction {
            commands: Vec::new(),
            refresh: true,
            status_hint: None,
        }
    }

    /// Queue a scrub position (latest-wins). Does not issue a seek.
    pub fn queue_scrub(&mut self, x: f32, track_x: f32, track_w: f32, first_ts: u64, last_ts: u64) {
        if !self.mode_on || !self.scrub.dragging {
            return;
        }
        self.scrub.pending_ts = Some(scrub_ts_from_x(x, track_x, track_w, first_ts, last_ts));
        self.scrub.dirty = true;
    }

    /// End scrub drag; a dirty pending target still drains on the next frame.
    pub fn end_scrub(&mut self) {
        self.scrub.dragging = false;
    }

    /// Drain at most one seek for the latest pending scrub target (frame boundary).
    ///
    /// Keeps `pending_ts` for marker paint while dragging; clears it after the
    /// post-release drain so the marker falls back to `applied_ts`.
    pub fn take_coalesced_seek(&mut self) -> Option<TransportCommand> {
        if !self.scrub.dirty {
            return None;
        }
        self.scrub.dirty = false;
        let ts = self.scrub.pending_ts?;
        if !self.scrub.dragging {
            self.scrub.pending_ts = None;
        }
        Some(self.issue_seek(ts))
    }

    fn issue_seek(&mut self, ts: u64) -> TransportCommand {
        let generation = self.next_seek_generation;
        self.next_seek_generation = self
            .next_seek_generation
            .checked_add(1)
            .expect("fft-ui: seek generation overflow");
        TransportCommand::Seek { ts, generation }
    }
}

/// Map a horizontal pointer position to a timestamp on `[first, last]`.
pub fn scrub_ts_from_x(x: f32, track_x: f32, track_w: f32, first_ts: u64, last_ts: u64) -> u64 {
    let (lo, hi) = ordered_range(first_ts, last_ts);
    if !(track_w.is_finite() && track_w > 0.0) || hi == lo {
        return lo;
    }
    let frac = ((x - track_x) / track_w).clamp(0.0, 1.0);
    let span = hi - lo;
    let offset = (f64::from(frac) * span as f64).round() as u64;
    lo.saturating_add(offset).min(hi)
}

/// Marker x for a timestamp on a track (paint).
pub fn scrub_x_from_ts(ts: u64, track_x: f32, track_w: f32, first_ts: u64, last_ts: u64) -> f32 {
    let (lo, hi) = ordered_range(first_ts, last_ts);
    if !(track_w.is_finite() && track_w > 0.0) || hi == lo {
        return track_x;
    }
    let frac = (ts.saturating_sub(lo) as f64 / (hi - lo) as f64).clamp(0.0, 1.0) as f32;
    track_x + frac * track_w
}

fn ordered_range(first: u64, last: u64) -> (u64, u64) {
    if first <= last {
        (first, last)
    } else {
        (last, first)
    }
}

/// Globex session scrub range for a CT trade date (days since Unix epoch).
///
/// Engine does not expose log `[first_ts, last_ts]` on the snapshot today, so the
/// scrub bar uses doctrine session bounds: 17:00 CT of the prior calendar day
/// through +24 h (next Globex open). Matches `fft-ingest::session`.
pub fn session_range_ns(trade_date_days: u32) -> (u64, u64) {
    let open = session_open_ns(trade_date_days);
    // Soft-fail sentinel from `session_open_ns` (and pre-epoch opens).
    if open == 0 {
        return (0, 1);
    }
    let close = open
        .checked_add(86_400 * 1_000_000_000)
        .expect("fft-ui: session close overflow");
    (open, close)
}

/// 17:00 America/Chicago of the calendar day before `trade_date_days`, as UTC ns.
///
/// Contract: `trade_date_days > 0` stays an assert. Environmental tz/date failures
/// soft-fail to `(0)` with a loud once-per-cause warning (callers use `(0,1)` range).
pub fn session_open_ns(trade_date_days: u32) -> u64 {
    assert!(
        trade_date_days > 0,
        "trade date must follow Unix epoch day zero"
    );
    ensure_tzdb_available();
    static WARNED: AtomicBool = AtomicBool::new(false);
    // jiff's from_unix_epoch_day is crate-private; use our civil_from_days pair.
    let (year, month, day) = crate::datetime::civil_from_days(i64::from(trade_date_days));
    let Ok(year) = i16::try_from(year) else {
        warn_once(
            &WARNED,
            format!(
                "fft-ui: WARNING trade_date year {year} out of jiff range; scrub range soft-fails to (0,1)"
            ),
        );
        return 0;
    };
    let Ok(month) = i8::try_from(month) else {
        warn_once(
            &WARNED,
            format!(
                "fft-ui: WARNING trade_date month {month} invalid; scrub range soft-fails to (0,1)"
            ),
        );
        return 0;
    };
    let Ok(day) = i8::try_from(day) else {
        warn_once(
            &WARNED,
            format!(
                "fft-ui: WARNING trade_date day {day} invalid; scrub range soft-fails to (0,1)"
            ),
        );
        return 0;
    };
    let Ok(date) = Date::new(year, month, day) else {
        warn_once(
            &WARNED,
            format!(
                "fft-ui: WARNING invalid trade_date {trade_date_days}; scrub range soft-fails to (0,1)"
            ),
        );
        return 0;
    };
    let Ok(prior) = date.yesterday() else {
        warn_once(
            &WARNED,
            format!("fft-ui: WARNING no day before {date}; scrub range soft-fails to (0,1)"),
        );
        return 0;
    };
    let Ok(zoned) = prior.at(17, 0, 0, 0).in_tz(CT) else {
        warn_once(
            &WARNED,
            format!(
                "fft-ui: WARNING cannot zone {prior} 17:00 {CT}; scrub range soft-fails to (0,1)"
            ),
        );
        return 0;
    };
    let ns = zoned.timestamp().as_nanosecond();
    match u64::try_from(ns) {
        Ok(ns) => ns,
        Err(_) => {
            warn_once(
                &WARNED,
                format!(
                    "fft-ui: WARNING session open {zoned} before epoch; scrub range soft-fails to (0,1)"
                ),
            );
            0
        }
    }
}

/// Format `applied_ts` (UTC ns) as America/Chicago `HH:MM:SS`.
pub fn format_ct_clock(ts_ns: u64) -> String {
    format_zone_clock_ns(i128::from(ts_ns), CT)
}

/// Format speed for the strip label (`×1`, `×0.25`, …).
pub fn format_speed(speed: f64) -> String {
    if (speed - speed.round()).abs() < 1e-9 && speed >= 1.0 {
        format!("×{}", speed as i64)
    } else {
        // Trim trailing zeros without pulling extra deps.
        let s = format!("×{speed}");
        if s.contains('.') {
            s.trim_end_matches('0').trim_end_matches('.').to_string()
        } else {
            s
        }
    }
}

/// Play/pause glyph for the strip.
pub fn play_glyph(playing: bool) -> &'static str {
    if playing { "■" } else { "▶" }
}

/// Scaled strip height.
#[inline]
pub fn transport_h(scale: f32) -> f32 {
    TRANSPORT_H * scale
}

#[cfg(test)]
#[path = "transport_tests.rs"]
mod tests;
