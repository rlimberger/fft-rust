//! Fractional pointer motion coalescing for pane panning, plus DOM hover-row math.

use crate::mp_layout::{AXIS_DOMINANCE_PX, DragAxis, classify_drag_axis};

/// Map a window-space pointer y onto a visible body row index from the top.
///
/// `origin_y` is the ladder element's top; body rows start at
/// `origin_y + header_h(scale)`. Returns `None` in the header, below the body,
/// or outside `[0, visible_rows)`.
pub fn hover_row_from_y(
    window_y: f32,
    origin_y: f32,
    scale: f32,
    visible_rows: usize,
) -> Option<usize> {
    use crate::layout::{header_h, row_h};

    assert!(
        window_y.is_finite() && origin_y.is_finite() && scale.is_finite(),
        "DOM hover geometry must be finite"
    );
    assert!(scale > 0.0, "DOM hover scale must be positive");
    if visible_rows == 0 {
        return None;
    }
    let hh = header_h(scale);
    let rh = row_h(scale);
    let local_y = window_y - origin_y;
    if local_y < hh {
        return None;
    }
    let from_top = ((local_y - hh) / rh).floor() as isize;
    if from_top < 0 {
        return None;
    }
    let from_top = from_top as usize;
    if from_top >= visible_rows {
        None
    } else {
        Some(from_top)
    }
}

/// Convert a from-top hover index into an ascending-price `rows[row_range]` index.
pub fn hover_row_index(row_range: &std::ops::Range<usize>, from_top: usize) -> Option<usize> {
    let count = row_range.end.saturating_sub(row_range.start);
    if from_top >= count {
        return None;
    }
    Some(row_range.start + (count - 1 - from_top))
}

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
    use super::{DomInput, PaneDrag, hover_row_from_y, hover_row_index};
    use crate::mp_layout::DragAxis;

    #[test]
    fn hover_row_skips_header_and_respects_scale() {
        // scale 1: header 22, row 18; origin_y = 100
        assert_eq!(hover_row_from_y(110.0, 100.0, 1.0, 5), None);
        assert_eq!(hover_row_from_y(122.0, 100.0, 1.0, 5), Some(0));
        assert_eq!(hover_row_from_y(139.9, 100.0, 1.0, 5), Some(0));
        assert_eq!(hover_row_from_y(140.0, 100.0, 1.0, 5), Some(1));
        assert_eq!(
            hover_row_from_y(100.0 + 22.0 + 18.0 * 5.0, 100.0, 1.0, 5),
            None
        );
        // scale 2 geometry (header/row doubled)
        assert_eq!(hover_row_from_y(44.0, 0.0, 2.0, 3), Some(0));
        assert_eq!(hover_row_from_y(80.0, 0.0, 2.0, 3), Some(1));
        assert_eq!(hover_row_from_y(43.9, 0.0, 2.0, 3), None);
    }

    #[test]
    fn hover_row_index_maps_descending_window() {
        assert_eq!(hover_row_index(&(2..6), 0), Some(5));
        assert_eq!(hover_row_index(&(2..6), 3), Some(2));
        assert_eq!(hover_row_index(&(2..6), 4), None);
        assert_eq!(hover_row_index(&(0..0), 0), None);
    }

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

    /// Wheel over the DOM produces row pan deltas (no zoom path — DOM has none).
    #[test]
    fn wheel_over_dom_produces_row_pan() {
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
