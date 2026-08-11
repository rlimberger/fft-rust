//! Pure replay-transport state (PRD §5 chrome + M5).
//!
//! No GPUI, no engine I/O. Shell maps [`TransportCommand`] → [`fft_engine::EngineCmd`]
//! and drains scrub seeks once per frame (latest-wins).

use jiff::Timestamp;
use jiff::civil::Date;

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

/// `l` placeholder until M6 GoLive.
pub const GO_LIVE_HINT: &str = "go-live: M6";

const CT: &str = "America/Chicago";

/// Engine-facing commands produced by transport input (no `GoLive`).
#[derive(Debug, Clone, PartialEq)]
pub enum TransportCommand {
    Play,
    Pause,
    SetSpeed(f64),
    Seek { ts: u64, generation: u64 },
}

/// Result of a pure input mapping.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct TransportAction {
    /// Engine commands to send (order preserved; typically 0–1).
    pub commands: Vec<TransportCommand>,
    /// Whether the shell should refresh (mode/strip/labels changed).
    pub refresh: bool,
    /// Operator-visible status line (e.g. go-live placeholder).
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

impl TransportState {
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

    /// `l`: no-op until M6; surface a status hint.
    pub fn go_live_placeholder(&mut self) -> TransportAction {
        if !self.mode_on {
            return TransportAction::default();
        }
        self.status_hint = Some(GO_LIVE_HINT);
        TransportAction {
            commands: Vec::new(),
            refresh: true,
            status_hint: Some(GO_LIVE_HINT),
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
    let close = open
        .checked_add(86_400 * 1_000_000_000)
        .expect("fft-ui: session close overflow");
    (open, close)
}

/// 17:00 America/Chicago of the calendar day before `trade_date_days`, as UTC ns.
pub fn session_open_ns(trade_date_days: u32) -> u64 {
    assert!(
        trade_date_days > 0,
        "trade date must follow Unix epoch day zero"
    );
    // jiff's from_unix_epoch_day is crate-private; use our civil_from_days pair.
    let (year, month, day) = crate::datetime::civil_from_days(i64::from(trade_date_days));
    let year = i16::try_from(year)
        .unwrap_or_else(|_| panic!("fft-ui: trade_date year {year} out of jiff range"));
    let month = i8::try_from(month).expect("fft-ui: month fits i8");
    let day = i8::try_from(day).expect("fft-ui: day fits i8");
    let date = Date::new(year, month, day)
        .unwrap_or_else(|err| panic!("fft-ui: invalid trade_date {trade_date_days}: {err}"));
    let prior = date
        .yesterday()
        .unwrap_or_else(|err| panic!("fft-ui: no day before {date}: {err}"));
    let zoned = prior
        .at(17, 0, 0, 0)
        .in_tz(CT)
        .unwrap_or_else(|err| panic!("fft-ui: cannot zone {prior} 17:00 {CT}: {err}"));
    let ns = zoned.timestamp().as_nanosecond();
    u64::try_from(ns).unwrap_or_else(|_| panic!("fft-ui: session open {zoned} before epoch"))
}

/// Format `applied_ts` (UTC ns) as America/Chicago `HH:MM:SS`.
pub fn format_ct_clock(ts_ns: u64) -> String {
    if ts_ns == 0 {
        return "--:--:--".to_string();
    }
    let ts = Timestamp::from_nanosecond(i128::from(ts_ns))
        .unwrap_or_else(|err| panic!("fft-ui: applied_ts {ts_ns} outside jiff range: {err}"));
    let zoned = ts
        .in_tz(CT)
        .unwrap_or_else(|err| panic!("fft-ui: tz database missing {CT}: {err}"));
    let t = zoned.time();
    format!("{:02}:{:02}:{:02}", t.hour(), t.minute(), t.second())
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
mod tests {
    use super::*;

    #[test]
    fn toggle_mode_does_not_emit_commands_or_pause() {
        let mut t = TransportState::default();
        assert!(!t.mode_on);
        assert!(t.playing);
        let a = t.toggle_mode();
        assert!(t.mode_on);
        assert!(t.playing, "entering replay mode must not pause");
        assert!(a.commands.is_empty());
        assert!(a.refresh);
        let a = t.toggle_mode();
        assert!(!t.mode_on);
        assert!(a.refresh);
    }

    #[test]
    fn play_pause_only_when_mode_on() {
        let mut t = TransportState::default();
        assert!(t.toggle_play().commands.is_empty());
        t.toggle_mode();
        let a = t.toggle_play();
        assert!(!t.playing);
        assert_eq!(a.commands, vec![TransportCommand::Pause]);
        let a = t.toggle_play();
        assert!(t.playing);
        assert_eq!(a.commands, vec![TransportCommand::Play]);
    }

    #[test]
    fn speed_clamps_at_ladder_ends() {
        let mut t = TransportState::default();
        t.toggle_mode();
        assert_eq!(t.speed(), 1.0);
        // Down to floor.
        for _ in 0..8 {
            t.speed_down();
        }
        assert_eq!(t.speed_index, 0);
        assert_eq!(t.speed(), 0.25);
        let a = t.speed_down();
        assert!(a.commands.is_empty(), "floor is a no-op");
        // Up to ceiling.
        for _ in 0..16 {
            t.speed_up();
        }
        assert_eq!(t.speed_index, SPEED_LADDER.len() - 1);
        assert_eq!(t.speed(), 64.0);
        let a = t.speed_up();
        assert!(a.commands.is_empty(), "ceiling is a no-op");
        // One step down emits SetSpeed.
        let a = t.speed_down();
        assert_eq!(a.commands, vec![TransportCommand::SetSpeed(16.0)]);
    }

    #[test]
    fn speed_ignored_when_mode_off() {
        let mut t = TransportState::default();
        assert!(t.speed_up().commands.is_empty());
        assert!(t.speed_down().commands.is_empty());
        assert_eq!(t.speed_index, DEFAULT_SPEED_INDEX);
    }

    #[test]
    fn scrub_coalesces_to_one_seek_with_last_position() {
        let mut t = TransportState::default();
        t.toggle_mode();
        let first = 1_000u64;
        let last = 2_000u64;
        t.begin_scrub(0.0, 0.0, 100.0, first, last);
        t.queue_scrub(25.0, 0.0, 100.0, first, last);
        t.queue_scrub(50.0, 0.0, 100.0, first, last);
        t.queue_scrub(90.0, 0.0, 100.0, first, last);
        t.end_scrub();
        let cmd = t.take_coalesced_seek().expect("one pending seek");
        match cmd {
            TransportCommand::Seek { ts, generation } => {
                assert_eq!(generation, FIRST_UI_SEEK_GENERATION);
                assert_eq!(ts, scrub_ts_from_x(90.0, 0.0, 100.0, first, last));
            }
            other => panic!("expected Seek, got {other:?}"),
        }
        assert!(t.take_coalesced_seek().is_none(), "second drain is empty");
    }

    #[test]
    fn seek_generation_is_monotonic_from_two() {
        let mut t = TransportState::default();
        t.toggle_mode();
        assert_eq!(t.next_seek_generation(), FIRST_UI_SEEK_GENERATION);
        let a = t.step(5_000_000_000, 0, 10_000_000_000, true);
        match &a.commands[..] {
            [TransportCommand::Seek { generation: 2, .. }] => {}
            other => panic!("expected gen 2, got {other:?}"),
        }
        let a = t.step(6_000_000_000, 0, 10_000_000_000, true);
        match &a.commands[..] {
            [TransportCommand::Seek { generation: 3, .. }] => {}
            other => panic!("expected gen 3, got {other:?}"),
        }
        assert_eq!(t.next_seek_generation(), 4);
    }

    #[test]
    fn step_arithmetic_clamps_to_range() {
        let mut t = TransportState::default();
        t.toggle_mode();
        let first = 10 * STEP_NS;
        let last = 20 * STEP_NS;
        // Backward from first is no-op.
        let a = t.step(first, first, last, false);
        assert!(a.commands.is_empty());
        // Forward one second.
        let a = t.step(first, first, last, true);
        match &a.commands[..] {
            [TransportCommand::Seek { ts, .. }] => assert_eq!(*ts, first + STEP_NS),
            other => panic!("expected step seek, got {other:?}"),
        }
        // Forward past last clamps.
        let a = t.step(last, first, last, true);
        assert!(a.commands.is_empty());
        let a = t.step(last - STEP_NS / 2, first, last, true);
        match &a.commands[..] {
            [TransportCommand::Seek { ts, .. }] => assert_eq!(*ts, last),
            other => panic!("expected clamp to last, got {other:?}"),
        }
    }

    #[test]
    fn go_live_is_hint_only() {
        let mut t = TransportState::default();
        assert!(t.go_live_placeholder().status_hint.is_none());
        t.toggle_mode();
        let a = t.go_live_placeholder();
        assert!(a.commands.is_empty());
        assert_eq!(a.status_hint, Some(GO_LIVE_HINT));
        assert_eq!(t.status_hint, Some(GO_LIVE_HINT));
    }

    #[test]
    fn scrub_ts_from_x_endpoints() {
        let first = 100u64;
        let last = 200u64;
        assert_eq!(scrub_ts_from_x(0.0, 0.0, 100.0, first, last), first);
        assert_eq!(scrub_ts_from_x(100.0, 0.0, 100.0, first, last), last);
        assert_eq!(scrub_ts_from_x(50.0, 0.0, 100.0, first, last), 150);
        // Out of track clamps.
        assert_eq!(scrub_ts_from_x(-10.0, 0.0, 100.0, first, last), first);
        assert_eq!(scrub_ts_from_x(999.0, 0.0, 100.0, first, last), last);
    }

    #[test]
    fn session_range_wed_sample_is_cdt_open() {
        // Trade date 2026-07-29 = day 20_663; open = 2026-07-28 17:00 CDT = 22:00 UTC.
        let (open, close) = session_range_ns(20_663);
        assert_eq!(open, 1_785_276_000_000_000_000);
        assert_eq!(close, open + 86_400 * 1_000_000_000);
        // PRD §6 anchor 13:50Z sits inside the range.
        let anchor = 1_785_333_000_000_000_000u64;
        assert!(anchor > open && anchor < close);
    }

    #[test]
    fn format_ct_clock_anchor() {
        // 2026-07-29T13:50:00Z = 08:50:00 CDT.
        assert_eq!(format_ct_clock(1_785_333_000_000_000_000), "08:50:00");
        assert_eq!(format_ct_clock(0), "--:--:--");
    }

    #[test]
    fn format_speed_labels() {
        assert_eq!(format_speed(1.0), "×1");
        assert_eq!(format_speed(0.25), "×0.25");
        assert_eq!(format_speed(64.0), "×64");
    }

    #[test]
    fn queue_without_begin_is_ignored() {
        let mut t = TransportState::default();
        t.toggle_mode();
        t.queue_scrub(50.0, 0.0, 100.0, 0, 100);
        assert!(t.take_coalesced_seek().is_none());
    }
}
