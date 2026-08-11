//! Pure DOM ladder layout math — no GPUI types, unit-tested headless.

/// Column labels in Daytradr order (PRD §5).
pub const COL_LABELS: [&str; 6] = ["PRICE", "VOL", "BID", "cB", "cA", "ASK"];

/// Relative column widths; must sum to 1.0.
pub const COL_FRACTIONS: [f32; 6] = [0.18, 0.14, 0.22, 0.10, 0.10, 0.26];

/// Header strip height in logical pixels at scale 1.0.
pub const HEADER_H: f32 = 22.0;

/// One price-row height in logical pixels (tick scale 1) at OS scale 1.0.
pub const ROW_H: f32 = 18.0;

/// Scaled header height: `HEADER_H * scale`.
#[inline]
pub fn header_h(scale: f32) -> f32 {
    HEADER_H * scale
}

/// Scaled row height: `ROW_H * scale`.
#[inline]
pub fn row_h(scale: f32) -> f32 {
    ROW_H * scale
}

/// Axis-aligned column strip: `x` origin and `width` within the ladder bounds.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ColRect {
    pub x: f32,
    pub w: f32,
}

/// Lay out the six DOM columns across `[origin_x, origin_x + width)`.
pub fn column_rects(origin_x: f32, width: f32) -> [ColRect; 6] {
    debug_assert!(
        (COL_FRACTIONS.iter().sum::<f32>() - 1.0).abs() < 1e-3,
        "column fractions must sum to 1"
    );
    let mut x = origin_x;
    let mut out = [ColRect { x: 0.0, w: 0.0 }; 6];
    for (i, &frac) in COL_FRACTIONS.iter().enumerate() {
        let w = width * frac;
        out[i] = ColRect { x, w };
        x += w;
    }
    out
}

/// How many body rows fit under the header inside `body_height`.
pub fn max_visible_rows(body_height: f32, scale: f32) -> usize {
    let rh = row_h(scale);
    if body_height <= 0.0 || !body_height.is_finite() || rh <= 0.0 {
        return 0;
    }
    (body_height / rh).floor() as usize
}

/// Inclusive-exclusive `[start, end)` window of ascending-price rows centered on
/// `center` (or the middle of the book when `center` is `None`), capped at `max_rows`.
pub fn visible_row_window(
    row_count: usize,
    center: Option<usize>,
    max_rows: usize,
) -> (usize, usize) {
    if row_count == 0 || max_rows == 0 {
        return (0, 0);
    }
    let max_rows = max_rows.min(row_count);
    let center = center.unwrap_or(row_count / 2).min(row_count - 1);
    let half = max_rows / 2;
    let mut start = center.saturating_sub(half);
    let mut end = start + max_rows;
    if end > row_count {
        end = row_count;
        start = end.saturating_sub(max_rows);
    }
    (start, end)
}

/// Top edge of the row drawn `row_from_top` rows below the header (row 0 = highest price).
pub fn row_top_y(origin_y: f32, row_from_top: usize, scale: f32) -> f32 {
    origin_y + header_h(scale) + row_from_top as f32 * row_h(scale)
}

/// Map a price to a row index from the top of a descending ladder window.
///
/// `top_price` is the highest price shown; `tick` is the instrument tick in 1e-9 units.
/// Returns `None` when `tick <= 0` or `price` is not on the tick grid relative to `top_price`.
pub fn price_to_row_from_top(price: i64, top_price: i64, tick: i64) -> Option<i64> {
    if tick <= 0 {
        return None;
    }
    let delta = top_price - price;
    if delta % tick != 0 {
        return None;
    }
    Some(delta / tick)
}

/// Y coordinate of the top of the row for `price` within a descending window.
pub fn price_to_y(price: i64, top_price: i64, tick: i64, origin_y: f32, scale: f32) -> Option<f32> {
    let row = price_to_row_from_top(price, top_price, tick)?;
    if row < 0 {
        return None;
    }
    Some(row_top_y(origin_y, row as usize, scale))
}

/// Depth-block width inside a side column, scaled to the window's max size on that side.
pub fn depth_block_width(size: u64, max_size: u64, col_width: f32) -> f32 {
    if size == 0 || max_size == 0 || col_width <= 0.0 {
        return 0.0;
    }
    let frac = (size as f64 / max_size as f64).clamp(0.0, 1.0) as f32;
    (col_width * frac).max(1.0)
}

/// Format a 1e-9 fixed-point price for the PRICE column.
pub fn format_price(price_1e9: i64) -> String {
    format!("{:.2}", price_1e9 as f64 / 1_000_000_000.0)
}

/// Format a size/volume counter; empty string when zero so the ladder stays quiet.
pub fn format_size(size: u64) -> String {
    if size == 0 {
        String::new()
    } else {
        size.to_string()
    }
}

/// True when `price` sits on the inside-market band (best bid … best ask).
pub fn is_inside_market(price: i64, best_bid: Option<i64>, best_ask: Option<i64>) -> bool {
    match (best_bid, best_ask) {
        (Some(b), Some(a)) => price >= b && price <= a,
        (Some(b), None) => price == b,
        (None, Some(a)) => price == a,
        (None, None) => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn columns_cover_full_width() {
        let cols = column_rects(10.0, 100.0);
        assert!((cols[0].x - 10.0).abs() < 1e-4);
        let right = cols[5].x + cols[5].w;
        assert!((right - 110.0).abs() < 1e-3);
        for i in 0..5 {
            assert!((cols[i].x + cols[i].w - cols[i + 1].x).abs() < 1e-4);
        }
    }

    #[test]
    fn visible_window_centers_and_clamps() {
        assert_eq!(visible_row_window(0, Some(0), 10), (0, 0));
        assert_eq!(visible_row_window(5, Some(2), 3), (1, 4));
        assert_eq!(visible_row_window(5, Some(0), 3), (0, 3));
        assert_eq!(visible_row_window(5, Some(4), 3), (2, 5));
        assert_eq!(visible_row_window(5, None, 10), (0, 5));
    }

    #[test]
    fn price_to_y_descends_with_price() {
        let tick = 250_000_000;
        let top = 5_000_000_000_000;
        let y0 = price_to_y(top, top, tick, 0.0, 1.0).unwrap();
        let y1 = price_to_y(top - tick, top, tick, 0.0, 1.0).unwrap();
        assert!((y0 - HEADER_H).abs() < 1e-4);
        assert!((y1 - (HEADER_H + ROW_H)).abs() < 1e-4);
        assert!(y1 > y0);
    }

    #[test]
    fn scale_multiplies_row_metrics() {
        assert!((row_h(1.0) - ROW_H).abs() < 1e-6);
        assert!((row_h(1.5) - ROW_H * 1.5).abs() < 1e-6);
        assert!((header_h(1.5) - HEADER_H * 1.5).abs() < 1e-6);
        assert_eq!(max_visible_rows(180.0, 1.0), 10);
        assert_eq!(max_visible_rows(180.0, 1.5), 6); // 180 / 27 = 6.66 → 6
        let y1 = row_top_y(0.0, 1, 1.0);
        let y15 = row_top_y(0.0, 1, 1.5);
        assert!((y1 - (HEADER_H + ROW_H)).abs() < 1e-4);
        assert!((y15 - (HEADER_H * 1.5 + ROW_H * 1.5)).abs() < 1e-4);
    }

    #[test]
    fn depth_block_scales_and_floors() {
        assert_eq!(depth_block_width(0, 100, 50.0), 0.0);
        assert_eq!(depth_block_width(50, 100, 50.0), 25.0);
        assert_eq!(depth_block_width(100, 100, 50.0), 50.0);
        assert_eq!(depth_block_width(1, 1000, 50.0), 1.0);
    }

    #[test]
    fn inside_market_band() {
        assert!(is_inside_market(100, Some(100), Some(101)));
        assert!(is_inside_market(101, Some(100), Some(101)));
        assert!(!is_inside_market(99, Some(100), Some(101)));
        assert!(is_inside_market(100, Some(100), None));
        assert!(!is_inside_market(100, None, None));
    }

    #[test]
    fn format_price_two_decimals() {
        assert_eq!(format_price(5_000_250_000_000), "5000.25");
        assert_eq!(format_price(0), "0.00");
    }
}
