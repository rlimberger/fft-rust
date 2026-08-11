//! Aggregation of native-refresh (iceberg) fields across tick scales.

use fft_core::Price;
use fft_engine::{DomPriceRow, DomRenderState, PriceRefreshRender};
use fft_ui::dom_view::aggregate_rows;

fn refresh_row(
    price: i64,
    bid_count: u32,
    ask_count: u32,
    bid_hidden: u64,
    ask_hidden: u64,
) -> DomPriceRow {
    DomPriceRow {
        price: Price(price),
        refresh_agg: PriceRefreshRender {
            bid_count,
            ask_count,
            bid_hidden,
            ask_hidden,
        },
        ..Default::default()
    }
}

#[test]
fn scale_two_and_four_sum_refresh_counts_and_hidden() {
    let source = DomRenderState {
        tick_size: Price(5),
        rows: vec![
            refresh_row(0, 1, 2, 10, 20),
            refresh_row(5, 3, 4, 30, 40),
            refresh_row(10, 5, 6, 50, 60),
            refresh_row(15, 7, 8, 70, 80),
        ],
        ..Default::default()
    };

    let scale2 = aggregate_rows(&source, 2);
    assert_eq!(scale2.rows.len(), 2);
    assert_eq!(
        (
            scale2.rows[0].refresh_bid_count,
            scale2.rows[0].refresh_ask_count,
            scale2.rows[0].refresh_bid_hidden,
            scale2.rows[0].refresh_ask_hidden
        ),
        (4, 6, 40, 60)
    );
    assert_eq!(
        (
            scale2.rows[1].refresh_bid_count,
            scale2.rows[1].refresh_ask_count,
            scale2.rows[1].refresh_bid_hidden,
            scale2.rows[1].refresh_ask_hidden
        ),
        (12, 14, 120, 140)
    );

    let scale4 = aggregate_rows(&source, 4);
    assert_eq!(scale4.rows.len(), 1);
    assert_eq!(
        (
            scale4.rows[0].refresh_bid_count,
            scale4.rows[0].refresh_ask_count,
            scale4.rows[0].refresh_bid_hidden,
            scale4.rows[0].refresh_ask_hidden
        ),
        (16, 20, 160, 200)
    );
}

#[test]
fn zero_refresh_fields_stay_zero_across_scales() {
    let source = DomRenderState {
        tick_size: Price(5),
        rows: vec![
            DomPriceRow {
                price: Price(0),
                bid_size: 9,
                ..Default::default()
            },
            DomPriceRow {
                price: Price(5),
                ask_size: 9,
                ..Default::default()
            },
            DomPriceRow {
                price: Price(10),
                session_volume: 9,
                ..Default::default()
            },
            DomPriceRow {
                price: Price(15),
                ..Default::default()
            },
        ],
        ..Default::default()
    };
    for scale in [1u8, 2, 4] {
        for row in &aggregate_rows(&source, scale).rows {
            assert_eq!(row.refresh_bid_count, 0);
            assert_eq!(row.refresh_ask_count, 0);
            assert_eq!(row.refresh_bid_hidden, 0);
            assert_eq!(row.refresh_ask_hidden, 0);
        }
    }
}
