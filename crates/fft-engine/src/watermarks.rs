/// Sequence integrity accounting owned and advanced by the engine thread.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Watermarks {
    /// Last source sequence accepted from the source.
    pub received_seq: u64,
    /// Last source sequence decoded to canonical form.
    pub decoded_seq: u64,
    /// Last source sequence applied to all derived state.
    pub applied_seq: u64,
    /// Last sequence durably present in the replay log.
    pub logged_seq: u64,
    /// Last sequence reflected by a published snapshot.
    pub published_seq: u64,
    /// A Gap record passed; the next sequenced event re-anchors absolutely.
    /// Gap is a first-class re-anchor point (`docs/FFTLOG-V2.md` §4) — the
    /// post-gap channel seq may legally be below the pre-gap watermark, exactly
    /// as the book re-anchors `last_seq` in `Book::do_gap`.
    gap_reanchor: bool,
}

impl Watermarks {
    pub(crate) fn apply_forward(&mut self, seq: u64) {
        if seq == 0 {
            return;
        }
        if self.gap_reanchor {
            self.set_applied(seq);
            return;
        }
        assert!(
            seq >= self.applied_seq,
            "engine applied sequence regressed: {seq} < {}",
            self.applied_seq
        );
        self.set_applied(seq);
    }

    /// The stream announced a source gap: sequence accounting re-anchors on the
    /// next sequenced event instead of asserting forward monotonicity across it.
    pub(crate) fn gap(&mut self) {
        self.gap_reanchor = true;
    }

    /// Absolute watermark set used by seeks (time travel may move seq backward).
    pub(crate) fn set_applied(&mut self, seq: u64) {
        self.received_seq = seq;
        self.decoded_seq = seq;
        self.applied_seq = seq;
        self.logged_seq = seq;
        self.gap_reanchor = false;
    }

    pub(crate) fn publish(&mut self) {
        self.published_seq = self.applied_seq;
        assert!(self.published_seq <= self.applied_seq);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gap_permits_one_backward_reanchor_then_enforces_monotonicity() {
        let mut marks = Watermarks::default();
        marks.apply_forward(1_000);
        assert_eq!(marks.applied_seq, 1_000);
        marks.gap();
        // Post-gap seq below the pre-gap watermark is legal exactly once.
        marks.apply_forward(640);
        assert_eq!(marks.applied_seq, 640);
        // The re-anchor is consumed: a second regression is a real defect.
        let regressed = std::panic::catch_unwind(move || marks.apply_forward(600));
        assert!(regressed.is_err());
    }

    #[test]
    fn seq_zero_between_gap_and_reanchor_keeps_the_pending_reanchor() {
        let mut marks = Watermarks::default();
        marks.apply_forward(1_000);
        marks.gap();
        // The Gap record itself carries seq 0 — it must not consume the re-anchor.
        marks.apply_forward(0);
        marks.apply_forward(500);
        assert_eq!(marks.applied_seq, 500);
    }

    #[test]
    fn seek_set_applied_clears_a_pending_reanchor() {
        let mut marks = Watermarks::default();
        marks.apply_forward(1_000);
        marks.gap();
        marks.set_applied(2_000);
        let regressed = std::panic::catch_unwind(move || marks.apply_forward(1_500));
        assert!(regressed.is_err());
    }
}
