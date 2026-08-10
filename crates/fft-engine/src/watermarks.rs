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
}

impl Watermarks {
    pub(crate) fn apply_forward(&mut self, seq: u64) {
        if seq == 0 {
            return;
        }
        assert!(
            seq >= self.applied_seq,
            "engine applied sequence regressed: {seq} < {}",
            self.applied_seq
        );
        self.set_applied(seq);
    }

    /// Absolute watermark set used by seeks (time travel may move seq backward).
    pub(crate) fn set_applied(&mut self, seq: u64) {
        self.received_seq = seq;
        self.decoded_seq = seq;
        self.applied_seq = seq;
        self.logged_seq = seq;
    }

    pub(crate) fn publish(&mut self) {
        self.published_seq = self.applied_seq;
        assert!(self.published_seq <= self.applied_seq);
    }
}
