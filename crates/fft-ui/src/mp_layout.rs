//! Pure Market Profile strip and semantic-line geometry.

/// Row height at OS scale 1.0.
pub const MP_ROW_H: f32 = 16.0;
/// Footer height at OS scale 1.0.
pub const MP_FOOTER_H: f32 = 22.0;

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
}
