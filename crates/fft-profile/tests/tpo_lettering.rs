//! TPO lettering against the fixture-week Wed 2026-07-29 session.
//!
//! PROFILE-WAVE (2026-08-10): RTH letters stop at 15:00 CT (A..M); admission
//! extends to 17:00 CT; ETH lattice still ends at 16:00 CT (46 periods).

mod common;

use common::*;
use fft_core::{Price, Side, Ts};
use fft_profile::{
    ETH_PERIOD_COUNT, PERIOD_NS, RTH_PERIOD_COUNT, SessionClock, SessionProfile, period_letter,
    tpo_letters,
};

#[test]
fn clock_anchors_match_hand_computed_utc() {
    let clock = SessionClock::for_trade_date(TRADE_DATE);
    assert_eq!(clock.session_open(), Ts(SESSION_OPEN_NS));
    assert_eq!(clock.rth_open(), Ts(RTH_OPEN_NS));
    assert_eq!(clock.rth_close(), Ts(RTH_CLOSE_NS));
    assert_eq!(clock.eth_end(), Ts(ETH_END_NS));
    assert_eq!(clock.session_end(), Ts(SESSION_END_NS));
    // 17:00 CT → 08:30 CT is 15.5 h = 31 half-hour periods.
    assert_eq!(clock.rth_offset_periods(), 31);
    // ETH lettering: 23 h = 46 periods (open → 16:00).
    assert_eq!(clock.period_count(), ETH_PERIOD_COUNT);
    assert_eq!(RTH_PERIOD_COUNT, 13);
}

#[test]
fn period_rollover_exactly_at_half_hours() {
    let clock = SessionClock::for_trade_date(TRADE_DATE);
    assert_eq!(clock.eth_period(Ts(SESSION_OPEN_NS)), 0);
    assert_eq!(clock.eth_period(Ts(SESSION_OPEN_NS + PERIOD_NS - 1)), 0);
    assert_eq!(clock.eth_period(Ts(SESSION_OPEN_NS + PERIOD_NS)), 1);
    // 08:29:59.999999999 CT is still ETH-only; 08:30:00 CT opens RTH A.
    assert_eq!(clock.rth_period(Ts(RTH_OPEN_NS - 1)), None);
    assert_eq!(clock.eth_period(Ts(RTH_OPEN_NS - 1)), 30);
    assert_eq!(clock.rth_period(Ts(RTH_OPEN_NS)), Some(0));
    assert_eq!(clock.eth_period(Ts(RTH_OPEN_NS)), 31);
    assert_eq!(clock.rth_period(Ts(RTH_OPEN_NS + PERIOD_NS)), Some(1));
    // Last nanosecond of ETH lettering is the last period.
    assert_eq!(clock.eth_period(Ts(ETH_END_NS - 1)), 45);
}

/// PROFILE-WAVE boundary table: RTH M ends at 14:59:59.999…; 15:00:00 has no RTH letter.
#[test]
fn rth_lettering_stops_at_1500_ct() {
    let clock = SessionClock::for_trade_date(TRADE_DATE);
    // 14:59:59.999… CT = last ns of RTH M (period 12).
    assert_eq!(clock.rth_period(Ts(RTH_CLOSE_NS - 1)), Some(12)); // M
    assert_eq!(period_letter(12), 'M');
    // 15:00:00.000 CT: no RTH letter (still ETH lettering).
    assert_eq!(clock.rth_period(Ts(RTH_CLOSE_NS)), None);
    assert!(clock.eth_period(Ts(RTH_CLOSE_NS)) < ETH_PERIOD_COUNT);
    // 15:15–15:30 halt window: still no RTH letter; ETH continues.
    let halt = RTH_CLOSE_NS + PERIOD_NS / 2; // 15:15
    assert_eq!(clock.rth_period(Ts(halt)), None);
    let _ = clock.eth_period(Ts(halt)); // must not panic
}

#[test]
#[should_panic(expected = "outside ETH lettering")]
fn pre_open_timestamp_panics() {
    // PROFILE-WAVE: eth_period panic message names ETH lettering (not full admission).
    SessionClock::for_trade_date(TRADE_DATE).eth_period(Ts(SESSION_OPEN_NS - 1));
}

#[test]
fn dual_lettering_eth_continuous_rth_restarts() {
    let mut p = SessionProfile::new(SessionClock::for_trade_date(TRADE_DATE), Price(TICK));
    // ETH A and B at tick 20_000.
    p.apply(&trade(20_000, 1, Side::Bid, SESSION_OPEN_NS));
    p.apply(&trade(20_000, 1, Side::Bid, SESSION_OPEN_NS + PERIOD_NS));
    // RTH A (= ETH period 31) at 20_000, RTH B at 20_001.
    p.apply(&trade(20_000, 1, Side::Bid, RTH_OPEN_NS));
    p.apply(&trade(20_001, 1, Side::Bid, RTH_OPEN_NS + PERIOD_NS));

    let r = p.row(price(20_000));
    // ETH stream marks every trade: bits 0, 1, and 31 (period at 08:30 CT).
    assert_eq!(r.eth_periods, 0b11 | (1 << 31));
    // RTH stream restarts at A.
    assert_eq!(r.rth_periods, 0b01);
    assert_eq!(r.tpo_count, 3);
    assert!(!r.single_print);

    let r1 = p.row(price(20_001));
    assert_eq!(r1.eth_periods, 1 << 32);
    assert_eq!(r1.rth_periods, 0b10);
    assert_eq!(r1.tpo_count, 1);
    assert!(r1.single_print);

    assert_eq!(p.current_eth_period(), 32);
    assert_eq!(p.current_rth_period(), Some(1));
}

#[test]
fn current_rth_period_is_none_before_rth() {
    let mut p = SessionProfile::new(SessionClock::for_trade_date(TRADE_DATE), Price(TICK));
    p.apply(&trade(20_000, 1, Side::Bid, SESSION_OPEN_NS));
    assert_eq!(p.current_rth_period(), None);
}

/// PROFILE-WAVE: post-ETH [16:00, 17:00) accrues volume, no TPO/PV, loud counter.
#[test]
fn post_close_volume_only_no_letters() {
    let mut p = SessionProfile::new(SessionClock::for_trade_date(TRADE_DATE), Price(TICK));
    p.apply(&trade(20_000, 5, Side::Bid, SESSION_OPEN_NS));
    assert_eq!(p.post_close_events(), 0);

    p.apply(&trade(20_001, 3, Side::Ask, ETH_END_NS)); // 16:00 CT
    p.apply(&trade(20_001, 2, Side::Bid, ETH_END_NS + 1));
    assert_eq!(p.post_close_events(), 2);
    assert_eq!(p.total_volume(), 10);
    let r = p.row(price(20_001));
    assert_eq!(r.volume, 5);
    assert_eq!(r.eth_periods, 0, "no TPO lettering post-close");
    assert_eq!(r.rth_periods, 0);
    assert_eq!(r.period_volume, 0, "no PV attribution post-close");
    // Developing period cursor unchanged by post-close events.
    assert_eq!(p.current_eth_period(), 0);
}

#[test]
fn letter_helpers() {
    assert_eq!(period_letter(0), 'A');
    assert_eq!(period_letter(25), 'Z');
    assert_eq!(period_letter(26), 'a');
    assert_eq!(period_letter(51), 'z');
    assert_eq!(period_letter(52), '?');
    assert_eq!(tpo_letters(0b1011, 8), "ABD");
    assert_eq!(tpo_letters(0b1111, 2), "AB…");
}
