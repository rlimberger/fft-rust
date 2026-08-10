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

    // PROFILE-WAVE: period roll on a Trade still clears the PV gap marker when the
    // gap landed in the *previous* period (cursor advances then clears).
    p.apply(&trade(20_000, 1, Side::Bid, SESSION_OPEN_NS + PERIOD_NS));
    assert!(!p.period_gap());
}

/// PROFILE-WAVE §3: a Gap whose ts enters a new period rolls first, then marks
/// the NEW period — the marker must not be lost on the next same-period trade.
#[test]
fn gap_rolls_into_new_period_before_marking() {
    let mut p = empty_profile();
    p.apply(&trade(20_000, 1, Side::Bid, SESSION_OPEN_NS));
    assert_eq!(p.current_eth_period(), 0);
    assert!(!p.period_gap());

    // Gap stamped at the start of period 1: cursor advances, then marks period 1.
    p.apply(&gap(SESSION_OPEN_NS + PERIOD_NS));
    assert_eq!(p.current_eth_period(), 1);
    assert!(
        p.period_gap(),
        "gap must mark the NEW period after the roll"
    );

    // Same-period trade must not clear the marker.
    p.apply(&trade(
        20_000,
        1,
        Side::Bid,
        SESSION_OPEN_NS + PERIOD_NS + 1,
    ));
    assert_eq!(p.current_eth_period(), 1);
    assert!(p.period_gap(), "same-period trade keeps the gap marker");
}

/// PROFILE-WAVE §4: cross-period-backward ts attributes to the current period.
#[test]
fn backward_ts_attributes_to_current_period() {
    let mut p = empty_profile();
    p.apply(&trade(20_000, 1, Side::Bid, SESSION_OPEN_NS + PERIOD_NS)); // period 1
    assert_eq!(p.current_eth_period(), 1);
    assert_eq!(p.backward_ts_events(), 0);

    // Trade stamped in period 0 while cursor is at 1.
    p.apply(&trade(20_001, 2, Side::Ask, SESSION_OPEN_NS));
    assert_eq!(p.current_eth_period(), 1, "cursor does not rewind");
    assert_eq!(p.backward_ts_events(), 1);
    // Attribution: volume on 20_001 under ETH period 1 bit, not period 0.
    let r = p.row(price(20_001));
    assert_eq!(r.eth_periods, 1 << 1);
    assert_eq!(r.volume, 2);
}

/// PROFILE-WAVE §5: snapshot-flagged non-Add panics; snapshot Add is ignored.
#[test]
#[should_panic(expected = "snapshot-flagged event must be Add")]
fn snapshot_non_add_panics() {
    let mut p = empty_profile();
    let mut ev = trade(20_000, 1, Side::Bid, SESSION_OPEN_NS);
    ev.flags = 1 << 5;
    p.apply(&ev);
}

#[test]
fn snapshot_add_is_ignored_by_profile() {
    let mut p = empty_profile();
    let mut ev = trade(20_000, 1, Side::Bid, SESSION_OPEN_NS);
    // Force kind Add + SNAPSHOT so book would load it; profile must ignore.
    ev.kind = fft_core::EventKind::Add;
    ev.flags = 1 << 5;
    p.apply(&ev);
    assert_eq!(p.total_volume(), 0);
    assert_eq!(p.row(price(20_000)).volume, 0);
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

#[test]
fn snapshot_clear_is_tolerated_and_ignored() {
    let mut p = empty_profile();
    p.apply(&trade(20_000, 3, Side::Bid, SESSION_OPEN_NS));
    let mut clear = trade(20_000, 1, Side::Bid, SESSION_OPEN_NS + 1);
    clear.kind = fft_core::EventKind::Clear;
    clear.size = 0;
    clear.flags = 1 << 5;
    p.apply(&clear);
    assert_eq!(p.total_volume(), 3);
}
