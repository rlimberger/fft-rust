//! Dalton 70% volume value area, expanded from the VPOC.
//!
//! Ported from the legacy engine (proven over the 82 M-event fixture week):
//! compare the next *two* rows above against the next *two* rows below and
//! take the heavier pair, adding both of its rows — including the classic
//! quirk that the second row of a chosen pair is added even when the first
//! alone reaches the target. Computed on demand from the dense volume array;
//! unlike legacy there is no debounced cached copy, so no VA state exists to
//! checkpoint or drift.

/// Value-area volume target: 70%, rounded up.
pub(crate) const VA_PERCENT: u64 = 70;

/// Expand from `poc_i` until the enclosed volume reaches `ceil(70%)` of
/// `total`. `lo_i..=hi_i` bound the traded range inside `volume`. Returns
/// `(low_index, high_index)` of the value area.
pub(crate) fn compute(
    volume: &[u64],
    lo_i: usize,
    hi_i: usize,
    poc_i: usize,
    total: u64,
) -> (usize, usize) {
    debug_assert!(lo_i <= poc_i && poc_i <= hi_i && hi_i < volume.len());
    let target = (total * VA_PERCENT).div_ceil(100);
    let mut acc = volume[poc_i];
    let mut lo = poc_i;
    let mut hi = poc_i;

    while acc < target {
        let up_n = (hi_i - hi).min(2);
        let dn_n = (lo - lo_i).min(2);
        if up_n == 0 && dn_n == 0 {
            break;
        }
        let up: u64 = (1..=up_n).map(|k| volume[hi + k]).sum();
        let dn: u64 = (1..=dn_n).map(|k| volume[lo - k]).sum();
        let take_up = if up_n == 0 {
            false
        } else if dn_n == 0 {
            true
        } else {
            up >= dn
        };
        if take_up {
            for _ in 0..up_n {
                hi += 1;
                acc += volume[hi];
            }
        } else {
            for _ in 0..dn_n {
                lo -= 1;
                acc += volume[lo];
            }
        }
    }
    (lo, hi)
}
