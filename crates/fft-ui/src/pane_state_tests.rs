use fft_engine::{DomPriceRow, ProfilePriceRow, ProfileSessionRender};

use super::*;

fn profile(prices: &[i64]) -> ProfileRenderState {
    ProfileRenderState {
        sessions: vec![ProfileSessionRender {
            rows: prices
                .iter()
                .map(|price| ProfilePriceRow {
                    price: Price(*price),
                    ..Default::default()
                })
                .collect(),
            ..Default::default()
        }],
    }
}

fn dom(prices: &[i64]) -> DomRenderState {
    DomRenderState {
        rows: prices
            .iter()
            .map(|price| DomPriceRow {
                price: Price(*price),
                ..Default::default()
            })
            .collect(),
        ..Default::default()
    }
}

#[test]
fn raw_follow_center_is_scale_independent() {
    let dom = DomRenderState {
        best_bid: Some(Price(100)),
        best_ask: Some(Price(110)),
        ..Default::default()
    };
    assert_eq!(raw_follow_center(&dom), Some(Price(105)));
}

#[test]
fn scale_key_changes_only_hovered_pane() {
    let mut state = PaneState::default();
    assert!(!state.set_hovered_scale(4));
    state.set_hovered(Pane::MarketProfile, true);
    assert!(state.set_hovered_scale(4));
    assert_eq!((state.mp_scale, state.dom_scale), (4, 1));
    state.set_hovered(Pane::Dom, true);
    assert!(state.set_hovered_scale(2));
    assert_eq!((state.mp_scale, state.dom_scale), (4, 2));
}

#[test]
fn t_syncs_other_pane_to_hovered_scale() {
    let mut state = PaneState::default();
    assert!(!state.sync_scale_from_hovered(), "no hover is a no-op");
    state.set_hovered(Pane::MarketProfile, true);
    assert!(state.set_hovered_scale(4));
    assert!(state.sync_scale_from_hovered());
    assert_eq!((state.mp_scale, state.dom_scale), (4, 4));
    assert!(!state.sync_scale_from_hovered(), "already equal is a no-op");
    state.set_hovered(Pane::Dom, true);
    assert!(state.set_hovered_scale(2));
    assert!(state.sync_scale_from_hovered());
    assert_eq!((state.mp_scale, state.dom_scale), (2, 2));
}

#[test]
fn dom_defaults_hidden_and_toggle_preserves_navigation() {
    let mut state = PaneState {
        center: Some(Price(105)),
        mp_scale: 4,
        mp_pan_px: 73.0,
        mp_zoom: 1.8,
        ..Default::default()
    };
    state.splitter.set_ratio(0.37);
    let before = (
        state.center,
        state.mp_scale,
        state.mp_pan_px,
        state.mp_zoom,
        state.splitter.ratio(),
    );

    assert!(!state.dom_visible());
    assert!(state.toggle_dom());
    assert!(!state.toggle_dom());
    assert_eq!(
        before,
        (
            state.center,
            state.mp_scale,
            state.mp_pan_px,
            state.mp_zoom,
            state.splitter.ratio(),
        )
    );
}

#[test]
fn effective_mp_width_tracks_surface_composition() {
    let mut state = PaneState::default();
    state.splitter.set_ratio(0.4);
    assert_eq!(state.effective_mp_width(1_000.0), 1_000.0);
    state.toggle_dom();
    assert!((state.effective_mp_width(1_000.0) - 397.6).abs() < 1e-4);
}

#[test]
fn hiding_dom_clears_dom_hover_and_cancels_splitter_drag() {
    let mut state = PaneState::default();
    state.toggle_dom();
    state.set_hovered(Pane::Dom, true);
    state.splitter.begin(400.0);
    assert!(state.splitter.is_dragging());

    state.toggle_dom();
    assert_eq!(state.hovered, None);
    assert!(!state.splitter.is_dragging());
    assert!(!state.splitter.consume(1_000.0));
}

#[test]
fn splitter_is_latest_wins_and_clamped() {
    let mut split = SplitterState::default();
    split.begin(250.0);
    split.queue(300.0);
    split.queue(400.0);
    assert!(split.consume(1_000.0));
    assert!((split.ratio() - 400.0 / 994.0).abs() < 1e-6);
    split.queue(-50.0);
    assert!(split.consume(1_000.0));
    assert!((split.ratio() - 180.0 / 994.0).abs() < 1e-6);
}

#[test]
fn splitter_keeps_final_pending_position_after_mouse_up() {
    let mut split = SplitterState::default();
    split.begin(300.0);
    split.queue(420.0);
    split.end();
    assert!(split.consume(1_000.0));
    assert!((split.ratio() - 420.0 / 994.0).abs() < 1e-6);
}

#[test]
fn navigation_range_uses_profile_extrema_beyond_dom_both_ends() {
    assert_eq!(
        navigation_range(&profile(&[80, 140]), &dom(&[100, 120])),
        Some((Price(80), Price(140)))
    );
}

#[test]
fn navigation_range_uses_dom_extrema_beyond_profile_both_ends() {
    assert_eq!(
        navigation_range(&profile(&[100, 120]), &dom(&[80, 140])),
        Some((Price(80), Price(140)))
    );
}

#[test]
fn navigation_range_accepts_only_one_source() {
    assert_eq!(
        navigation_range(&profile(&[80, 140]), &DomRenderState::default()),
        Some((Price(80), Price(140)))
    );
    assert_eq!(
        navigation_range(&ProfileRenderState::default(), &dom(&[100, 120])),
        Some((Price(100), Price(120)))
    );
    assert_eq!(
        navigation_range(&ProfileRenderState::default(), &DomRenderState::default()),
        None
    );
}

#[test]
fn pure_center_clamp_honors_optional_range() {
    assert_eq!(
        clamp_center(Some(Price(500)), Some((Price(80), Price(140)))),
        Some(Price(140))
    );
    assert_eq!(clamp_center(Some(Price(70)), None), Some(Price(70)));
    assert_eq!(clamp_center(None, Some((Price(80), Price(140)))), None);
}

#[test]
fn pan_center_sign_moves_with_pointer_rows() {
    assert_eq!(pan_center(Price(110), Price(1), 1, 3), Price(113));
    assert_eq!(pan_center(Price(110), Price(1), 1, -3), Price(107));
}

#[test]
fn pan_center_applies_each_tick_scale() {
    for (scale, expected) in [(1, 112), (2, 114), (4, 118)] {
        assert_eq!(pan_center(Price(110), Price(1), scale, 2), Price(expected));
    }
}

#[test]
fn recenter_clears_center_and_preserves_mp_navigation() {
    let mut state = PaneState {
        center: Some(Price(105)),
        mp_scale: 4,
        mp_zoom: 1.8,
        ..Default::default()
    };
    state.reconcile_mp_pan(700.0, 400.0);
    state.navigate_mp_pan(-227.0, 700.0, 400.0);
    assert_eq!(state.mp_pan_px, 73.0);
    assert!(!state.mp_at_rest());
    assert!(state.recenter());
    assert_eq!(state.center, None);
    assert_eq!(
        (state.mp_scale, state.mp_pan_px, state.mp_zoom),
        (4, 73.0, 1.8)
    );
    assert!(!state.mp_at_rest(), "c is price-only horizontal state");
}

#[test]
fn first_layout_and_geometry_changes_follow_current_session_rest() {
    let mut state = PaneState::default();
    assert!(state.mp_at_rest());

    assert!(state.reconcile_mp_pan(540.0, 420.0));
    assert_eq!(state.mp_pan_px, 120.0);
    assert!(state.mp_at_rest());

    // Resize/DOM composition, session growth, zoom, and OS scale all arrive as new geometry.
    assert!(state.reconcile_mp_pan(620.0, 300.0));
    assert_eq!(state.mp_pan_px, 320.0);
    assert!(state.reconcile_mp_pan(790.0, 360.0));
    assert_eq!(state.mp_pan_px, 430.0);
    assert!(state.mp_at_rest());
}

#[test]
fn horizontal_drag_leaves_rest_and_future_geometry_only_clamps() {
    let mut state = PaneState::default();
    state.reconcile_mp_pan(700.0, 400.0);
    assert!(state.navigate_mp_pan(-160.0, 700.0, 400.0));
    assert!(!state.mp_at_rest());

    assert!(!state.reconcile_mp_pan(900.0, 450.0));
    assert_eq!(state.mp_pan_px, 140.0, "growth must not snap to new rest");
    assert!(state.reconcile_mp_pan(500.0, 450.0));
    assert_eq!(
        state.mp_pan_px, 50.0,
        "chosen pan clamps to a smaller range"
    );
    assert!(!state.mp_at_rest());
}

#[test]
fn drag_at_rest_bound_stays_logically_at_rest() {
    let mut state = PaneState::default();
    state.reconcile_mp_pan(700.0, 400.0);
    assert_eq!(state.mp_pan_px, 300.0);
    assert!(!state.navigate_mp_pan(0.0, 700.0, 400.0));
    assert!(state.mp_at_rest());
    assert!(state.reconcile_mp_pan(900.0, 400.0));
    assert_eq!(state.mp_pan_px, 500.0);
}

#[test]
fn user_pan_survives_temporary_narrow_geometry_and_restores() {
    let mut state = PaneState::default();
    state.reconcile_mp_pan(900.0, 400.0);
    state.navigate_mp_pan(-180.0, 900.0, 400.0);
    assert_eq!(state.mp_pan_px, 320.0);
    assert!(!state.mp_at_rest());

    assert!(state.reconcile_mp_pan(600.0, 400.0));
    assert_eq!(state.mp_pan_px, 200.0, "display clamps in narrow geometry");
    assert!(state.reconcile_mp_pan(900.0, 400.0));
    assert_eq!(
        state.mp_pan_px, 320.0,
        "logical pan restores when width returns"
    );
    assert!(!state.reconcile_mp_pan(900.0, 400.0));
    assert_eq!(
        state.mp_pan_px, 320.0,
        "consecutive renders do not oscillate"
    );
}

#[test]
fn horizontal_drag_away_and_back_rearms_rest() {
    let mut state = PaneState::default();
    state.reconcile_mp_pan(700.0, 400.0);
    state.navigate_mp_pan(-100.0, 700.0, 400.0);
    assert_eq!(state.mp_pan_px, 200.0);
    assert!(!state.mp_at_rest());
    state.navigate_mp_pan(100.0, 700.0, 400.0);
    assert_eq!(state.mp_pan_px, 300.0);
    assert!(state.mp_at_rest());

    assert!(state.reconcile_mp_pan(900.0, 400.0));
    assert_eq!(state.mp_pan_px, 500.0);
}

#[test]
fn persisted_zoom_launch_and_rest_transitions_use_actual_geometry() {
    let mut state = PaneState {
        mp_zoom: 1.5,
        ..Default::default()
    };
    // One current session at zoom 1.5: (420 - 80) * 0.5 = 170.
    let one = crate::mp_layout::session_layout(0.0, 420.0, 1, 0.0, state.mp_zoom, 1.0);
    assert!(state.reconcile_mp_pan(one.content_width, one.strip_viewport.w));
    assert_eq!(state.mp_pan_px, 170.0);
    assert!(!state.reconcile_mp_pan(one.content_width, one.strip_viewport.w));

    // Priors arrive progressively in the same geometry.
    for sessions in 2..=5 {
        let layout = crate::mp_layout::session_layout(
            0.0,
            420.0,
            sessions,
            state.mp_pan_px,
            state.mp_zoom,
            1.0,
        );
        assert!(state.reconcile_mp_pan(layout.content_width, layout.strip_viewport.w));
        assert_eq!(
            state.mp_pan_px,
            current_session_rest_pan(layout.content_width, layout.strip_viewport.w)
        );
        assert!(!state.reconcile_mp_pan(layout.content_width, layout.strip_viewport.w));
    }
}

#[test]
fn rest_tracks_dom_toggle_resize_splitter_and_os_scale_without_oscillation() {
    let mut state = PaneState {
        mp_zoom: 1.5,
        ..Default::default()
    };
    state.splitter.set_ratio(0.4);
    for (viewport, dom_visible, split_ratio, scale) in [
        (1_000.0, false, 0.4, 1.0),
        (1_000.0, true, 0.4, 1.0),
        (1_000.0, true, 0.6, 1.0),
        (1_200.0, true, 0.6, 1.0),
        (1_200.0, true, 0.6, 1.5),
        (1_200.0, false, 0.6, 1.5),
    ] {
        state.splitter.set_ratio(split_ratio);
        if state.dom_visible() != dom_visible {
            state.toggle_dom();
        }
        let pane_w = state.effective_mp_width(viewport);
        let layout =
            crate::mp_layout::session_layout(0.0, pane_w, 5, state.mp_pan_px, state.mp_zoom, scale);
        state.reconcile_mp_pan(layout.content_width, layout.strip_viewport.w);
        assert_eq!(
            state.mp_pan_px,
            current_session_rest_pan(layout.content_width, layout.strip_viewport.w)
        );
        assert!(!state.reconcile_mp_pan(layout.content_width, layout.strip_viewport.w));
    }
}

#[test]
fn cursor_zoom_rearms_at_new_rest() {
    let mut state = PaneState::default();
    state.reconcile_mp_pan(700.0, 400.0);
    assert!(state.navigate_mp_zoom(1.1, 330.0, 730.0, 400.0));
    assert!(state.mp_at_rest());

    assert!(state.navigate_mp_zoom(1.2, 250.0, 760.0, 400.0));
    assert!(!state.mp_at_rest());
    assert!(!state.reconcile_mp_pan(900.0, 400.0));
    assert_eq!(state.mp_pan_px, 250.0);

    // Reaching the new rest bound re-arms automatic rest.
    assert!(state.navigate_mp_zoom(1.3, 500.0, 900.0, 400.0));
    assert!(state.mp_at_rest());
    assert!(state.reconcile_mp_pan(1_000.0, 400.0));
    assert_eq!(state.mp_pan_px, 600.0);
}

#[test]
fn automatic_center_stays_inside_navigation_range() {
    let state = PaneState::default();
    let mut dom = dom(&[100, 120]);
    dom.best_bid = Some(Price(90));
    dom.best_ask = Some(Price(92));
    assert_eq!(
        state.navigation_center(&profile(&[80, 140]), &dom),
        Some(Price(91))
    );

    dom.best_bid = Some(Price(60));
    dom.best_ask = Some(Price(62));
    assert_eq!(
        state.navigation_center(&profile(&[80, 140]), &dom),
        Some(Price(80))
    );
    assert_eq!(state.center, None);
}

#[test]
fn effective_mp_width_zero_viewport_is_degenerate() {
    let mut state = PaneState::default();
    assert_eq!(state.effective_mp_width(0.0), 1.0);
    assert_eq!(state.effective_mp_width(-40.0), 1.0);
    assert_eq!(state.effective_mp_width(f32::NAN), 1.0);
    state.toggle_dom();
    assert_eq!(state.effective_mp_width(0.0), 1.0);
}

#[test]
fn splitter_consume_zero_width_is_noop() {
    let mut split = SplitterState::default();
    split.begin(250.0);
    split.queue(400.0);
    assert!(!split.consume(0.0));
    assert!(!split.consume(-1.0));
    assert!(!split.consume(f32::NAN));
    // pending remains until a valid width arrives
    assert!(split.consume(1_000.0));
}
