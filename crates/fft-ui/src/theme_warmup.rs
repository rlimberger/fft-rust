//! Pending OS-theme adoption with UI-thread glyph pre-shape.
//!
//! GlyphCache keys include text + color bits + font size, so both scale and
//! palette-only switches miss cold. Detection and adoption are split: render
//! stays on the old theme while a bounded per-frame budget warms the shared
//! cache, then adoption is atomic.

use std::sync::Arc;
use std::time::{Duration, Instant};

use fft_engine::RenderSnapshot;
use gpui::{Hsla, Pixels, Window, px};

use crate::dom_view::DomView;
use crate::glyph_cache::GlyphCache;
use crate::layout::{COL_LABELS, format_price, format_size, header_h, max_visible_rows};
use crate::mp_layout::max_rows as mp_max_rows;
use crate::mp_view::{current_session, session_open_footer, visible_rows};
use crate::os_theme::ThemeSnapshot;
use crate::theme::Palette;

/// Per-frame shaping budget while a theme is pending (time, never count).
pub const WARM_FRAME_BUDGET: Duration = Duration::from_millis(2);

/// Hard cap on warm frames before forced adoption (bound, not a hang).
pub const MAX_WARM_FRAMES: u8 = 8;

/// One glyph key to pre-shape into the shared [`GlyphCache`].
#[derive(Clone, Debug, PartialEq)]
pub struct GlyphJob {
    pub text: String,
    pub color: Hsla,
    pub font_size: Pixels,
}

/// Pending theme waiting for a warm cache before atomic adoption.
#[derive(Clone, Debug)]
pub struct PendingTheme {
    pub snap: Arc<ThemeSnapshot>,
    pub warm_frames: u8,
    pub warmed_entries: u64,
    pub queue: Vec<GlyphJob>,
    pub cursor: usize,
}

impl PendingTheme {
    pub fn new(snap: Arc<ThemeSnapshot>) -> Self {
        Self {
            snap,
            warm_frames: 0,
            warmed_entries: 0,
            queue: Vec::new(),
            cursor: 0,
        }
    }

    /// Install the visible glyph set once. Later frames keep cursor progress.
    ///
    /// If the first install was empty (snapshot not yet populated), a later
    /// non-empty set replaces it. Mid-warm queue swaps are avoided so a 2 ms
    /// budget can drain across frames.
    pub fn ensure_queue(&mut self, queue: Vec<GlyphJob>) {
        if self.queue.is_empty() {
            self.queue = queue;
            self.cursor = 0;
        }
    }

    pub fn remaining(&self) -> usize {
        self.queue.len().saturating_sub(self.cursor)
    }

    pub fn fully_warmed(&self) -> bool {
        !self.queue.is_empty() && self.cursor >= self.queue.len()
    }
}

/// Outcome of one warm/adopt step (detection is separate).
#[derive(Clone, Debug, PartialEq)]
pub enum ThemeWarmAction {
    /// No pending theme.
    Idle,
    /// Still warming; keep rendering the old theme.
    KeepPending,
    /// Adopt the warmed snapshot now (queue drained or hard-capped).
    ///
    /// Carries the pending snap itself — never re-load the slot on adopt; the
    /// watcher can publish a newer generation between detect and adopt.
    Adopt {
        snap: Arc<ThemeSnapshot>,
        warm_frames_used: u8,
        warmed_entries: u64,
    },
}

/// Pure warm/adopt step. `shape_batch` returns newly inserted cache entries.
///
/// Empty queues do **not** adopt immediately — the shell refreshes the visible
/// set each frame; adoption waits until the queue drains or the hard cap hits.
pub fn drive_theme_warmup(
    pending: &mut Option<PendingTheme>,
    mut shape_batch: impl FnMut(&mut PendingTheme, Duration) -> u64,
) -> ThemeWarmAction {
    let Some(pend) = pending.as_mut() else {
        return ThemeWarmAction::Idle;
    };

    if !pend.fully_warmed() && pend.warm_frames < MAX_WARM_FRAMES {
        let shaped = shape_batch(pend, WARM_FRAME_BUDGET);
        pend.warmed_entries = pend.warmed_entries.saturating_add(shaped);
        pend.warm_frames = pend.warm_frames.saturating_add(1);
    }

    if pend.fully_warmed() || pend.warm_frames >= MAX_WARM_FRAMES {
        let pend = pending.take().expect("pending checked above");
        ThemeWarmAction::Adopt {
            snap: pend.snap,
            warm_frames_used: pend.warm_frames,
            warmed_entries: pend.warmed_entries,
        }
    } else {
        ThemeWarmAction::KeepPending
    }
}

/// Latest-wins pending install when the slot generation advances.
pub fn note_theme_slot_advance(
    pending: &mut Option<PendingTheme>,
    slot_generation: u64,
    observed_generation: u64,
    load: impl FnOnce() -> Arc<ThemeSnapshot>,
) -> bool {
    if slot_generation == observed_generation {
        return false;
    }
    let snap = load();
    *pending = Some(PendingTheme::new(snap));
    true
}

/// Shape queued jobs through `GlyphCache::get_or_shape` until the time budget elapses.
pub fn shape_pending_batch(
    pending: &mut PendingTheme,
    cache: &mut GlyphCache,
    window: &mut Window,
    budget: Duration,
) -> u64 {
    let started = Instant::now();
    let misses_before = cache.misses();
    while pending.cursor < pending.queue.len() {
        if started.elapsed() >= budget {
            break;
        }
        let job = &pending.queue[pending.cursor];
        let _ = cache.get_or_shape(window, job.text.as_str(), job.color, job.font_size);
        pending.cursor += 1;
    }
    cache.misses().saturating_sub(misses_before)
}

/// Collect the visible digit/price/size + header/footer glyph set at `palette`/`scale`.
///
/// Matches the DOM/MP pane formatters and font sizes used on the paint path. TPO
/// glyph runs are width-dependent and out of this warm-up set (brief: digits /
/// price / size + header/footer labels).
pub fn collect_visible_glyph_jobs(
    snapshot: &RenderSnapshot,
    center: Option<fft_core::Price>,
    mp_tick_scale: u8,
    dom_tick_scale: u8,
    palette: &Palette,
    scale: f32,
    viewport_height: f32,
) -> Vec<GlyphJob> {
    let mut jobs = Vec::new();
    let text = palette.text;
    let subtext = palette.subtext;

    let dom_font = px(12.0 * scale);
    for label in COL_LABELS {
        push_job(&mut jobs, label, subtext, dom_font);
    }

    let dom_view = DomView {
        anchor: center,
        tick_scale: dom_tick_scale,
    };
    let dom = dom_view.aggregate(&snapshot.dom);
    let body_h = (viewport_height - header_h(scale)).max(0.0);
    let max_dom = max_visible_rows(body_h, scale);
    let range = dom_view.window_range(&dom, max_dom);
    for row in &dom.rows[range] {
        push_job(&mut jobs, format_price(row.price.0), text, dom_font);
        for size in [
            row.session_volume,
            row.bid_size,
            row.cb,
            row.ca,
            row.ask_size,
        ] {
            let s = format_size(size);
            if !s.is_empty() {
                push_job(&mut jobs, s, text, dom_font);
            }
        }
    }

    if let Some(session) = current_session(&snapshot.profile) {
        let profile = visible_rows(
            session,
            snapshot.dom.tick_size,
            mp_tick_scale,
            center,
            mp_max_rows(viewport_height, scale),
        );
        let price_font = px(10.0 * scale);
        let size_font = px(9.0 * scale);
        for row in &profile.rows {
            push_job(&mut jobs, format_price(row.price.0), text, price_font);
            for value in [row.period_volume, row.session_volume] {
                let s = format_size(value);
                if !s.is_empty() {
                    push_job(&mut jobs, s, text, size_font);
                }
            }
        }
        push_job(
            &mut jobs,
            session_open_footer(session.trade_date),
            text,
            px(11.0 * scale),
        );
    }

    jobs
}

fn push_job(jobs: &mut Vec<GlyphJob>, text: impl Into<String>, color: Hsla, font_size: Pixels) {
    jobs.push(GlyphJob {
        text: text.into(),
        color,
        font_size,
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::theme::Palette;

    fn snap(generation: u64, scale: f32) -> Arc<ThemeSnapshot> {
        Arc::new(ThemeSnapshot {
            palette: Palette::mocha(),
            scale,
            generation,
        })
    }

    fn job(text: &str) -> GlyphJob {
        GlyphJob {
            text: text.into(),
            color: Hsla::default(),
            font_size: px(12.0),
        }
    }

    #[test]
    fn idle_without_pending() {
        let mut pending = None;
        assert_eq!(
            drive_theme_warmup(&mut pending, |_, _| 0),
            ThemeWarmAction::Idle
        );
    }

    #[test]
    fn empty_queue_does_not_adopt_until_hard_cap() {
        let expected = snap(4, 1.2);
        let mut pending = Some(PendingTheme::new(Arc::clone(&expected)));
        for frame in 1..=MAX_WARM_FRAMES {
            let action = drive_theme_warmup(&mut pending, |_, _| 0);
            if frame < MAX_WARM_FRAMES {
                assert_eq!(action, ThemeWarmAction::KeepPending);
                assert_eq!(pending.as_ref().unwrap().warm_frames, frame);
            } else {
                match action {
                    ThemeWarmAction::Adopt {
                        snap,
                        warm_frames_used,
                        warmed_entries,
                    } => {
                        assert!(Arc::ptr_eq(&snap, &expected));
                        assert_eq!(snap.generation, 4);
                        assert!((snap.scale - 1.2).abs() < 1e-6);
                        assert_eq!(warm_frames_used, MAX_WARM_FRAMES);
                        assert_eq!(warmed_entries, 0);
                    }
                    other => panic!("expected Adopt, got {other:?}"),
                }
                assert!(pending.is_none());
            }
        }
    }

    #[test]
    fn advances_until_queue_drained() {
        let expected = snap(2, 1.0);
        let mut pending = Some(PendingTheme {
            snap: Arc::clone(&expected),
            warm_frames: 0,
            warmed_entries: 0,
            queue: vec![job("a"), job("b")],
            cursor: 0,
        });
        let action = drive_theme_warmup(&mut pending, |pend, _| {
            if pend.cursor < pend.queue.len() {
                pend.cursor += 1;
                1
            } else {
                0
            }
        });
        assert_eq!(action, ThemeWarmAction::KeepPending);
        assert_eq!(pending.as_ref().unwrap().warm_frames, 1);
        assert_eq!(pending.as_ref().unwrap().warmed_entries, 1);
        assert_eq!(pending.as_ref().unwrap().remaining(), 1);

        let action = drive_theme_warmup(&mut pending, |pend, _| {
            if pend.cursor < pend.queue.len() {
                pend.cursor += 1;
                1
            } else {
                0
            }
        });
        match action {
            ThemeWarmAction::Adopt {
                snap,
                warm_frames_used,
                warmed_entries,
            } => {
                assert!(Arc::ptr_eq(&snap, &expected));
                assert_eq!(snap.generation, 2);
                assert_eq!(warm_frames_used, 2);
                assert_eq!(warmed_entries, 2);
            }
            other => panic!("expected Adopt, got {other:?}"),
        }
        assert!(pending.is_none());
    }

    #[test]
    fn hard_cap_forces_adopt_with_remainder() {
        let expected = snap(9, 1.0);
        let mut pending = Some(PendingTheme {
            snap: Arc::clone(&expected),
            warm_frames: 0,
            warmed_entries: 0,
            queue: (0..20).map(|i| job(&i.to_string())).collect(),
            cursor: 0,
        });
        for _ in 0..MAX_WARM_FRAMES {
            let action = drive_theme_warmup(&mut pending, |pend, _| {
                let _ = pend;
                0
            });
            if pending.is_some() {
                assert_eq!(action, ThemeWarmAction::KeepPending);
            } else {
                match action {
                    ThemeWarmAction::Adopt {
                        snap,
                        warm_frames_used,
                        warmed_entries,
                    } => {
                        assert!(Arc::ptr_eq(&snap, &expected));
                        assert_eq!(snap.generation, 9);
                        assert_eq!(warm_frames_used, MAX_WARM_FRAMES);
                        assert_eq!(warmed_entries, 0);
                    }
                    other => panic!("expected Adopt, got {other:?}"),
                }
            }
        }
        assert!(pending.is_none());
    }

    #[test]
    fn newer_pending_replaces_older_latest_wins() {
        let mut pending = Some(PendingTheme {
            snap: snap(2, 1.0),
            warm_frames: 3,
            warmed_entries: 7,
            queue: vec![job("old")],
            cursor: 0,
        });
        let replaced = note_theme_slot_advance(&mut pending, 5, 1, || snap(5, 1.5));
        assert!(replaced);
        let pend = pending.as_ref().unwrap();
        assert_eq!(pend.snap.generation, 5);
        assert!((pend.snap.scale - 1.5).abs() < 1e-6);
        assert_eq!(pend.warm_frames, 0);
        assert_eq!(pend.warmed_entries, 0);
        assert!(pend.queue.is_empty());
    }

    #[test]
    fn ensure_queue_keeps_cursor_progress() {
        let mut pend = PendingTheme::new(snap(1, 1.0));
        pend.ensure_queue(vec![job("a"), job("b"), job("c")]);
        pend.cursor = 2;
        pend.ensure_queue(vec![job("x"), job("y")]); // ignored — already installed
        assert_eq!(pend.queue[0].text, "a");
        assert_eq!(pend.cursor, 2);

        let mut empty = PendingTheme::new(snap(1, 1.0));
        empty.ensure_queue(Vec::new());
        empty.ensure_queue(vec![job("late")]);
        assert_eq!(empty.queue.len(), 1);
        assert_eq!(empty.queue[0].text, "late");
    }

    #[test]
    fn slot_advance_notes_pending() {
        let mut pending = None;
        assert!(note_theme_slot_advance(&mut pending, 2, 1, || snap(2, 1.0)));
        assert!(!note_theme_slot_advance(&mut pending, 2, 2, || snap(
            2, 1.0
        )));
    }
}
