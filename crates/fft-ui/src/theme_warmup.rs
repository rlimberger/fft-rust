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
    // Match `dom_ladder_prepare`: body height under the header, then the same
    // linked-center lattice paint uses (`aggregate_window` + `window_range`).
    let body_h = (viewport_height - header_h(scale)).max(0.0);
    let max_dom = max_visible_rows(body_h, scale);
    let dom = dom_view.aggregate_window(&snapshot.dom, max_dom);
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
#[path = "theme_warmup_tests.rs"]
mod tests;
