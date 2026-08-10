//! Pure Market Profile strip and semantic-line geometry.

pub const MP_ROW_H: f32 = 16.0;
pub const MP_FOOTER_H: f32 = 22.0;

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

pub fn max_rows(height: f32) -> usize {
    if !height.is_finite() || height <= MP_FOOTER_H {
        return 0;
    }
    ((height - MP_FOOTER_H) / MP_ROW_H).floor() as usize
}

pub fn row_y(origin_y: f32, from_top: usize) -> f32 {
    origin_y + from_top as f32 * MP_ROW_H
}

pub fn volume_width(value: u64, max: u64, available: f32) -> f32 {
    if value == 0 || max == 0 || available <= 0.0 {
        return 0.0;
    }
    ((value as f64 / max as f64) as f32 * available).max(1.0)
}

/// Y coordinate at the center of a semantic price row in a descending window.
pub fn price_line_y(price: i64, top_price: i64, scaled_tick: i64, origin_y: f32) -> Option<f32> {
    if scaled_tick <= 0 {
        return None;
    }
    let delta = top_price.checked_sub(price)?;
    if delta < 0 || delta % scaled_tick != 0 {
        return None;
    }
    Some(row_y(origin_y, (delta / scaled_tick) as usize) + MP_ROW_H / 2.0)
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
    fn va_line_placement_uses_descending_price_rows() {
        assert_eq!(price_line_y(100, 104, 2, 10.0), Some(50.0));
        assert_eq!(price_line_y(101, 104, 2, 10.0), None);
        assert_eq!(price_line_y(106, 104, 2, 10.0), None);
    }
}
