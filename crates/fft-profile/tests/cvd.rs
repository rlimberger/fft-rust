//! CVD candles and cB/cA touch counters against scripted tapes, plus gap
//! honesty-marker semantics.

mod common;

use common::*;
use fft_core::{Price, Side};
use fft_profile::{CvdCandle, PERIOD_NS, SessionClock, SessionProfile};

fn empty_profile() -> SessionProfile {
    SessionProfile::new(SessionClock::for_trade_date(TRADE_DATE), Price(TICK))
}

#[test]
fn cvd_candles_track_delta_per_period() {
    let mut p = empty_profile();
    // Period A: +10 (buy), −3 (sell) → delta path 10 → 7.
    p.apply(&trade(20_000, 10, Side::Bid, SESSION_OPEN_NS));
    p.apply(&trade(19_999, 3, Side::Ask, SESSION_OPEN_NS + 1));
    // Period C (skipping B): +1 → 8. B fills flat at 7.
    p.apply(&trade(
        20_000,
        1,
        Side::Bid,
        SESSION_OPEN_NS + 2 * PERIOD_NS,
    ));

    let cvd = p.cvd();
    assert_eq!(cvd.delta(), 8);
    assert_eq!(cvd.buy_volume(), 11);
    assert_eq!(cvd.sell_volume(), 3);
    assert_eq!(cvd.range(), (0, 10));
    let candles = cvd.candles();
    assert_eq!(candles.len(), 3);
    assert_eq!(
        candles[0],
        CvdCandle {
            open: 0,
            high: 10,
            low: 0,
            close: 7
        }
    );
    assert_eq!(
        candles[1],
        CvdCandle::flat(7),
        "untraded period carries flat"
    );
    assert_eq!(
        candles[2],
        CvdCandle {
            open: 7,
            high: 8,
            low: 7,
            close: 8
        }
    );
    assert!(candles[2].is_up());
    assert_eq!(candles[2].period_delta(), 1);
}

#[test]
fn cb_ca_reset_on_price_change() {
    let mut p = empty_profile();
    let t0 = SESSION_OPEN_NS;
    // Sells hit the bid at 20_000: cB accumulates.
    p.apply(&trade(20_000, 5, Side::Ask, t0));
    p.apply(&trade(20_000, 3, Side::Ask, t0 + 1));
    assert_eq!(p.cvd().current_bid().at(price(20_000)), 8);
    // Bid price moves down → cB resets to the new touch.
    p.apply(&trade(19_999, 2, Side::Ask, t0 + 2));
    assert_eq!(p.cvd().current_bid().at(price(20_000)), 0);
    assert_eq!(p.cvd().current_bid().at(price(19_999)), 2);
    // Buys lift the offer at 20_001: cA accumulates independently.
    p.apply(&trade(20_001, 4, Side::Bid, t0 + 3));
    p.apply(&trade(20_001, 1, Side::Bid, t0 + 4));
    assert_eq!(p.cvd().current_ask().at(price(20_001)), 5);
    assert_eq!(
        p.cvd().current_bid().at(price(19_999)),
        2,
        "cB untouched by buys"
    );
    // Offer lifts to 20_002 → cA resets.
    p.apply(&trade(20_002, 7, Side::Bid, t0 + 5));
    assert_eq!(p.cvd().current_ask().at(price(20_001)), 0);
    assert_eq!(p.cvd().current_ask().at(price(20_002)), 7);
}

#[test]
fn gap_marks_in_flight_counters_and_keeps_accumulated_state() {
    let mut p = empty_profile();
    p.apply(&trade(20_000, 5, Side::Ask, SESSION_OPEN_NS));
    p.apply(&trade(20_001, 4, Side::Bid, SESSION_OPEN_NS + 1));
    assert!(!p.period_gap());

    p.apply(&gap(SESSION_OPEN_NS + 2));

    // Queryable gap state on everything in flight.
    assert!(p.period_gap(), "developing-period PV is gap-marked");
    assert!(p.cvd().current_bid().is_gap());
    assert!(p.cvd().current_ask().is_gap());
    // Accumulated volume/TPO stays.
    assert_eq!(p.row(price(20_000)).volume, 5);
    assert_eq!(p.row(price(20_000)).period_volume, 5);
    assert_eq!(p.total_volume(), 9);
    // Counter values survive under the marker.
    assert_eq!(p.cvd().current_bid().at(price(20_000)), 5);

    // Same-price trade keeps the marker (count still incomplete)...
    p.apply(&trade(20_000, 2, Side::Ask, SESSION_OPEN_NS + 3));
    assert!(p.cvd().current_bid().is_gap());
    assert_eq!(p.cvd().current_bid().at(price(20_000)), 7);
    // ...a price change starts a fresh, trustworthy counter.
    p.apply(&trade(19_999, 1, Side::Ask, SESSION_OPEN_NS + 4));
    assert!(!p.cvd().current_bid().is_gap());
    assert_eq!(p.cvd().current_bid().at(price(19_999)), 1);
    // cA never reset → still marked.
    assert!(p.cvd().current_ask().is_gap());

    // Period roll clears the PV gap marker.
    p.apply(&trade(20_000, 1, Side::Bid, SESSION_OPEN_NS + PERIOD_NS));
    assert!(!p.period_gap());
}

#[test]
fn sideless_trade_counts_volume_but_no_delta_or_touch() {
    let mut p = empty_profile();
    p.apply(&trade(20_000, 6, Side::None, SESSION_OPEN_NS));
    assert_eq!(p.total_volume(), 6);
    assert_eq!(p.row(price(20_000)).volume, 6);
    assert_eq!(p.row(price(20_000)).buy_volume, 0);
    assert_eq!(p.row(price(20_000)).sell_volume, 0);
    assert_eq!(p.session_delta(), 0);
    assert!(p.cvd().candles().is_empty());
    assert_eq!(p.cvd().current_bid().price(), None);
}
