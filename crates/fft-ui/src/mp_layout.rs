//! Pure Market Profile strip and multi-session geometry.

/// Row height at OS scale 1.0.
pub const MP_ROW_H: f32 = 16.0;
/// Footer height at OS scale 1.0.
pub const MP_FOOTER_H: f32 = 22.0;
/// Letters-only prior CP column width at zoom 1.0 / OS scale 1.0.
pub const PRIOR_CP_W: f32 = 28.0;
/// Divider between session blocks at OS scale 1.0.
pub const SESSION_DIVIDER_W: f32 = 2.0;
/// Readable pinned price-axis width at OS scale 1.0.
pub const PRICE_AXIS_W: f32 = 80.0;
/// Axis-dominant drag threshold in px (classify once past this).
pub const AXIS_DOMINANCE_PX: f32 = 3.0;
/// Horizontal zoom clamps.
pub const ZOOM_MIN: f32 = 0.5;
pub const ZOOM_MAX: f32 = 3.0;
/// Multiplicative zoom step per wheel notch.
pub const ZOOM_STEP: f32 = 1.1;

/// Map a wheel delta's Y component to discrete zoom notches.
/// Positive = zoom in. Zero / non-finite → 0.
#[inline]
pub fn scroll_notches(delta_y: f32) -> f32 {
    if !delta_y.is_finite() {
        return 0.0;
    }
    if delta_y > 0.0 {
        1.0
    } else if delta_y < 0.0 {
        -1.0
    } else {
        0.0
    }
}

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
/// pinned on the right and is unaffected by horizontal pan. Positive `pan_px` shifts content
/// left, revealing the current/right side; dragging right decreases pan and reveals older/left.
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
    let axis_w = (PRICE_AXIS_W * ui_scale).min(pane_width);
    let strip_w = pane_width - axis_w;
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

/// Furthest valid horizontal pan. At this position the current session's right edge is
/// aligned with the strip viewport's right edge.
pub fn current_session_max_pan(content_width: f32, viewport_width: f32) -> f32 {
    assert!(
        content_width.is_finite() && content_width >= 0.0,
        "MP content width must be finite"
    );
    assert!(
        viewport_width.is_finite() && viewport_width >= 0.0,
        "MP strip viewport must be finite"
    );
    (content_width - viewport_width).max(0.0)
}

/// Default/rest pan: current session right-aligned in the strip viewport.
pub fn current_session_rest_pan(content_width: f32, viewport_width: f32) -> f32 {
    current_session_max_pan(content_width, viewport_width)
}

pub fn clamp_pan(pan_px: f32, content_width: f32, viewport_width: f32) -> f32 {
    assert!(pan_px.is_finite(), "MP pan must be finite");
    pan_px.clamp(0.0, current_session_max_pan(content_width, viewport_width))
}

#[derive(Clone, Copy)]
enum ZoomAnchor {
    Prior { index: usize, local: f32 },
    Divider { index: usize, local: f32 },
    Current { local: f32 },
}

fn zoom_anchor(layout: &SessionLayout, cursor_x: f32, ui_scale: f32) -> (ZoomAnchor, f32) {
    let viewport_local = (cursor_x - layout.strip_viewport.x).clamp(0.0, layout.strip_viewport.w);
    let content_x = (layout.pan_px + viewport_local).clamp(0.0, layout.content_width);
    let prior_count = layout.blocks.len() - 1;
    let prior_w = PRIOR_CP_W * ui_scale * layout.zoom;
    let divider_w = SESSION_DIVIDER_W * ui_scale;
    let stride = prior_w + divider_w;

    for index in 0..prior_count {
        let start = index as f32 * stride;
        if content_x <= start + prior_w {
            return (
                ZoomAnchor::Prior {
                    index,
                    local: ((content_x - start) / prior_w).clamp(0.0, 1.0),
                },
                viewport_local,
            );
        }
        if content_x <= start + stride {
            return (
                ZoomAnchor::Divider {
                    index,
                    local: ((content_x - start - prior_w) / divider_w).clamp(0.0, 1.0),
                },
                viewport_local,
            );
        }
    }

    let current_start = prior_count as f32 * stride;
    let current_w = layout.blocks.last().expect("current session block").w;
    let local = if current_w > 0.0 {
        ((content_x - current_start) / current_w).clamp(0.0, 1.0)
    } else {
        0.0
    };
    (ZoomAnchor::Current { local }, viewport_local)
}

fn anchored_content_x(anchor: ZoomAnchor, layout: &SessionLayout, ui_scale: f32) -> f32 {
    let prior_w = PRIOR_CP_W * ui_scale * layout.zoom;
    let divider_w = SESSION_DIVIDER_W * ui_scale;
    let stride = prior_w + divider_w;
    match anchor {
        ZoomAnchor::Prior { index, local } => index as f32 * stride + local * prior_w,
        ZoomAnchor::Divider { index, local } => index as f32 * stride + prior_w + local * divider_w,
        ZoomAnchor::Current { local } => {
            (layout.blocks.len() - 1) as f32 * stride
                + local * layout.blocks.last().expect("current session block").w
        }
    }
}

/// Apply a multiplicative zoom step anchored at cursor-x inside the strip viewport.
/// Fixed-width dividers retain their width; the block-local point under `cursor_x` stays fixed
/// unless the resulting pan reaches a navigation bound.
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
    let (anchor, viewport_local) = zoom_anchor(&before, cursor_x, ui_scale);
    let after = session_layout(origin_x, pane_width, session_count, 0.0, new_zoom, ui_scale);
    let new_pan = clamp_pan(
        anchored_content_x(anchor, &after, ui_scale) - viewport_local,
        after.content_width,
        after.strip_viewport.w,
    );
    (after.zoom, new_pan)
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
#[path = "mp_layout_tests.rs"]
mod tests;
