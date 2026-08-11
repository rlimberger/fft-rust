//! Snapshot admission + live trade-date filter (`docs/FFTLOG-V2.md` §4).

use fft_core::EventKind;
use jiff::civil::Date;

use crate::decode::DecodedEvent;
use crate::session::TradeDateBucketer;

use super::WriteStats;

pub(crate) fn is_snapshot(ev: &DecodedEvent) -> bool {
    ev.event.kind != EventKind::Gap && ev.event.flags & u16::from(dbn::flags::SNAPSHOT) != 0
}

/// Live (non-snapshot, non-gap) events for `instrument_id` must bucket to `trade_date`.
/// Gaps are channel-wide and keep when their observing ts buckets to `trade_date`.
pub(crate) fn keep_live_or_gap(
    ev: &DecodedEvent,
    instrument_id: u32,
    trade_date: Date,
    bucketer: &mut TradeDateBucketer,
) -> bool {
    match ev.event.kind {
        EventKind::Gap => bucketer.bucket(ev.event.ts) == trade_date,
        _ if ev.instrument_id != instrument_id => false,
        _ => bucketer.bucket(ev.event.ts) == trade_date,
    }
}

/// Per-file snapshot admission (`docs/FFTLOG-V2.md` §4): a file's SNAPSHOT block for the
/// selected instrument is admitted iff the file's **first non-snapshot** event (any
/// instrument, or a synthesized Gap which can only appear after a non-snapshot observe)
/// buckets to `trade_date`. Pending snapshots are held until that decision; stale blocks
/// are dropped and counted.
pub(crate) struct FileAdmission {
    trade_date: Date,
    instrument_id: u32,
    /// `None` until the first non-snapshot record in this file is seen.
    admit_snapshots: Option<bool>,
    pending_snapshots: Vec<DecodedEvent>,
}

impl FileAdmission {
    pub(crate) fn new(instrument_id: u32, trade_date: Date) -> Self {
        Self {
            trade_date,
            instrument_id,
            admit_snapshots: None,
            pending_snapshots: Vec::new(),
        }
    }

    fn decide(&mut self, ts: fft_core::Ts, bucketer: &mut TradeDateBucketer) -> bool {
        let admit = bucketer.bucket(ts) == self.trade_date;
        self.admit_snapshots = Some(admit);
        admit
    }

    /// Flush buffered snapshots after the admission decision is known.
    fn flush_pending(&mut self, out: &mut impl FnMut(DecodedEvent), stats: &mut WriteStats) {
        let admit = self
            .admit_snapshots
            .expect("flush_pending without decision");
        let pending = std::mem::take(&mut self.pending_snapshots);
        if admit {
            for ev in pending {
                stats.snapshots_kept += 1;
                out(ev);
            }
        } else {
            stats.snapshots_dropped += pending.len() as u64;
        }
    }

    /// End-of-file: any still-pending snapshots mean the file had no non-snapshot event
    /// to establish admission — drop them (vacuous reject) and count.
    pub(crate) fn finish(&mut self, stats: &mut WriteStats) {
        if !self.pending_snapshots.is_empty() {
            stats.snapshots_dropped += self.pending_snapshots.len() as u64;
            self.pending_snapshots.clear();
        }
    }
}

/// Consume one decoded event under §4 admission + live trade-date filter. Calls `out`
/// for each event that should be written.
///
/// `forward_hole` is true when this live event revealed a forward channel-seq hole
/// (batch filter artifact). Counted on `seq_holes_ignored` when the observing ts buckets
/// to the target trade date — same keep rule Gaps used under the prior policy.
pub(crate) fn admit_event(
    ev: DecodedEvent,
    admission: &mut FileAdmission,
    bucketer: &mut TradeDateBucketer,
    stats: &mut WriteStats,
    forward_hole: bool,
    out: &mut impl FnMut(DecodedEvent),
) {
    // Gap ⇒ decoder already observed a non-snapshot; decide from the gap's ts (the
    // observing record's ts) before applying the keep filter.
    if ev.event.kind == EventKind::Gap {
        if admission.admit_snapshots.is_none() {
            admission.decide(ev.event.ts, bucketer);
            admission.flush_pending(out, stats);
        }
        if keep_live_or_gap(&ev, admission.instrument_id, admission.trade_date, bucketer) {
            stats.gaps_kept += 1;
            out(ev);
        }
        return;
    }

    if is_snapshot(&ev) {
        if ev.instrument_id != admission.instrument_id {
            return;
        }
        match admission.admit_snapshots {
            None => admission.pending_snapshots.push(ev),
            Some(true) => {
                stats.snapshots_kept += 1;
                out(ev);
            }
            Some(false) => {
                stats.snapshots_dropped += 1;
            }
        }
        return;
    }

    // Forward hole accounting uses observing-ts trade-date (channel-wide), matching
    // prior gaps_kept scoping — not the instrument keep filter.
    if forward_hole && bucketer.bucket(ev.event.ts) == admission.trade_date {
        stats.seq_holes_ignored += 1;
    }

    // First non-snapshot live record in this file establishes snapshot admission.
    if admission.admit_snapshots.is_none() {
        admission.decide(ev.event.ts, bucketer);
        admission.flush_pending(out, stats);
    }
    if keep_live_or_gap(&ev, admission.instrument_id, admission.trade_date, bucketer) {
        out(ev);
    }
}
