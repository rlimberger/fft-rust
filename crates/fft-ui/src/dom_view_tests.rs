use super::*;

fn row(price: i64, value: u64) -> DomPriceRow {
    let mut row = DomPriceRow::default();
    row.price.0 = price;
    row.bid_size = value;
    row.ask_size = value;
    row.bid_orders = u32::try_from(value).unwrap();
    row.ask_orders = u32::try_from(value).unwrap();
    row.session_volume = value;
    row.cb = value;
    row.ca = value;
    row.bid_added_5s = u32::try_from(value).unwrap();
    row.bid_cancelled_5s = u32::try_from(value).unwrap();
    row.ask_added_5s = u32::try_from(value).unwrap();
    row.ask_cancelled_5s = u32::try_from(value).unwrap();
    row.refresh_agg.bid_count = u32::try_from(value).unwrap();
    row.refresh_agg.ask_count = u32::try_from(value).unwrap();
    row.refresh_agg.bid_hidden = value;
    row.refresh_agg.ask_hidden = value;
    row
}

fn dom(tick: i64, prices: &[i64]) -> DomRenderState {
    let mut dom = DomRenderState::default();
    dom.tick_size.0 = tick;
    dom.rows = prices.iter().map(|&price| row(price, 1)).collect();
    dom
}

#[test]
fn empty_book_is_a_no_op() {
    let aggregated = DomView::default().aggregate(&dom(5, &[]));
    assert!(aggregated.rows.is_empty());
    let mut view = DomView::default();
    assert!(!view.pan_rows(&aggregated, 10));
    assert_eq!(view.anchor, None);
    assert_eq!(view.window_range(&aggregated, 7), 0..0);
}

#[test]
fn default_engine_state_aggregates_for_instant_shell() {
    assert_eq!(
        aggregate_rows(&DomRenderState::default(), 1),
        AggregatedDom::default()
    );
}

#[test]
fn aggregates_exact_boundaries_and_all_metadata() {
    let aggregated = aggregate_rows(&dom(5, &[0, 5, 10, 15]), 2);
    assert_eq!(aggregated.rows.len(), 2);
    assert_eq!(aggregated.rows[0].price, Price(0));
    assert_eq!(aggregated.rows[1].price, Price(10));
    let first = aggregated.rows[0];
    assert_eq!(
        (first.bid_size, first.ask_size, first.session_volume),
        (2, 2, 2)
    );
    assert_eq!((first.cb, first.ca), (2, 2));
    assert_eq!((first.bid_added_5s, first.ask_cancelled_5s), (2, 2));
    assert_eq!((first.bid_orders, first.ask_orders), (2, 2));
    assert_eq!((first.refresh_bid_count, first.refresh_ask_count), (2, 2));
    assert_eq!((first.refresh_bid_hidden, first.refresh_ask_hidden), (2, 2));
}

#[test]
fn aggregates_order_counts_across_scales() {
    let mut source = DomRenderState::default();
    source.tick_size.0 = 5;
    source.rows = vec![
        {
            let mut r = DomPriceRow::default();
            r.price.0 = 0;
            r.bid_orders = 1;
            r.ask_orders = 2;
            r
        },
        {
            let mut r = DomPriceRow::default();
            r.price.0 = 5;
            r.bid_orders = 3;
            r.ask_orders = 4;
            r
        },
        {
            let mut r = DomPriceRow::default();
            r.price.0 = 10;
            r.bid_orders = 5;
            r.ask_orders = 6;
            r
        },
        {
            let mut r = DomPriceRow::default();
            r.price.0 = 15;
            r.bid_orders = 7;
            r.ask_orders = 8;
            r
        },
    ];
    let scale2 = aggregate_rows(&source, 2);
    assert_eq!(
        (scale2.rows[0].bid_orders, scale2.rows[0].ask_orders),
        (4, 6)
    );
    assert_eq!(
        (scale2.rows[1].bid_orders, scale2.rows[1].ask_orders),
        (12, 14)
    );
    let scale4 = aggregate_rows(&source, 4);
    assert_eq!(
        (scale4.rows[0].bid_orders, scale4.rows[0].ask_orders),
        (16, 20)
    );
}

#[test]
fn scales_one_two_and_four() {
    let source = dom(5, &[0, 5, 10, 15]);
    assert_eq!(aggregate_rows(&source, 1).rows.len(), 4);
    assert_eq!(aggregate_rows(&source, 2).rows.len(), 2);
    assert_eq!(aggregate_rows(&source, 4).rows.len(), 1);
}

#[test]
fn odd_source_count_keeps_partial_edge_bucket() {
    let aggregated = aggregate_rows(&dom(5, &[0, 5, 10, 15, 20]), 2);
    assert_eq!(
        aggregated
            .rows
            .iter()
            .map(|row| (row.price, row.bid_size))
            .collect::<Vec<_>>(),
        vec![(Price(0), 2), (Price(10), 2), (Price(20), 1)]
    );
}

#[test]
fn inside_prices_map_to_containing_bucket() {
    let mut source = dom(5, &[0, 5, 10, 15]);
    source.best_bid = Some(source.rows[1].price);
    source.best_ask = Some(source.rows[2].price);
    let aggregated = aggregate_rows(&source, 4);
    assert_eq!(aggregated.best_bid, Some(Price(0)));
    assert_eq!(aggregated.best_ask, Some(Price(0)));
}

#[test]
fn follow_mode_preserves_inside_midpoint_ceiling() {
    let mut source = dom(10, &[0, 10]);
    source.best_bid = Some(Price(0));
    source.best_ask = Some(Price(10));
    let aggregated = aggregate_rows(&source, 1);
    assert_eq!(DomView::default().window_range(&aggregated, 1), 1..2);
}

#[test]
fn odd_window_centers_and_shifts_at_edges() {
    let aggregated = aggregate_rows(&dom(1, &[0, 1, 2, 3, 4]), 1);
    let mut view = DomView {
        anchor: Some(Price(2)),
        ..DomView::default()
    };
    assert_eq!(view.window_range(&aggregated, 3), 1..4);
    view.anchor = Some(Price(0));
    assert_eq!(view.window_range(&aggregated, 3), 0..3);
    view.anchor = Some(Price(4));
    assert_eq!(view.window_range(&aggregated, 3), 2..5);
}

#[test]
fn follow_mode_keeps_existing_edge_clamped_source() {
    let mut source = dom(1, &[10, 11, 12, 13, 14]);
    source.best_bid = Some(Price(10));
    let view = DomView::default();
    let visible = view.aggregate_window(&source, 5);
    assert_eq!(visible, view.aggregate(&source));
    assert_eq!(view.window_range(&visible, 3), 0..3);
}

fn prices(dom: &AggregatedDom) -> Vec<i64> {
    dom.rows.iter().map(|row| row.price.0).collect()
}

fn assert_empty(row: &DomViewRow) {
    let price = row.price;
    assert_eq!(
        *row,
        DomViewRow {
            price,
            ..DomViewRow::default()
        }
    );
}

#[test]
fn linked_anchor_above_range_synthesizes_centered_empty_rows() {
    let view = DomView {
        anchor: Some(Price(20)),
        tick_scale: 1,
    };
    let visible = view.aggregate_window(&dom(1, &[10, 11, 12]), 5);
    assert_eq!(prices(&visible), vec![18, 19, 20, 21, 22]);
    assert!(visible.rows.iter().all(|row| !row.source_present));
    visible.rows.iter().for_each(assert_empty);
    assert_eq!(view.window_range(&visible, 5), 0..5);
}

#[test]
fn linked_anchor_below_range_synthesizes_centered_empty_rows() {
    let view = DomView {
        anchor: Some(Price(2)),
        tick_scale: 1,
    };
    let visible = view.aggregate_window(&dom(1, &[10, 11, 12]), 5);
    assert_eq!(prices(&visible), vec![0, 1, 2, 3, 4]);
    visible.rows.iter().for_each(assert_empty);
}

#[test]
fn linked_lattice_preserves_partial_source_overlap_only() {
    let view = DomView {
        anchor: Some(Price(14)),
        tick_scale: 1,
    };
    let visible = view.aggregate_window(&dom(1, &[10, 11, 12]), 5);
    assert_eq!(prices(&visible), vec![12, 13, 14, 15, 16]);
    assert_eq!(visible.rows[0].bid_size, 1);
    assert!(visible.rows[0].source_present);
    visible.rows[1..].iter().for_each(assert_empty);
}

#[test]
fn linked_anchor_inside_near_low_edge_stays_centered_with_overlap() {
    let view = DomView {
        anchor: Some(Price(11)),
        tick_scale: 1,
    };
    let visible = view.aggregate_window(&dom(1, &[10, 11, 12, 13, 14]), 5);
    assert_eq!(prices(&visible), vec![9, 10, 11, 12, 13]);
    assert_empty(&visible.rows[0]);
    assert!(visible.rows[1..].iter().all(|row| row.source_present));
    assert_eq!(view.window_range(&visible, 5), 0..5);
}

#[test]
fn linked_anchor_inside_near_high_edge_stays_centered_with_overlap() {
    let view = DomView {
        anchor: Some(Price(13)),
        tick_scale: 1,
    };
    let visible = view.aggregate_window(&dom(1, &[10, 11, 12, 13, 14]), 5);
    assert_eq!(prices(&visible), vec![11, 12, 13, 14, 15]);
    assert!(visible.rows[..4].iter().all(|row| row.source_present));
    assert_empty(&visible.rows[4]);
    assert_eq!(view.window_range(&visible, 5), 0..5);
}

#[test]
fn exact_half_window_boundaries_need_no_synthesis() {
    let source = dom(1, &[10, 11, 12, 13, 14, 15, 16]);
    for anchor in [12, 14] {
        let view = DomView {
            anchor: Some(Price(anchor)),
            tick_scale: 1,
        };
        let visible = view.aggregate_window(&source, 5);
        assert_eq!(visible, view.aggregate(&source));
        let range = view.window_range(&visible, 5);
        assert_eq!(visible.rows[range.start + 2].price, Price(anchor));
    }
}

#[test]
fn linked_empty_source_with_valid_tick_has_no_fake_data_or_inside() {
    let view = DomView {
        anchor: Some(Price(50)),
        tick_scale: 1,
    };
    let visible = view.aggregate_window(&dom(5, &[]), 3);
    assert_eq!(prices(&visible), vec![45, 50, 55]);
    assert_eq!((visible.best_bid, visible.best_ask), (None, None));
    visible.rows.iter().for_each(assert_empty);
}

#[test]
fn linked_lattice_honors_scales_one_two_and_four_at_source_edges() {
    let source = dom(5, &[0, 5, 10, 15, 20, 25, 30, 35, 40]);
    for (scale, anchor, expected, present) in [
        (1, 5, vec![-5, 0, 5, 10, 15], 4),
        (2, 10, vec![-10, 0, 10, 20, 30], 4),
        (4, 20, vec![-20, 0, 20, 40, 60], 3),
    ] {
        let view = DomView {
            anchor: Some(Price(anchor)),
            tick_scale: scale,
        };
        let visible = view.aggregate_window(&source, 5);
        assert_eq!(prices(&visible), expected);
        assert_eq!(visible.scaled_tick_size, Price(5 * i64::from(scale)));
        assert_eq!(
            visible.rows.iter().filter(|row| row.source_present).count(),
            present
        );
        assert_eq!(visible.rows[2].price, Price(anchor));
    }
}

#[test]
fn linked_odd_and_even_windows_use_upper_center_index() {
    let source = dom(1, &[10, 11, 12]);
    for (row_count, expected) in [(5, vec![8, 9, 10, 11, 12]), (4, vec![8, 9, 10, 11])] {
        let view = DomView {
            anchor: Some(Price(10)),
            tick_scale: 1,
        };
        let visible = view.aggregate_window(&source, row_count);
        assert_eq!(prices(&visible), expected);
        assert_eq!(visible.rows[row_count / 2].price, Price(10));
    }
}

#[test]
fn normal_in_range_aggregation_and_window_are_unchanged() {
    let source = dom(1, &[10, 11, 12, 13, 14]);
    let view = DomView {
        anchor: Some(Price(12)),
        tick_scale: 1,
    };
    let direct = view.aggregate(&source);
    let visible = view.aggregate_window(&source, 3);
    assert_eq!(visible, direct);
    assert_eq!(view.window_range(&visible, 3), 1..4);
    assert!(visible.rows.iter().all(|row| row.source_present));

    let scaled = DomView {
        anchor: Some(Price(12)),
        tick_scale: 2,
    };
    assert_eq!(
        scaled.aggregate_window(&source, 3),
        scaled.aggregate(&source)
    );
}

#[test]
fn panning_clamps_to_present_aggregated_rows() {
    let aggregated = aggregate_rows(&dom(1, &[10, 11, 12]), 1);
    let mut view = DomView::default();
    assert!(view.pan_rows(&aggregated, i64::MAX));
    assert_eq!(view.anchor, Some(Price(12)));
    assert!(!view.pan_rows(&aggregated, i64::MAX));
    assert!(view.pan_rows(&aggregated, i64::MIN));
    assert_eq!(view.anchor, Some(Price(10)));
}

#[test]
#[should_panic(expected = "DOM tick scale must be 1, 2, or 4")]
fn invalid_scale_panics_even_for_empty_book() {
    aggregate_rows(&dom(1, &[]), 3);
}

#[test]
#[should_panic(expected = "DOM tick size must be positive")]
fn invalid_tick_panics_for_nonempty_book() {
    aggregate_rows(&dom(0, &[0]), 1);
}

#[test]
#[should_panic(expected = "DOM tick size must be positive")]
fn invalid_tick_panics_for_inconsistent_empty_state() {
    let source = DomRenderState {
        best_bid: Some(Price(1)),
        ..DomRenderState::default()
    };
    aggregate_rows(&source, 1);
}

#[test]
fn mutation_helpers_report_changes() {
    let mut view = DomView::default();
    assert!(!view.set_tick_scale(1));
    assert!(view.set_tick_scale(2));
    assert!(!view.recenter());
    view.anchor = Some(Price(10));
    assert!(view.recenter());
}

#[test]
#[should_panic(expected = "DOM bid_size aggregation overflow")]
fn aggregation_overflow_is_loud() {
    let mut source = dom(1, &[0, 1]);
    source.rows[0].bid_size = u64::MAX;
    aggregate_rows(&source, 2);
}
