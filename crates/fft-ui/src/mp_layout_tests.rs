use super::*;

fn close(actual: f32, expected: f32) {
    assert!(
        (actual - expected).abs() < 0.05,
        "expected {expected}, got {actual}"
    );
}

#[test]
fn legacy_strips_still_cover_width_and_pin_axis_right() {
    let cols = strips(10.0, 500.0);
    close(cols.cp.x, 10.0);
    close(cols.axis.x + cols.axis.w, 510.0);
}

#[test]
fn product_axis_is_scale_aware_readable_and_bounded() {
    for (pane_width, scale, axis_w, body_w) in [
        (180.0, 1.0, 80.0, 100.0),
        (500.0, 1.0, 80.0, 420.0),
        (1_600.0, 1.0, 80.0, 1_520.0),
        (500.0, 1.5, 120.0, 380.0),
        (180.0, 1.5, 120.0, 60.0),
    ] {
        let layout = session_layout(10.0, pane_width, 1, 0.0, 1.0, scale);
        close(layout.axis.w, axis_w);
        close(layout.strip_viewport.w, body_w);
        close(layout.axis.x + layout.axis.w, 10.0 + pane_width);
        assert!(layout.strip_viewport.w >= 0.0);
    }

    let narrower_than_axis = session_layout(0.0, 60.0, 1, 0.0, 1.0, 1.0);
    close(narrower_than_axis.axis.w, 60.0);
    close(narrower_than_axis.strip_viewport.w, 0.0);
    let (zoom, pan) = zoom_at_cursor(0.0, 60.0, 1, 0.0, 1.0, 1.0, 20.0, 1.0);
    close(zoom, ZOOM_STEP);
    close(pan, 0.0);
}

#[test]
fn pv_sv_scaling_is_linear_and_quiet_at_zero() {
    assert_eq!(volume_width(0, 10, 80.0), 0.0);
    assert_eq!(volume_width(5, 10, 80.0), 40.0);
    assert_eq!(volume_width(10, 10, 80.0), 80.0);
}

#[test]
fn sv_bar_width_is_driven_by_session_volume_only() {
    let cols = strips(0.0, 500.0);
    let available = cols.sv.w - 4.0;
    let sv_w = volume_width(50, 100, available);
    close(sv_w, available * 0.5);
    let legacy_half = (cols.sv.w - 4.0) / 2.0;
    assert!((sv_w - volume_width(90, 100, legacy_half)).abs() > 1.0);
}

#[test]
fn vertical_geometry_is_scaled_and_bounded() {
    close(mp_row_h(1.5), MP_ROW_H * 1.5);
    close(mp_footer_h(1.5), MP_FOOTER_H * 1.5);
    assert_eq!(max_rows(182.0, 1.0), 10);
    assert_eq!(max_rows(182.0, 1.5), 6);
    assert_eq!(max_rows(MP_FOOTER_H, 1.0), 0);
    close(row_y(10.0, 2, 1.5), 58.0);
    assert_eq!(price_line_y(100, 104, 2, 10.0, 1.5), Some(70.0));
    assert_eq!(price_line_y(101, 104, 2, 10.0, 1.0), None);
    assert_eq!(price_line_y(106, 104, 2, 10.0, 1.0), None);
    assert_eq!(price_line_y(100, 104, 0, 10.0, 1.0), None);
}

#[test]
fn one_and_five_session_layouts_use_current_session_rest_pan() {
    for sessions in [1, 5] {
        let unpanned = session_layout(10.0, 500.0, sessions, 0.0, 1.0, 1.0);
        let max = current_session_max_pan(unpanned.content_width, unpanned.strip_viewport.w);
        let rest = current_session_rest_pan(unpanned.content_width, unpanned.strip_viewport.w);
        close(rest, max);
        if sessions == 1 {
            close(rest, 0.0);
        } else {
            close(rest, 4.0 * (PRIOR_CP_W + SESSION_DIVIDER_W));
        }

        let layout = session_layout(10.0, 500.0, sessions, rest, 1.0, 1.0);
        let current = layout.blocks.last().unwrap();
        close(
            current.x + current.w,
            layout.strip_viewport.x + layout.strip_viewport.w,
        );
    }
}

#[test]
fn five_sessions_place_fixed_dividers_and_scaled_blocks() {
    let layout = session_layout(10.0, 1_000.0, 5, 0.0, 1.5, 1.0);
    assert_eq!(layout.blocks.len(), 5);
    assert_eq!(layout.dividers.len(), 4);
    for block in &layout.blocks[..4] {
        assert_eq!(block.kind, SessionBlockKind::Prior);
        close(block.w, PRIOR_CP_W * 1.5);
    }
    for pair in layout.dividers.windows(2) {
        close(pair[1] - pair[0], PRIOR_CP_W * 1.5 + SESSION_DIVIDER_W);
    }
    assert_eq!(layout.blocks[4].kind, SessionBlockKind::Current);
    close(layout.blocks[4].w, layout.strip_viewport.w * 1.5);
    close(layout.axis.x + layout.axis.w, 1_010.0);
}

#[test]
fn zoom_clamp_and_rest_pan_helpers() {
    assert_eq!(clamp_zoom(0.1), ZOOM_MIN);
    assert_eq!(clamp_zoom(1.25), 1.25);
    assert_eq!(clamp_zoom(9.0), ZOOM_MAX);
    assert_eq!(current_session_max_pan(300.0, 400.0), 0.0);
    // Soft clamp helper retained; live navigation is free-canvas (unclamped).
    assert_eq!(clamp_pan(-25.0, 540.0, 420.0), 0.0);
    assert_eq!(clamp_pan(40.0, 540.0, 420.0), 40.0);
    assert_eq!(clamp_pan(999.0, 540.0, 420.0), 120.0);
}

#[test]
fn session_layout_accepts_canvas_pan_beyond_content() {
    let layout = session_layout(0.0, 420.0, 3, -80.0, 1.0, 1.0);
    assert_eq!(layout.pan_px, -80.0);
    // Content origin is to the right of the strip when pan is negative.
    assert!(layout.blocks[0].x > layout.strip_viewport.x);

    let past = session_layout(0.0, 420.0, 3, layout.content_width + 50.0, 1.0, 1.0);
    assert_eq!(past.pan_px, layout.content_width + 50.0);
    // Entire strip content is left of the viewport.
    let last = past.blocks.last().expect("sessions");
    assert!(last.x + last.w < past.strip_viewport.x);
}

fn assert_cursor_anchor(cursor: f32, pan: f32, notch: f32) {
    let before = session_layout(0.0, 500.0, 5, pan, 1.0, 1.0);
    let (anchor, viewport_local) = zoom_anchor(&before, cursor, 1.0);
    let (zoom, new_pan) = zoom_at_cursor(0.0, 500.0, 5, pan, 1.0, 1.0, cursor, notch);
    let after = session_layout(0.0, 500.0, 5, new_pan, zoom, 1.0);
    let actual = anchored_content_x(anchor, &after, 1.0) - after.pan_px;
    assert!(
        (actual - viewport_local).abs() < 0.05,
        "cursor={cursor} pan={pan} notch={notch}: expected {viewport_local}, got {actual}, new_pan={new_pan}"
    );
}

#[test]
fn zoom_anchor_tracks_regions_across_fixed_dividers() {
    let layout = session_layout(0.0, 500.0, 5, 10.0, 1.0, 1.0);
    let oldest_cp = layout.blocks[0].x + layout.blocks[0].w * 0.75;
    let middle_cp = layout.blocks[2].x + layout.blocks[2].w * 0.5;

    let current_pan = 60.0;
    let current = session_layout(0.0, 500.0, 5, current_pan, 1.0, 1.0);
    let current = current.blocks.last().unwrap();
    let current_cp = current.strips.cp.x + current.strips.cp.w * 0.5;
    let current_sv = current.strips.sv.x + current.strips.sv.w * 0.5;

    for (cursor, pan) in [
        (oldest_cp, 10.0),
        (middle_cp, 10.0),
        (current_cp, current_pan),
        (current_sv, current_pan),
    ] {
        assert_cursor_anchor(cursor, pan, 1.0);
        assert_cursor_anchor(cursor, pan, -1.0);
    }
}

#[test]
fn wheel_quantization_and_zoom_bounds_are_unchanged() {
    assert_eq!(scroll_notches(1.0), 1.0);
    assert_eq!(scroll_notches(-3.5), -1.0);
    assert_eq!(scroll_notches(0.0), 0.0);
    assert_eq!(scroll_notches(f32::NAN), 0.0);
    let (at_max, _) = zoom_at_cursor(0.0, 500.0, 2, 10.0, ZOOM_MAX, 1.0, 80.0, 1.0);
    close(at_max, ZOOM_MAX);
    let (at_min, _) = zoom_at_cursor(0.0, 500.0, 2, 10.0, ZOOM_MIN, 1.0, 80.0, -1.0);
    close(at_min, ZOOM_MIN);
}

#[test]
fn axis_dominance_classifier_covers_threshold_and_ties() {
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
    assert_eq!(
        classify_drag_axis(5.0, 5.0, AXIS_DOMINANCE_PX),
        DragAxis::Vertical
    );
}
