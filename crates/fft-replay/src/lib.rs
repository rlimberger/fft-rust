//! File replay over an [`fft_log::LogReader`], including exact checkpoint
//! restoration and cancellable tail replay.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

mod error;
mod ordinal;
mod source;
mod splice;

pub use error::{ReplayError, Result};
pub use source::{ForwardProgress, ReplaySource, SeekReport};
pub use splice::write_with_injected_gap;
