//! Checkpoint discipline: serialize → restore → serialize byte-identity,
//! equality of every queryable stat, and loud rejection of bad sections.

mod common;

use common::*;
use fft_core::{Price, Side};
use fft_profile::{
    CVD_SECTION_VERSION, MultiProfile, PERIOD_NS, PROFILE_SECTION_VERSION, RestoreError,
    SessionClock,
};

/// Two-session week slice: Mon 2026-07-27 (frozen) + Wed 2026-07-29
/// (developing, with an in-flight gap marker).
fn fixture() -> MultiProfile {
    let mut p = MultiProfile::new(Price(TICK));

    let monday = 20_661;
    p.begin_session(monday);
    let mon_open = SessionClock::for_trade_date(monday).session_open().0;
    let mon_rth = SessionClock::for_trade_date(monday).rth_open().0;
    p.apply(&trade(20_000, 10, Side::Bid, mon_open));
    p.apply(&trade(19_999, 4, Side::Ask, mon_open + 1));
    p.apply(&trade(20_001, 2, Side::Bid, mon_rth));
    p.apply(&trade(20_002, 3, Side::Ask, mon_rth + PERIOD_NS));

    p.begin_session(TRADE_DATE);
    p.apply(&trade(20_010, 7, Side::Ask, SESSION_OPEN_NS));
    p.apply(&trade(20_012, 5, Side::Bid, SESSION_OPEN_NS + 1));
    p.apply(&trade(20_012, 1, Side::None, RTH_OPEN_NS));
    // Gap lands in the developing RTH period: markers must survive restore.
    p.apply(&gap(RTH_OPEN_NS + 1));
    p
}

#[test]
fn serialize_restore_serialize_is_byte_identical() {
    let original = fixture();
    let (profile1, cvd1) = original.serialize();
    let restored = MultiProfile::restore(
        PROFILE_SECTION_VERSION,
        &profile1,
        CVD_SECTION_VERSION,
        &cvd1,
    )
    .expect("restore");
    let (profile2, cvd2) = restored.serialize();
    assert_eq!(
        profile1, profile2,
        "PROFILE section must round-trip byte-identically"
    );
    assert_eq!(cvd1, cvd2, "CVD section must round-trip byte-identically");
    // Complete-state equality, not just byte equality.
    assert_eq!(original, restored);
}

#[test]
fn every_queryable_stat_survives_restore() {
    let original = fixture();
    let (pb, cb) = original.serialize();
    let restored =
        MultiProfile::restore(PROFILE_SECTION_VERSION, &pb, CVD_SECTION_VERSION, &cb).unwrap();

    assert_eq!(restored.tick(), original.tick());
    assert_eq!(restored.sessions().len(), original.sessions().len());
    for (a, b) in original.sessions().iter().zip(restored.sessions()) {
        assert_eq!(a.trade_date(), b.trade_date());
        assert_eq!(a.vpoc(), b.vpoc());
        assert_eq!(a.value_area(), b.value_area());
        assert_eq!(a.initial_balance(), b.initial_balance());
        assert_eq!(a.range(), b.range());
        assert_eq!(a.open_price(), b.open_price());
        assert_eq!(a.total_volume(), b.total_volume());
        assert_eq!(a.session_delta(), b.session_delta());
        assert_eq!(a.current_eth_period(), b.current_eth_period());
        assert_eq!(a.current_rth_period(), b.current_rth_period());
        assert_eq!(a.period_gap(), b.period_gap());
        assert_eq!(a.cvd(), b.cvd());
        let (lo, hi) = a.range().expect("fixture sessions trade");
        let mut t = lo.0;
        while t <= hi.0 {
            assert_eq!(a.row(Price(t)), b.row(Price(t)));
            t += TICK;
        }
    }
    // The developing gap markers made the trip.
    let cur = restored.current().unwrap();
    assert!(cur.period_gap());
    assert!(cur.cvd().current_bid().is_gap());
}

#[test]
fn empty_and_untraded_sessions_round_trip() {
    let mut p = MultiProfile::new(Price(TICK));
    let (pb, cb) = p.serialize();
    let r = MultiProfile::restore(PROFILE_SECTION_VERSION, &pb, CVD_SECTION_VERSION, &cb).unwrap();
    assert_eq!(p, r);

    p.begin_session(TRADE_DATE);
    p.apply(&gap(SESSION_OPEN_NS)); // gap before any trade
    let (pb, cb) = p.serialize();
    let r = MultiProfile::restore(PROFILE_SECTION_VERSION, &pb, CVD_SECTION_VERSION, &cb).unwrap();
    assert_eq!(p, r);
    assert!(r.current().unwrap().period_gap());
}

#[test]
fn unknown_versions_are_rejected_loudly() {
    let (pb, cb) = fixture().serialize();
    assert_eq!(
        MultiProfile::restore(2, &pb, CVD_SECTION_VERSION, &cb),
        Err(RestoreError::UnsupportedVersion {
            section: "PROFILE",
            version: 2
        })
    );
    assert_eq!(
        MultiProfile::restore(PROFILE_SECTION_VERSION, &pb, 9, &cb),
        Err(RestoreError::UnsupportedVersion {
            section: "CVD",
            version: 9
        })
    );
}

#[test]
fn truncated_and_oversized_sections_are_rejected_loudly() {
    let (pb, cb) = fixture().serialize();

    let truncated = &pb[..pb.len() - 1];
    assert_eq!(
        MultiProfile::restore(PROFILE_SECTION_VERSION, truncated, CVD_SECTION_VERSION, &cb),
        Err(RestoreError::Truncated { section: "PROFILE" })
    );

    let mut trailing = cb.clone();
    trailing.push(0);
    assert_eq!(
        MultiProfile::restore(PROFILE_SECTION_VERSION, &pb, CVD_SECTION_VERSION, &trailing),
        Err(RestoreError::Corrupt {
            section: "CVD",
            what: "trailing bytes"
        })
    );
}
