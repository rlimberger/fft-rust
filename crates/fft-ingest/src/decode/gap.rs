//! Channel-sequence accounting per DBN channel (`docs/FFTLOG-V2.md` §4 batch gap policy).

use fft_core::{CanonicalEvent, Ts};

/// Channel-sequence accounting per DBN channel (`docs/FFTLOG-V2.md` §4 batch gap policy).
///
/// Batch files are symbol-filtered, so **forward** holes are expected filtering artifacts:
/// counted on [`seq_holes_ignored`](Self::seq_holes_ignored) and otherwise ignored — no
/// Gap record, no gap-state poison. A **regression** (`observed < expected`) is a genuine
/// anomaly and synthesizes a canonical Gap. Same-packet repeats (`seq == last`) and the
/// immediate successor are continuous.
///
/// Records with `sequence == 0` carry no venue sequence and are excluded from accounting
/// entirely. Measured on the sample week: Databento's synthetic week-open `Clear` records
/// all carry `sequence` 0 while the first sequenced record starts in the hundreds, so
/// seeding the baseline from 0 would fabricate a discontinuity at every week open.
#[derive(Default)]
pub struct GapDetector {
    last: Vec<(u8, u32)>,
    seq_holes_ignored: u64,
}

impl GapDetector {
    /// Observe a record's channel/sequence; returns a Gap only on sequence regression.
    pub fn observe(&mut self, channel_id: u8, seq: u32, ts: Ts) -> Option<CanonicalEvent> {
        if seq == 0 {
            return None;
        }
        match self.last.iter_mut().find(|(ch, _)| *ch == channel_id) {
            None => {
                self.last.push((channel_id, seq));
                None
            }
            Some((_, last)) => {
                let expected = u64::from(*last) + 1;
                let observed = u64::from(seq);
                if seq == *last || observed == expected {
                    *last = seq;
                    return None;
                }
                *last = seq;
                if observed < expected {
                    Some(CanonicalEvent::gap(ts, expected, observed))
                } else {
                    // Forward jump: filter artifact. Count loudly; do not synthesize Gap.
                    self.seq_holes_ignored += 1;
                    None
                }
            }
        }
    }

    /// Forward channel-seq holes ignored under the batch gap policy (one per discontinuity).
    pub fn seq_holes_ignored(&self) -> u64 {
        self.seq_holes_ignored
    }

    /// Channels seen so far.
    pub fn channels(&self) -> impl Iterator<Item = u8> {
        self.last.iter().map(|(ch, _)| *ch)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fft_core::EventKind;

    #[test]
    fn gap_synthesis() {
        let mut det = GapDetector::default();
        let ts = Ts(1);
        assert_eq!(det.observe(0, 0, ts), None); // seq 0 = unsequenced: never a baseline
        assert_eq!(det.observe(0, 100, ts), None); // first sequenced record: no baseline
        assert_eq!(det.observe(0, 100, ts), None); // same packet
        assert_eq!(det.observe(0, 101, ts), None); // successor
        // Forward jump: counted, no Gap.
        assert_eq!(det.observe(0, 107, ts), None);
        assert_eq!(det.seq_holes_ignored(), 1);
        // Regression (observed < expected) synthesizes a Gap.
        let gap = det
            .observe(0, 3, ts)
            .expect("regression must synthesize a gap");
        assert_eq!(gap.kind, EventKind::Gap);
        assert_eq!(gap.gap_seqs(), (108, 3));
        assert_eq!(det.seq_holes_ignored(), 1); // regressions do not increment holes
        // Channels are tracked independently.
        assert_eq!(det.observe(1, 500, ts), None);
        assert_eq!(det.observe(0, 4, ts), None);
    }
}
