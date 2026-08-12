//! Harness-side event-stream splice helper (`docs/ENGINE.md` §5.6).

use crate::{ReplayError, ReplaySource, Result};
use fft_core::CanonicalEvent;
use fft_log::LogWriter;
use std::path::Path;

const BATCH_EVENTS: usize = 4_096;

/// Copy a monotonic source range into `dst`, inserting one Gap immediately before
/// the first event with `ts > inject_after_ts`.
///
/// Events through `copy_through_ts` are copied exactly and in source order. The
/// source range is fully validated before `dst` is created: timestamps must be
/// nondecreasing, the open report must contain no warnings, `observed` must be
/// greater than `expected`, and an insertion boundary must exist. Consequently,
/// validation failure never leaves a destination that resembles a valid log.
///
/// Returns `(output_event_count, injected_gap_ts)`.
pub fn write_with_injected_gap(
    src: &Path,
    dst: &Path,
    inject_after_ts: u64,
    copy_through_ts: u64,
    expected: u64,
    observed: u64,
) -> Result<(u64, u64)> {
    if observed <= expected || expected > i64::MAX as u64 {
        return Err(ReplayError::InvalidGapSequences { expected, observed });
    }

    let mut source = ReplaySource::open(src)?;
    if !source.open_report().warnings.is_empty() {
        return Err(ReplayError::SpliceOpenWarnings(
            source.open_report().warnings.clone(),
        ));
    }

    let meta = source.meta().clone();
    let mut previous_ts = None;
    let mut event_index = 0u64;
    let mut gap_ts = None;

    while let Some(event) = source.next_event()? {
        if let Some(previous_ts) = previous_ts
            && event.ts.0 < previous_ts
        {
            return Err(ReplayError::NonMonotonicSpliceInput {
                event_index,
                previous_ts,
                observed_ts: event.ts.0,
            });
        }
        previous_ts = Some(event.ts.0);
        if gap_ts.is_none() && event.ts.0 > inject_after_ts && event.ts.0 <= copy_through_ts {
            gap_ts = Some(event.ts);
        }
        event_index += 1;
    }

    let gap_ts = gap_ts.ok_or(ReplayError::SpliceAnchorNotFound {
        inject_after_ts,
        copy_through_ts,
    })?;
    let gap = CanonicalEvent::gap(gap_ts, expected, observed);

    let mut source = ReplaySource::open(src)?;
    if !source.open_report().warnings.is_empty() {
        return Err(ReplayError::SpliceOpenWarnings(
            source.open_report().warnings.clone(),
        ));
    }
    let mut writer = LogWriter::create(dst, &meta)?;
    let write_result = (|| {
        let mut batch = Vec::with_capacity(BATCH_EVENTS);
        let mut output_events = 0u64;
        let mut injected = false;
        while let Some(event) = source.next_event()? {
            if event.ts.0 > copy_through_ts {
                break;
            }
            if !injected && event.ts.0 > inject_after_ts {
                batch.push(gap);
                output_events += 1;
                injected = true;
            }
            batch.push(event);
            output_events += 1;
            if batch.len() >= BATCH_EVENTS {
                writer.append_events(&batch)?;
                batch.clear();
            }
        }
        if !batch.is_empty() {
            writer.append_events(&batch)?;
        }
        writer.close()?;
        Ok((output_events, gap_ts.0))
    })();

    if write_result.is_err() {
        let _ = std::fs::remove_file(dst);
    }
    write_result
}
