//! Fractional pointer motion coalescing for DOM price panning.

/// Pointer state shared by the ladder wrapper's direct GPUI listeners.
#[derive(Debug, Default)]
pub struct DomInput {
    drag_y: Option<f32>,
    drag_rows: RowAccumulator,
    wheel_rows: RowAccumulator,
}

impl DomInput {
    /// Start a price drag at a window-space y coordinate.
    pub fn begin_drag(&mut self, y: f32) {
        assert!(y.is_finite(), "DOM drag coordinate must be finite");
        self.drag_y = Some(y);
        self.drag_rows.reset();
    }

    /// Consume pointer motion and return completed rendered-row steps.
    /// Positive movement pans toward higher prices.
    pub fn drag_to(&mut self, y: f32, row_height: f32) -> i64 {
        assert!(y.is_finite(), "DOM drag coordinate must be finite");
        assert!(
            row_height.is_finite() && row_height > 0.0,
            "DOM row height must be positive and finite"
        );
        let Some(previous_y) = self.drag_y else {
            return 0;
        };
        self.drag_y = Some(y);
        self.drag_rows.push((y - previous_y) / row_height)
    }

    /// End a pointer drag and discard any sub-row remainder.
    pub fn end_drag(&mut self) {
        self.drag_y = None;
        self.drag_rows.reset();
    }

    /// Whether a pointer drag is active.
    pub fn is_dragging(&self) -> bool {
        self.drag_y.is_some()
    }

    /// Consume wheel motion expressed in rendered rows.
    pub fn wheel(&mut self, rows: f32) -> i64 {
        self.wheel_rows.push(rows)
    }
}

#[derive(Debug, Default)]
struct RowAccumulator {
    remainder: f32,
}

impl RowAccumulator {
    fn push(&mut self, rows: f32) -> i64 {
        assert!(rows.is_finite(), "DOM pan delta must be finite");
        let total = self.remainder + rows;
        assert!(total.is_finite(), "DOM pan accumulator overflow");
        let whole = total.trunc();
        assert!(whole.abs() < i64::MAX as f32, "DOM pan delta exceeds i64");
        self.remainder = total - whole;
        whole as i64
    }

    fn reset(&mut self) {
        self.remainder = 0.0;
    }
}

#[cfg(test)]
mod tests {
    use super::DomInput;

    #[test]
    fn one_drag_row_is_one_price_row() {
        let mut input = DomInput::default();
        input.begin_drag(100.0);
        assert_eq!(input.drag_to(118.0, 18.0), 1);
        assert_eq!(input.drag_to(100.0, 18.0), -1);
    }

    #[test]
    fn fractional_drag_motion_coalesces() {
        let mut input = DomInput::default();
        input.begin_drag(0.0);
        assert_eq!(input.drag_to(9.0, 18.0), 0);
        assert_eq!(input.drag_to(18.0, 18.0), 1);
        input.end_drag();
        assert_eq!(input.drag_to(36.0, 18.0), 0);
        assert!(!input.is_dragging());
    }

    #[test]
    fn wheel_motion_coalesces_without_crossing_directions() {
        let mut input = DomInput::default();
        assert_eq!(input.wheel(0.75), 0);
        assert_eq!(input.wheel(-0.5), 0);
        assert_eq!(input.wheel(0.75), 1);
        assert_eq!(input.wheel(-1.0), -1);
    }

    #[test]
    #[should_panic(expected = "DOM pan delta must be finite")]
    fn nonfinite_motion_panics() {
        DomInput::default().wheel(f32::NAN);
    }
}
