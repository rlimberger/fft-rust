use super::*;
use crate::layout::{COL_LABELS, HEADER_H, ROW_H, format_price};
use crate::theme::Palette;
use fft_core::Price;
use fft_engine::{DomPriceRow, DomRenderState, RenderSnapshot};

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

/// Fixed-point dollars → 1e-9 price units so `format_price` strings stay distinct.
fn dollars(units: i64) -> i64 {
    units
        .checked_mul(1_000_000_000)
        .expect("test price overflows i64")
}

fn dom_state(tick_dollars: i64, price_dollars: &[i64]) -> DomRenderState {
    DomRenderState {
        tick_size: Price(dollars(tick_dollars)),
        rows: price_dollars
            .iter()
            .map(|&p| DomPriceRow {
                price: Price(dollars(p)),
                ..DomPriceRow::default()
            })
            .collect(),
        ..DomRenderState::default()
    }
}

fn snapshot_with_dom(dom: DomRenderState) -> RenderSnapshot {
    RenderSnapshot {
        dom,
        ..RenderSnapshot::default()
    }
}

/// Viewport height that yields exactly `rows` DOM body rows at `scale`
/// (header strip + `rows * ROW_H * scale`), matching `dom_ladder_prepare`.
fn viewport_for_rows(rows: usize, scale: f32) -> f32 {
    HEADER_H * scale + rows as f32 * ROW_H * scale
}

fn expected_window_prices(
    dom: &DomRenderState,
    center: Option<Price>,
    tick_scale: u8,
    max_rows: usize,
) -> Vec<String> {
    let view = DomView {
        anchor: center,
        tick_scale,
    };
    let aggregated = view.aggregate_window(dom, max_rows);
    let range = view.window_range(&aggregated, max_rows);
    aggregated.rows[range]
        .iter()
        .map(|row| format_price(row.price.0))
        .collect()
}

fn warmed_dom_price_strings(jobs: &[GlyphJob], scale: f32) -> Vec<String> {
    let font = px(12.0 * scale);
    jobs.iter()
        .skip(COL_LABELS.len())
        // Prices always carry a decimal via `format_price`; sizes are bare integers.
        .filter(|job| job.font_size == font && job.text.contains('.'))
        .map(|job| job.text.clone())
        .collect()
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

#[test]
fn warm_budget_constants_unchanged() {
    assert_eq!(WARM_FRAME_BUDGET, Duration::from_millis(2));
    assert_eq!(MAX_WARM_FRAMES, 8);
}

fn assert_warmed_prices_match_aggregate_window(
    price_dollars: &[i64],
    center_dollars: i64,
    max_rows: usize,
) {
    let scale = 1.0;
    let tick_scale = 1;
    let dom = dom_state(1, price_dollars);
    let snapshot = snapshot_with_dom(dom.clone());
    let center = Some(Price(dollars(center_dollars)));
    let viewport_height = viewport_for_rows(max_rows, scale);
    let expected = expected_window_prices(&dom, center, tick_scale, max_rows);
    assert_eq!(expected.len(), max_rows);

    let jobs = collect_visible_glyph_jobs(
        &snapshot,
        center,
        1,
        tick_scale,
        &Palette::mocha(),
        scale,
        viewport_height,
    );
    let warmed = warmed_dom_price_strings(&jobs, scale);
    assert_eq!(warmed, expected);
}

#[test]
fn linked_warm_prices_match_aggregate_window_outside_above() {
    // Source 10..=12; outside-above center synthesizes the full painted lattice.
    assert_warmed_prices_match_aggregate_window(&[10, 11, 12], 20, 5);
}

#[test]
fn linked_warm_prices_match_aggregate_window_outside_below() {
    assert_warmed_prices_match_aggregate_window(&[10, 11, 12], 2, 5);
}

#[test]
fn linked_warm_prices_match_aggregate_window_near_low_edge() {
    // Inside near the low edge still synthesizes so the linked center stays centered.
    assert_warmed_prices_match_aggregate_window(&[10, 11, 12, 13, 14], 11, 5);
}

#[test]
fn linked_warm_prices_match_aggregate_window_near_high_edge() {
    assert_warmed_prices_match_aggregate_window(&[10, 11, 12, 13, 14], 13, 5);
}

#[test]
fn linked_warm_prices_match_aggregate_window_normal_in_range() {
    // Deep enough that `window_range` alone centers the anchor — no synthesis.
    assert_warmed_prices_match_aggregate_window(&[10, 11, 12, 13, 14, 15, 16], 13, 5);
}
