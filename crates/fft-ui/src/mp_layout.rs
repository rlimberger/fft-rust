//! Pure Market Profile strip and multi-session geometry.

/// Row height at OS scale 1.0.
pub const MP_ROW_H: f32 = 16.0;
/// Footer height at OS scale 1.0.
pub const MP_FOOTER_H: f32 = 22.0;
/// Letters-only prior CP column width at zoom 1.0 / OS scale 1.0.
pub const PRIOR_CP_W: f32 = 28.0;
/// Divider between session blocks at OS scale 1.0.
pub const SESSION_DIVIDER_W: f32 = 2.0;
/// Axis-dominant drag threshold in px (classify once past this).
pub const AXIS_DOMINANCE_PX: f32 = 3.0;
/// Horizontal zoom clamps.
pub const ZOOM_MIN: f32 = 0.5;
pub const ZOOM_MAX: f32 = 3.0;
/// Multiplicative zoom step per wheel notch.
pub const ZOOM_STEP: f32 = 1.1;

/// Scaled MP row height.
#[inline]
pub fn mp_row_h(scale: f32) -> f32 {
    MP_ROW_H * scale
}

/// Scaled MP footer height.
#[inline]
pub fn mp_footer_h(scale: f32) -> f32 {
    MP_FOOTER_H * scale
}

const FRACTIONS: [f32; 5] = [0.22, 0.38, 0.12, 0.18, 0.10];

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Strip {
    pub x: f32,
    pub w: f32,
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct MpStrips {
    pub cp: Strip,
    pub ep: Strip,
    pub pv: Strip,
    pub sv: Strip,
    pub axis: Strip,
}

pub fn strips(origin_x: f32, width: f32) -> MpStrips {
    assert!(width.is_finite() && width >= 0.0, "MP width must be finite");
    debug_assert!((FRACTIONS.iter().sum::<f32>() - 1.0).abs() < 1e-6);
    let mut x = origin_x;
    let mut out = [Strip::default(); 5];
    for (index, fraction) in FRACTIONS.into_iter().enumerate() {
        let w = width * fraction;
        out[index] = Strip { x, w };
        x += w;
    }
    MpStrips {
        cp: out[0],
        ep: out[1],
        pv: out[2],
        sv: out[3],
        axis: out[4],
    }
}

/// Current-session body strips (CP→EP→PV→SV) inside a content width that excludes the
/// pinned axis. Axis geometry is supplied separately by [`session_layout`].
pub fn current_body_strips(origin_x: f32, body_width: f32) -> MpStrips {
    assert!(
        body_width.is_finite() && body_width >= 0.0,
        "MP body width must be finite"
    );
    let body_frac: f32 = FRACTIONS[..4].iter().sum();
    assert!(body_frac > 0.0, "MP body fractions must be positive");
    let mut x = origin_x;
    let mut out = [Strip::default(); 4];
    for (index, fraction) in FRACTIONS[..4].iter().copied().enumerate() {
        let w = body_width * (fraction / body_frac);
        out[index] = Strip { x, w };
        x += w;
    }
    MpStrips {
        cp: out[0],
        ep: out[1],
        pv: out[2],
        sv: out[3],
        axis: Strip::default(),
    }
}

pub fn max_rows(height: f32, scale: f32) -> usize {
    let footer = mp_footer_h(scale);
    let row = mp_row_h(scale);
    if !height.is_finite() || height <= footer || row <= 0.0 {
        return 0;
    }
    ((height - footer) / row).floor() as usize
}

pub fn row_y(origin_y: f32, from_top: usize, scale: f32) -> f32 {
    origin_y + from_top as f32 * mp_row_h(scale)
}

pub fn volume_width(value: u64, max: u64, available: f32) -> f32 {
    if value == 0 || max == 0 || available <= 0.0 {
        return 0.0;
    }
    ((value as f64 / max as f64) as f32 * available).max(1.0)
}

/// Y coordinate at the center of a semantic price row in a descending window.
pub fn price_line_y(
    price: i64,
    top_price: i64,
    scaled_tick: i64,
    origin_y: f32,
    scale: f32,
) -> Option<f32> {
    if scaled_tick <= 0 {
        return None;
    }
    let delta = top_price.checked_sub(price)?;
    if delta < 0 || delta % scaled_tick != 0 {
        return None;
    }
    Some(row_y(origin_y, (delta / scaled_tick) as usize, scale) + mp_row_h(scale) / 2.0)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SessionBlockKind {
    Prior,
    Current,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SessionBlock {
    pub kind: SessionBlockKind,
    pub session_index: usize,
    pub x: f32,
    pub w: f32,
    /// Content strips. Prior: only `cp` is meaningful. Current: CP→EP→PV→SV.
    pub strips: MpStrips,
}

#[derive(Clone, Debug, PartialEq)]
pub struct SessionLayout {
    pub blocks: Vec<SessionBlock>,
    /// Absolute x of each inter-session divider (left edge).
    pub dividers: Vec<f32>,
    pub axis: Strip,
    pub strip_viewport: Strip,
    pub content_width: f32,
    pub pan_px: f32,
    pub zoom: f32,
}

/// Layout all MP session blocks.
///
/// Priors are collapsed letters-only CP columns (oldest left). Current session keeps the
/// CP→EP→PV→SV body fractions of today's layout, scaled by `zoom`. The price axis stays
/// pinned on the right and is unaffected by horizontal pan. `pan_px` is content-space
/// offset (positive shifts content left / reveals older sessions).
pub fn session_layout(
    origin_x: f32,
    pane_width: f32,
    session_count: usize,
    pan_px: f32,
    zoom: f32,
    ui_scale: f32,
) -> SessionLayout {
    assert!(
        pane_width.is_finite() && pane_width >= 0.0,
        "MP pane width must be finite"
    );
    assert!(session_count >= 1, "MP layout requires ≥1 session");
    assert!(
        ui_scale.is_finite() && ui_scale > 0.0,
        "UI scale must be > 0"
    );
    let zoom = clamp_zoom(zoom);
    let axis_w = pane_width * FRACTIONS[4];
    let strip_w = (pane_width - axis_w).max(0.0);
    let axis = Strip {
        x: origin_x + strip_w,
        w: axis_w,
    };
    let strip_viewport = Strip {
        x: origin_x,
        w: strip_w,
    };

    let prior_w = PRIOR_CP_W * ui_scale * zoom;
    let divider_w = SESSION_DIVIDER_W * ui_scale;
    let current_body_w = strip_w * zoom;
    let prior_count = session_count - 1;
    let content_width = prior_count as f32 * (prior_w + divider_w) + current_body_w;
    let pan_px = clamp_pan(pan_px, content_width, strip_w);

    let mut blocks = Vec::with_capacity(session_count);
    let mut dividers = Vec::with_capacity(prior_count);
    let mut cursor = origin_x - pan_px;
    for index in 0..prior_count {
        blocks.push(SessionBlock {
            kind: SessionBlockKind::Prior,
            session_index: index,
            x: cursor,
            w: prior_w,
            strips: MpStrips {
                cp: Strip {
                    x: cursor,
                    w: prior_w,
                },
                ..MpStrips::default()
            },
        });
        cursor += prior_w;
        dividers.push(cursor);
        cursor += divider_w;
    }
    let current = current_body_strips(cursor, current_body_w);
    blocks.push(SessionBlock {
        kind: SessionBlockKind::Current,
        session_index: prior_count,
        x: cursor,
        w: current_body_w,
        strips: current,
    });

    SessionLayout {
        blocks,
        dividers,
        axis,
        strip_viewport,
        content_width,
        pan_px,
        zoom,
    }
}

pub fn clamp_zoom(zoom: f32) -> f32 {
    assert!(zoom.is_finite() && zoom > 0.0, "MP zoom must be finite > 0");
    zoom.clamp(ZOOM_MIN, ZOOM_MAX)
}

pub fn clamp_pan(pan_px: f32, content_width: f32, viewport_width: f32) -> f32 {
    assert!(pan_px.is_finite(), "MP pan must be finite");
    assert!(
        content_width.is_finite() && content_width >= 0.0,
        "MP content width must be finite"
    );
    assert!(
        viewport_width.is_finite() && viewport_width >= 0.0,
        "MP strip viewport must be finite"
    );
    // Keep the current session's right edge from leaving the strip viewport entirely,
    // and never pan past the leftmost content edge.
    let max_pan = (content_width - viewport_width).max(0.0);
    pan_px.clamp(0.0, max_pan)
}

/// Apply a multiplicative zoom step anchored at cursor-x inside the strip viewport.
/// Returns `(new_zoom, new_pan_px)` such that the content point under `cursor_x`
/// stays fixed.
#[allow(clippy::too_many_arguments)]
pub fn zoom_at_cursor(
    origin_x: f32,
    pane_width: f32,
    session_count: usize,
    pan_px: f32,
    zoom: f32,
    ui_scale: f32,
    cursor_x: f32,
    wheel_notches: f32,
) -> (f32, f32) {
    assert!(cursor_x.is_finite(), "MP zoom cursor must be finite");
    assert!(wheel_notches.is_finite(), "MP zoom notches must be finite");
    let before = session_layout(origin_x, pane_width, session_count, pan_px, zoom, ui_scale);
    let factor = if wheel_notches >= 0.0 {
        ZOOM_STEP.powf(wheel_notches)
    } else {
        (1.0 / ZOOM_STEP).powf(-wheel_notches)
    };
    let new_zoom = clamp_zoom(before.zoom * factor);
    if (new_zoom - before.zoom).abs() <= f32::EPSILON {
        return (before.zoom, before.pan_px);
    }
    let local = (cursor_x - before.strip_viewport.x).clamp(0.0, before.strip_viewport.w);
    let content_x = before.pan_px + local;
    let scaled_content_x = content_x * (new_zoom / before.zoom);
    let new_pan = clamp_pan(
        scaled_content_x - local,
        before.content_width * (new_zoom / before.zoom),
        before.strip_viewport.w,
    );
    // Recompute against the true content width at the new zoom (axis fraction fixed).
    let after = session_layout(
        origin_x,
        pane_width,
        session_count,
        new_pan,
        new_zoom,
        ui_scale,
    );
    (after.zoom, after.pan_px)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DragAxis {
    Undecided,
    Horizontal,
    Vertical,
}

/// Classify an axis-dominant drag once cumulative |dx| or |dy| crosses the threshold.
pub fn classify_drag_axis(dx: f32, dy: f32, threshold: f32) -> DragAxis {
    assert!(
        dx.is_finite() && dy.is_finite(),
        "drag deltas must be finite"
    );
    assert!(
        threshold.is_finite() && threshold > 0.0,
        "axis threshold must be > 0"
    );
    let ax = dx.abs();
    let ay = dy.abs();
    if ax.max(ay) < threshold {
        DragAxis::Undecided
    } else if ax > ay {
        DragAxis::Horizontal
    } else {
        DragAxis::Vertical
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strips_cover_width_and_pin_axis_right() {
        let cols = strips(10.0, 500.0);
        assert!((cols.cp.x - 10.0).abs() < 1e-6);
        assert!((cols.axis.x + cols.axis.w - 510.0).abs() < 1e-4);
    }

    #[test]
    fn pv_sv_scaling_is_linear_and_quiet_at_zero() {
        assert_eq!(volume_width(0, 10, 80.0), 0.0);
        assert_eq!(volume_width(5, 10, 80.0), 40.0);
        assert_eq!(volume_width(10, 10, 80.0), 80.0);
    }

    #[test]
    fn sv_bar_width_is_driven_by_session_volume_only() {
        // Mirror paint_rows: available = cols.sv.w - 4.0; width = volume_width(session_volume, …).
        // Aggressor buy/sell volumes must not change the SV geometry (René 2026-08-11).
        let cols = strips(0.0, 500.0);
        let available = cols.sv.w - 4.0;
        let max_sv = 100;
        let session_volume = 50u64;
        let buy_volume = 90u64;
        let sell_volume = 90u64;
        let sv_w = volume_width(session_volume, max_sv, available);
        assert!((sv_w - available * 0.5).abs() < 1e-4);
        let legacy_half = (cols.sv.w - 4.0) / 2.0;
        let legacy_sell = volume_width(sell_volume, max_sv, legacy_half);
        let legacy_buy = volume_width(buy_volume, max_sv, legacy_half);
        assert!(
            (sv_w - legacy_sell).abs() > 1.0 && (sv_w - legacy_buy).abs() > 1.0,
            "session_volume width must differ from the removed centered aggressor half-bars"
        );
    }

    #[test]
    fn va_line_placement_uses_descending_price_rows() {
        assert_eq!(price_line_y(100, 104, 2, 10.0, 1.0), Some(50.0));
        assert_eq!(price_line_y(101, 104, 2, 10.0, 1.0), None);
        assert_eq!(price_line_y(106, 104, 2, 10.0, 1.0), None);
    }

    #[test]
    fn scale_multiplies_row_y_and_max_rows() {
        assert!((mp_row_h(1.5) - MP_ROW_H * 1.5).abs() < 1e-6);
        assert!((mp_footer_h(1.5) - MP_FOOTER_H * 1.5).abs() < 1e-6);
        // height 182, footer 22 → body 160 / 16 = 10 at scale 1.0
        assert_eq!(max_rows(182.0, 1.0), 10);
        // footer 33, body 149 / 24 = 6.208 → 6
        assert_eq!(max_rows(182.0, 1.5), 6);
        assert!((row_y(10.0, 2, 1.0) - 42.0).abs() < 1e-4);
        assert!((row_y(10.0, 2, 1.5) - (10.0 + 2.0 * 24.0)).abs() < 1e-4);
        // price_line_y at scale 1.5: row 2 → 10 + 2*24 + 12 = 70
        assert_eq!(price_line_y(100, 104, 2, 10.0, 1.5), Some(70.0));
    }

    #[test]
    fn single_session_matches_legacy_body_and_pinned_axis() {
        let layout = session_layout(0.0, 500.0, 1, 0.0, 1.0, 1.0);
        let legacy = strips(0.0, 500.0);
        assert_eq!(layout.blocks.len(), 1);
        assert!(layout.dividers.is_empty());
        assert!((layout.axis.x - legacy.axis.x).abs() < 1e-4);
        assert!((layout.axis.w - legacy.axis.w).abs() < 1e-4);
        let cur = &layout.blocks[0];
        assert_eq!(cur.kind, SessionBlockKind::Current);
        assert!((cur.strips.cp.x - legacy.cp.x).abs() < 1e-3);
        assert!((cur.strips.cp.w - legacy.cp.w).abs() < 1e-3);
        assert!((cur.strips.ep.w - legacy.ep.w).abs() < 1e-3);
        assert!((cur.strips.pv.w - legacy.pv.w).abs() < 1e-3);
        assert!((cur.strips.sv.w - legacy.sv.w).abs() < 1e-3);
        assert!((layout.pan_px).abs() < 1e-6);
    }

    #[test]
    fn three_sessions_place_priors_left_with_dividers() {
        let layout = session_layout(10.0, 500.0, 3, 0.0, 1.0, 1.0);
        assert_eq!(layout.blocks.len(), 3);
        assert_eq!(layout.dividers.len(), 2);
        assert_eq!(layout.blocks[0].kind, SessionBlockKind::Prior);
        assert_eq!(layout.blocks[1].kind, SessionBlockKind::Prior);
        assert_eq!(layout.blocks[2].kind, SessionBlockKind::Current);
        assert!((layout.blocks[0].w - PRIOR_CP_W).abs() < 1e-4);
        assert!((layout.blocks[0].x - 10.0).abs() < 1e-4);
        assert!((layout.dividers[0] - (10.0 + PRIOR_CP_W)).abs() < 1e-4);
        // Second divider sits after prior1, before the current block — only one
        // SESSION_DIVIDER_W between the two priors, not two.
        assert!((layout.dividers[1] - (10.0 + 2.0 * PRIOR_CP_W + SESSION_DIVIDER_W)).abs() < 1e-4);
        let expected_content = 2.0 * (PRIOR_CP_W + SESSION_DIVIDER_W) + layout.strip_viewport.w;
        assert!((layout.content_width - expected_content).abs() < 1e-3);
        // Current starts after two prior blocks + dividers.
        let expected_current_x = 10.0 + 2.0 * (PRIOR_CP_W + SESSION_DIVIDER_W);
        assert!((layout.blocks[2].x - expected_current_x).abs() < 1e-3);
        // Axis pinned right, independent of priors.
        assert!((layout.axis.x + layout.axis.w - 510.0).abs() < 1e-3);
    }

    #[test]
    fn five_sessions_widths_and_offsets() {
        let layout = session_layout(0.0, 1_000.0, 5, 0.0, 1.0, 1.0);
        assert_eq!(layout.blocks.len(), 5);
        assert_eq!(layout.dividers.len(), 4);
        for block in &layout.blocks[..4] {
            assert_eq!(block.kind, SessionBlockKind::Prior);
            assert!((block.w - PRIOR_CP_W).abs() < 1e-4);
        }
        assert_eq!(layout.blocks[4].kind, SessionBlockKind::Current);
        assert!((layout.blocks[4].w - layout.strip_viewport.w).abs() < 1e-3);
        let expected = 4.0 * (PRIOR_CP_W + SESSION_DIVIDER_W) + layout.strip_viewport.w;
        assert!((layout.content_width - expected).abs() < 1e-3);
    }

    #[test]
    fn pan_clamps_both_ends() {
        let layout = session_layout(0.0, 500.0, 3, 0.0, 1.0, 1.0);
        let max_pan = layout.content_width - layout.strip_viewport.w;
        assert!(max_pan > 0.0);
        let left = session_layout(0.0, 500.0, 3, -50.0, 1.0, 1.0);
        assert!((left.pan_px - 0.0).abs() < 1e-4);
        let right = session_layout(0.0, 500.0, 3, max_pan + 80.0, 1.0, 1.0);
        assert!((right.pan_px - max_pan).abs() < 1e-3);
        // At max pan, current session's right edge meets the strip viewport's right edge.
        let current = right.blocks.last().unwrap();
        assert!(
            ((current.x + current.w) - (right.strip_viewport.x + right.strip_viewport.w)).abs()
                < 1e-2
        );
    }

    #[test]
    fn zoom_anchor_keeps_cursor_content_invariant() {
        let origin = 0.0;
        let width = 500.0;
        let sessions = 3usize;
        let pan = 20.0;
        let zoom = 1.0;
        let cursor = 120.0;
        let before = session_layout(origin, width, sessions, pan, zoom, 1.0);
        let local = cursor - before.strip_viewport.x;
        let content_before = before.pan_px + local;
        let (new_zoom, new_pan) =
            zoom_at_cursor(origin, width, sessions, pan, zoom, 1.0, cursor, 1.0);
        assert!((new_zoom - ZOOM_STEP).abs() < 1e-4);
        let after = session_layout(origin, width, sessions, new_pan, new_zoom, 1.0);
        let content_after = after.pan_px + local;
        let expected = content_before * (new_zoom / zoom);
        assert!(
            (content_after - expected).abs() < 0.05,
            "content under cursor drifted: before_scaled={expected} after={content_after}"
        );
    }

    #[test]
    fn axis_dominance_classifier() {
        assert_eq!(
            classify_drag_axis(1.0, 1.0, AXIS_DOMINANCE_PX),
            DragAxis::Undecided
        );
        assert_eq!(
            classify_drag_axis(4.0, 1.0, AXIS_DOMINANCE_PX),
            DragAxis::Horizontal
        );
        assert_eq!(
            classify_drag_axis(1.0, 4.0, AXIS_DOMINANCE_PX),
            DragAxis::Vertical
        );
        // Equal past threshold prefers vertical (price pan is the legacy path).
        assert_eq!(
            classify_drag_axis(5.0, 5.0, AXIS_DOMINANCE_PX),
            DragAxis::Vertical
        );
    }
}
