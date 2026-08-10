//! Panes (custom GPUI Elements) + the frame pump. Also hosts the frame-time measurement
//! harness used by the perf gate.

pub mod datetime;
pub mod dom_input;
pub mod dom_ladder;
pub mod dom_view;
pub mod frame_stats;
pub mod gate_report;
pub mod glyph_cache;
pub mod harness;
pub mod layout;
pub mod mp_element;
pub mod mp_layout;
mod mp_prepare;
pub mod mp_view;
pub mod pane_state;
pub mod shell;
pub mod theme;
