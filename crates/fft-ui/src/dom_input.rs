//! Fractional pointer motion coalescing for pane panning.

use crate::mp_layout::{AXIS_DOMINANCE_PX, DragAxis, classify_drag_axis};

/// Result of one pointer-move sample during an active drag.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum PaneDrag {
    None,
    Vertical(i64),
    Horizontal(f32),
}

/// Pointer state shared by the ladder/MP wrappers' direct GPUI listeners.
#[derive(Debug)]
pub struct DomInput {
    drag_x: Option<f32>,
    drag_y: Option<f32>,
    start_x: f32,
    start_y: f32,
    axis: DragAxis,
    drag_rows: RowAccumulator,
    wheel_rows: RowAccumulator,
}

impl Default for DomInput {
    fn default() -> Self {
        Self {
            drag_x: None,
            drag_y: None,
            start_x: 0.0,
            start_y: 0.0,
            axis: DragAxis::Undecided,
            drag_rows: RowAccumulator::default(),
            wheel_rows: RowAccumulator::default(),
        }
    }
}

impl DomInput {
    /// Start a drag at window-space coordinates.
    pub fn begin_drag(&mut self, x: f32, y: f32) {
        assert!(
            x.is_finite() && y.is_finite(),
            "drag coordinate must be finite"
        );
        self.drag_x = Some(x);
        self.drag_y = Some(y);
        self.start_x = x;
        self.start_y = y;
        self.axis = DragAxis::Undecided;
        self.drag_rows.reset();
    }

    /// Consume pointer motion. Vertical returns completed rendered-row steps;
    /// horizontal returns the latest dx in px (content-space: caller subtracts from pan).
    pub fn drag_to(&mut self, x: f32, y: f32, row_height: f32) -> PaneDrag {
        assert!(
            x.is_finite() && y.is_finite(),
            "drag coordinate must be finite"
        );
        assert!(
            row_height.is_finite() && row_height > 0.0,
            "row height must be positive and finite"
        );
        let (Some(prev_x), Some(prev_y)) = (self.drag_x, self.drag_y) else {
            return PaneDrag::None;
        };
        self.drag_x = Some(x);
        self.drag_y = Some(y);
        if self.axis == DragAxis::Undecided {
            self.axis = classify_drag_axis(x - self.start_x, y - self.start_y, AXIS_DOMINANCE_PX);
        }
        match self.axis {
            DragAxis::Undecided => PaneDrag::None,
            DragAxis::Vertical => {
                let delta = self.drag_rows.push((y - prev_y) / row_height);
                if delta == 0 {
                    PaneDrag::None
                } else {
                    PaneDrag::Vertical(delta)
                }
            }
            DragAxis::Horizontal => PaneDrag::Horizontal(x - prev_x),
        }
    }

    /// End a pointer drag and discard any sub-row remainder.
    pub fn end_drag(&mut self) {
        self.drag_x = None;
        self.drag_y = None;
        self.axis = DragAxis::Undecided;
        self.drag_rows.reset();
    }

    /// Whether a pointer drag is active.
    pub fn is_dragging(&self) -> bool {
        self.drag_y.is_some()
    }

    /// Active drag axis once classified.
    pub fn axis(&self) -> DragAxis {
        self.axis
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
    use super::{DomInput, PaneDrag};
    use crate::mp_layout::DragAxis;

    #[test]
    fn one_drag_row_is_one_price_row() {
        let mut input = DomInput::default();
        input.begin_drag(0.0, 100.0);
        assert_eq!(input.drag_to(0.0, 118.0, 18.0), PaneDrag::Vertical(1));
        assert_eq!(input.drag_to(0.0, 100.0, 18.0), PaneDrag::Vertical(-1));
    }

    #[test]
    fn fractional_drag_motion_coalesces() {
        let mut input = DomInput::default();
        input.begin_drag(0.0, 0.0);
        assert_eq!(input.drag_to(0.0, 9.0, 18.0), PaneDrag::None);
        assert_eq!(input.drag_to(0.0, 18.0, 18.0), PaneDrag::Vertical(1));
        input.end_drag();
        assert_eq!(input.drag_to(0.0, 36.0, 18.0), PaneDrag::None);
        assert!(!input.is_dragging());
    }

    #[test]
    fn horizontal_axis_locks_and_reports_dx() {
        let mut input = DomInput::default();
        input.begin_drag(100.0, 100.0);
        assert_eq!(input.drag_to(101.0, 100.5, 16.0), PaneDrag::None);
        assert_eq!(input.axis(), DragAxis::Undecided);
        assert_eq!(input.drag_to(104.0, 100.5, 16.0), PaneDrag::Horizontal(3.0));
        assert_eq!(input.axis(), DragAxis::Horizontal);
        // Once horizontal, vertical motion does not flip the axis.
        assert_eq!(input.drag_to(110.0, 140.0, 16.0), PaneDrag::Horizontal(6.0));
        assert_eq!(input.axis(), DragAxis::Horizontal);
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
