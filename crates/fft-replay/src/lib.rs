//! File replay over an [`fft_log::LogReader`], including exact checkpoint
//! restoration and cancellable tail replay.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

mod error;
mod source;

pub use error::{ReplayError, Result};
pub use source::{ForwardProgress, ReplaySource, SeekReport};
