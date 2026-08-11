//! Pure iceberg-badge helpers for the DOM ladder — engine state only, no detection.

use crate::dom_view::DomViewRow;
use crate::layout::ColRect;

/// Badge edge length in logical pixels at scale 1.0 (~6 px × scale).
pub const BADGE_SIZE: f32 = 6.0;
/// Alias for ladder paint paths that import the mission-brief names.
pub const ICEBERG_BADGE_PX: f32 = BADGE_SIZE;

/// Gap between badge and reload-count text inside the depth block.
pub const BADGE_TEXT_GAP: f32 = 2.0;
/// Alias for ladder paint paths that import the mission-brief names.
pub const ICEBERG_COUNT_GAP_PX: f32 = BADGE_TEXT_GAP;

/// Side of the ladder that owns a native-refresh badge.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum IcebergSide {
    Bid,
    Ask,
}

/// True when this side has observed native-refresh activity (`count > 0`).
#[inline]
pub fn iceberg_badge_visible(count: u32) -> bool {
    count > 0
}

/// Reload-count label painted next to the badge (`"×N"`).
#[inline]
pub fn format_reload_count(count: u32) -> String {
    format!("×{count}")
}

/// Axis-aligned badge square at the inner edge of a side's depth column.
///
/// Bid: right edge of BID. Ask: left edge of ASK.
pub fn iceberg_badge_bounds(
    side: IcebergSide,
    col: ColRect,
    row_y: f32,
    row_h: f32,
    scale: f32,
) -> (f32, f32, f32, f32) {
    let size = BADGE_SIZE * scale;
    let y = row_y + ((row_h - size) * 0.5).max(0.0);
    let x = match side {
        IcebergSide::Bid => col.x + col.w - size - 2.0 * scale,
        IcebergSide::Ask => col.x + 2.0 * scale,
    };
    (x, y, size, size)
}

/// Origin for the `"×N"` label when it fits beside the badge inside the column.
///
/// Bid text sits left of the badge (right-aligned into that inset). Ask text sits
/// right of the badge (left-aligned). Returns `None` when the remaining inset is
/// too narrow for a meaningful glyph run.
pub fn reload_count_text_origin(
    side: IcebergSide,
    col: ColRect,
    badge_x: f32,
    badge_w: f32,
    row_y: f32,
    scale: f32,
    text_width: f32,
) -> Option<(f32, f32, f32)> {
    let gap = BADGE_TEXT_GAP * scale;
    let pad = 4.0 * scale;
    let y = row_y + 2.0 * scale;
    match side {
        IcebergSide::Bid => {
            let right = badge_x - gap;
            let left = col.x + pad;
            let avail = right - left;
            if avail < text_width || avail < 8.0 * scale {
                return None;
            }
            Some((left, y, avail))
        }
        IcebergSide::Ask => {
            let left = badge_x + badge_w + gap;
            let right = col.x + col.w - pad;
            let avail = right - left;
            if avail < text_width || avail < 8.0 * scale {
                return None;
            }
            Some((left, y, avail))
        }
    }
}

/// Per-side visibility derived from an aggregated render row.
#[inline]
pub fn row_iceberg_sides(row: &DomViewRow) -> (bool, bool) {
    (
        iceberg_badge_visible(row.refresh_bid_count),
        iceberg_badge_visible(row.refresh_ask_count),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dom_view::DomViewRow;

    #[test]
    fn badge_visible_only_when_count_positive() {
        assert!(!iceberg_badge_visible(0));
        assert!(iceberg_badge_visible(1));
        assert!(iceberg_badge_visible(u32::MAX));
    }

    #[test]
    fn row_sides_mirror_engine_counts() {
        let mut row = DomViewRow::default();
        assert_eq!(row_iceberg_sides(&row), (false, false));
        row.refresh_bid_count = 2;
        assert_eq!(row_iceberg_sides(&row), (true, false));
        row.refresh_ask_count = 1;
        assert_eq!(row_iceberg_sides(&row), (true, true));
    }

    #[test]
    fn reload_count_format() {
        assert_eq!(format_reload_count(1), "×1");
        assert_eq!(format_reload_count(12), "×12");
    }

    #[test]
    fn badge_sits_on_inner_depth_edge() {
        let bid = ColRect { x: 100.0, w: 40.0 };
        let ask = ColRect { x: 200.0, w: 50.0 };
        let (bx, by, bw, bh) = iceberg_badge_bounds(IcebergSide::Bid, bid, 10.0, 18.0, 1.0);
        assert!((bw - BADGE_SIZE).abs() < 1e-4);
        assert!((bh - BADGE_SIZE).abs() < 1e-4);
        assert!((bx - (100.0 + 40.0 - BADGE_SIZE - 2.0)).abs() < 1e-4);
        assert!((by - (10.0 + (18.0 - BADGE_SIZE) * 0.5)).abs() < 1e-4);

        let (ax, _, aw, _) = iceberg_badge_bounds(IcebergSide::Ask, ask, 10.0, 18.0, 1.0);
        assert!((aw - BADGE_SIZE).abs() < 1e-4);
        assert!((ax - (200.0 + 2.0)).abs() < 1e-4);
    }

    #[test]
    fn reload_text_skips_when_column_too_tight() {
        let narrow = ColRect { x: 0.0, w: 10.0 };
        let (bx, _, bw, _) = iceberg_badge_bounds(IcebergSide::Ask, narrow, 0.0, 18.0, 1.0);
        assert!(
            reload_count_text_origin(IcebergSide::Ask, narrow, bx, bw, 0.0, 1.0, 12.0).is_none()
        );

        let wide = ColRect { x: 0.0, w: 80.0 };
        let (bx, _, bw, _) = iceberg_badge_bounds(IcebergSide::Ask, wide, 0.0, 18.0, 1.0);
        let origin =
            reload_count_text_origin(IcebergSide::Ask, wide, bx, bw, 0.0, 1.0, 12.0).unwrap();
        assert!(origin.2 >= 12.0);
    }
}
