//! Source-event ordinal helpers.
//!
//! The frame index stores only `offset` / `kind` / `first_ts` / `first_seq` — not
//! per-frame canonical event counts. EVENTS frame headers' `count` includes TsReset
//! wire records that never become [`fft_core::CanonicalEvent`]s, so an exact ordinal
//! at a restore frame cannot be read from the index alone. Counting therefore
//! decodes every EVENTS frame before the restore point: **O(events preceding the
//! cursor)** once per checkpoint restore / prior-build setup.

use crate::Result;
use fft_log::LogReader;

/// Count decoded source events in frames `[0, frame)`.
pub(crate) fn events_before_frame(reader: &LogReader, frame: usize) -> Result<u64> {
    let mut n = 0u64;
    for item in reader.events(0..frame) {
        item?;
        n += 1;
    }
    Ok(n)
}
