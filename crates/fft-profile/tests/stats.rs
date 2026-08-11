//! Derived stats against hand-computed distributions: VA/VAH/VAL, VPOC, IB,
//! session range/open, PV reset, SV spectrum.

mod common;

use common::*;
use fft_core::{Price, Side};
use fft_profile::{PERIOD_NS, SessionClock, SessionProfile};

fn empty_profile() -> SessionProfile {
    SessionProfile::new(SessionClock::for_trade_date(TRADE_DATE), Price(TICK))
}

#[test]
fn value_area_and_vpoc_hand_computed() {
    // Distribution (ticks → volume): 19_998:1, 19_999:2, 20_000:10, 20_001:2,
    // 20_002:1. Total 16, VA target = ceil(16 × 70%) = 12.
    // Expansion from VPOC 20_000 (acc 10): pair above (20_001+20_002 = 3) vs
    // pair below (19_999+19_998 = 3) — tie takes the upper pair, adding both
    // rows → acc 13 ≥ 12. VA = [20_000, 20_002].
    let mut p = empty_profile();
    let tape = [
        (20_000, 10),
        (20_001, 2),
        (19_999, 2),
        (20_002, 1),
        (19_998, 1),
    ];
    for (i, (tick, vol)) in tape.into_iter().enumerate() {
        p.apply(&trade(tick, vol, Side::Bid, SESSION_OPEN_NS + i as u64));
    }
    assert_eq!(p.total_volume(), 16);
    assert_eq!(p.vpoc(), Some(price(20_000)));
    let (val, vah) = p.value_area().expect("traded session has a VA");
    assert_eq!(val, price(20_000));
    assert_eq!(vah, price(20_002));
}

#[test]
fn value_area_expands_down_when_lower_pair_heavier() {
    // 19_998:5, 19_999:4, 20_000:10, 20_001:1. Total 20, target = 14.
    // From VPOC (acc 10): upper pair = 1, lower pair = 9 → take lower, acc 19.
    // VA = [19_998, 20_000].
    let mut p = empty_profile();
    let tape = [(20_000, 10), (19_999, 4), (19_998, 5), (20_001, 1)];
    for (i, (tick, vol)) in tape.into_iter().enumerate() {
        p.apply(&trade(tick, vol, Side::Bid, SESSION_OPEN_NS + i as u64));
    }
    let (val, vah) = p.value_area().expect("va");
    assert_eq!(val, price(19_998));
    assert_eq!(vah, price(20_000));
}

#[test]
fn initial_balance_is_first_two_rth_periods() {
    let mut p = empty_profile();
    // ETH trade: in range, not in IB.
    p.apply(&trade(19_990, 1, Side::Bid, SESSION_OPEN_NS));
    // RTH A, B, C.
    p.apply(&trade(20_000, 1, Side::Bid, RTH_OPEN_NS));
    p.apply(&trade(20_005, 1, Side::Bid, RTH_OPEN_NS + PERIOD_NS));
    p.apply(&trade(20_010, 1, Side::Bid, RTH_OPEN_NS + 2 * PERIOD_NS));
    assert_eq!(
        p.initial_balance(),
        Some((price(20_000), price(20_005))),
        "IB spans RTH A+B only"
    );
    assert_eq!(p.range(), Some((price(19_990), price(20_010))));
    assert_eq!(p.open_price(), Some(price(19_990)));
}

#[test]
fn initial_balance_absent_before_rth() {
    let mut p = empty_profile();
    p.apply(&trade(20_000, 1, Side::Bid, SESSION_OPEN_NS));
    assert_eq!(p.initial_balance(), None);
}

#[test]
fn period_volume_resets_on_roll_session_volume_accumulates() {
    let mut p = empty_profile();
    p.apply(&trade(20_000, 7, Side::Bid, SESSION_OPEN_NS));
    assert_eq!(p.row(price(20_000)).period_volume, 7);
    p.apply(&trade(20_000, 3, Side::Bid, SESSION_OPEN_NS + PERIOD_NS));
    let r = p.row(price(20_000));
    assert_eq!(r.period_volume, 3, "PV resets at the period roll");
    assert_eq!(r.volume, 10, "SV keeps accumulating");
    assert_eq!(r.tpo_count, 2);
    assert_eq!(r.eth_periods, 0b11);
}

#[test]
fn spectrum_splits_aggressor_sides() {
    let mut p = empty_profile();
    p.apply(&trade(20_000, 10, Side::Bid, SESSION_OPEN_NS));
    p.apply(&trade(19_999, 4, Side::Ask, SESSION_OPEN_NS + 1));
    p.apply(&trade(20_000, 5, Side::Bid, SESSION_OPEN_NS + 2));
    let r = p.row(price(20_000));
    assert_eq!(r.volume, 15);
    assert_eq!(r.buy_volume, 15);
    assert_eq!(r.sell_volume, 0);
    assert!(r.is_poc);
    let r99 = p.row(price(19_999));
    assert_eq!(r99.sell_volume, 4);
    assert!(!r99.is_poc);
    assert_eq!(p.session_delta(), 11);
    // Untraded price reads the zero row.
    assert_eq!(p.row(price(21_000)), fft_profile::ProfileRow::default());
}

#[test]
fn zero_size_trade_counts_loudly_without_volume() {
    let mut p = empty_profile();
    p.apply(&trade(20_000, 5, Side::Bid, SESSION_OPEN_NS));
    let before_vol = p.total_volume();
    let before_delta = p.session_delta();
    let before_row = p.row(price(20_000));
    let before_period = p.current_eth_period();
    assert_eq!(p.zero_size_trades(), 0);

    // Same-period zero-size Trade: time advances via apply/advance_period; no volume.
    p.apply(&trade(20_001, 0, Side::Ask, SESSION_OPEN_NS + 1));
    assert_eq!(p.zero_size_trades(), 1);
    assert_eq!(p.total_volume(), before_vol);
    assert_eq!(p.session_delta(), before_delta);
    assert_eq!(p.row(price(20_000)), before_row);
    assert_eq!(
        p.row(price(20_001)),
        fft_profile::ProfileRow::default(),
        "zero-size must not open a TPO/volume row"
    );
    assert_eq!(p.current_eth_period(), before_period);

    // Period-boundary zero-size still rolls the developing period.
    p.apply(&trade(20_002, 0, Side::Bid, SESSION_OPEN_NS + PERIOD_NS));
    assert_eq!(p.zero_size_trades(), 2);
    assert_eq!(
        p.current_eth_period(),
        1,
        "zero-size at boundary rolls period"
    );
    assert_eq!(p.total_volume(), before_vol);
    assert_eq!(p.row(price(20_000)).period_volume, 0, "PV cleared on roll");
    assert_eq!(p.row(price(20_000)).volume, 5, "SV unchanged");
    assert_eq!(p.row(price(20_002)), fft_profile::ProfileRow::default());

    // Sized trade after zero-size still accumulates normally.
    p.apply(&trade(
        20_000,
        3,
        Side::Bid,
        SESSION_OPEN_NS + PERIOD_NS + 1,
    ));
    assert_eq!(p.zero_size_trades(), 2);
    assert_eq!(p.total_volume(), before_vol + 3);
    assert_eq!(p.row(price(20_000)).period_volume, 3);
    assert_eq!(p.row(price(20_000)).volume, 8);
    assert_eq!(p.row(price(20_000)).eth_periods, 0b11);
}

#[test]
fn zero_size_trades_excluded_from_state_equality() {
    // Same-period zero-size Trade moves only the runtime counter; state equality
    // must ignore it so restore+tail ≡ forward still holds without SESSION v1.
    let mut a = empty_profile();
    let mut b = empty_profile();
    a.apply(&trade(20_000, 5, Side::Bid, SESSION_OPEN_NS));
    b.apply(&trade(20_000, 5, Side::Bid, SESSION_OPEN_NS));
    assert_eq!(a, b);
    assert_eq!(a.zero_size_trades(), 0);

    a.apply(&trade(20_001, 0, Side::Ask, SESSION_OPEN_NS + 1));
    assert_eq!(a.zero_size_trades(), 1);
    assert_eq!(b.zero_size_trades(), 0);
    assert_eq!(
        a, b,
        "runtime-only zero_size_trades must not participate in PartialEq"
    );
    assert_eq!(a.current_eth_period(), b.current_eth_period());
    assert_eq!(a.total_volume(), b.total_volume());
}

#[test]
fn zero_size_trade_post_close_counts_without_volume() {
    let mut p = empty_profile();
    p.apply(&trade(20_000, 4, Side::Bid, SESSION_OPEN_NS));
    let before = p.total_volume();
    p.apply(&trade(20_000, 0, Side::Ask, ETH_END_NS));
    assert_eq!(p.post_close_events(), 1);
    assert_eq!(p.zero_size_trades(), 1);
    assert_eq!(p.total_volume(), before);
    assert_eq!(p.row(price(20_000)).volume, 4);
}

#[test]
#[should_panic(expected = "off the")]
fn off_grid_price_is_a_loud_bug() {
    let mut p = empty_profile();
    let mut ev = trade(20_000, 1, Side::Bid, SESSION_OPEN_NS);
    ev.price = Price(ev.price.0 + 1);
    p.apply(&ev);
}
