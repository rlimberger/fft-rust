//! Checkpoint discipline: serialize → restore → serialize byte-identity,
//! equality of every queryable stat, and loud rejection of bad sections.
//! PROFILE-WAVE: three-section PROFILE/CVD/SESSION payloads.

mod common;

use common::*;
use fft_core::{Price, Side};
use fft_profile::{MultiProfile, PERIOD_NS, RestoreError, SessionClock};

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
    let s1 = original.serialize();
    let restored = MultiProfile::restore(&s1.profile, &s1.cvd, &s1.session).expect("restore");
    let s2 = restored.serialize();
    assert_eq!(
        s1.profile, s2.profile,
        "PROFILE section must round-trip byte-identically"
    );
    assert_eq!(
        s1.cvd, s2.cvd,
        "CVD section must round-trip byte-identically"
    );
    assert_eq!(
        s1.session, s2.session,
        "SESSION section must round-trip byte-identically"
    );
    assert_eq!(original, restored);
}

#[test]
fn every_queryable_stat_survives_restore() {
    let original = fixture();
    let s = original.serialize();
    let restored = MultiProfile::restore(&s.profile, &s.cvd, &s.session).unwrap();

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
        assert_eq!(a.post_close_events(), b.post_close_events());
        assert_eq!(a.backward_ts_events(), b.backward_ts_events());
        assert_eq!(a.cvd(), b.cvd());
        assert_eq!(a.clock(), b.clock());
        let (lo, hi) = a.range().expect("fixture sessions trade");
        let mut t = lo.0;
        while t <= hi.0 {
            assert_eq!(a.row(Price(t)), b.row(Price(t)));
            t += TICK;
        }
    }
    let cur = restored.current().unwrap();
    assert!(cur.period_gap());
    assert!(cur.cvd().current_bid().is_gap());
}

#[test]
fn empty_and_untraded_sessions_round_trip() {
    let mut p = MultiProfile::new(Price(TICK));
    let s = p.serialize();
    let r = MultiProfile::restore(&s.profile, &s.cvd, &s.session).unwrap();
    assert_eq!(p, r);

    p.begin_session(TRADE_DATE);
    p.apply(&gap(SESSION_OPEN_NS)); // gap before any trade
    let s = p.serialize();
    let r = MultiProfile::restore(&s.profile, &s.cvd, &s.session).unwrap();
    assert_eq!(p, r);
    assert!(r.current().unwrap().period_gap());
}

#[test]
fn unknown_versions_are_rejected_loudly() {
    let s = fixture().serialize();
    let mut bad_profile = s.profile.clone();
    bad_profile[..2].copy_from_slice(&2u16.to_le_bytes());
    assert_eq!(
        MultiProfile::restore(&bad_profile, &s.cvd, &s.session),
        Err(RestoreError::UnsupportedVersion {
            section: "PROFILE",
            version: 2
        })
    );
    let mut bad_cvd = s.cvd.clone();
    bad_cvd[..2].copy_from_slice(&9u16.to_le_bytes());
    assert_eq!(
        MultiProfile::restore(&s.profile, &bad_cvd, &s.session),
        Err(RestoreError::UnsupportedVersion {
            section: "CVD",
            version: 9
        })
    );
    let mut bad_session = s.session.clone();
    bad_session[..2].copy_from_slice(&3u16.to_le_bytes());
    assert_eq!(
        MultiProfile::restore(&s.profile, &s.cvd, &bad_session),
        Err(RestoreError::UnsupportedVersion {
            section: "SESSION",
            version: 3
        })
    );
}

#[test]
fn truncated_and_oversized_sections_are_rejected_loudly() {
    let s = fixture().serialize();

    let truncated = &s.profile[..s.profile.len() - 1];
    assert_eq!(
        MultiProfile::restore(truncated, &s.cvd, &s.session),
        Err(RestoreError::Truncated { section: "PROFILE" })
    );

    let mut trailing = s.cvd.clone();
    trailing.push(0);
    assert_eq!(
        MultiProfile::restore(&s.profile, &trailing, &s.session),
        Err(RestoreError::Corrupt {
            section: "CVD",
            what: "trailing bytes"
        })
    );
}

/// PROFILE-WAVE: single-byte corruption of SESSION is loud.
#[test]
fn session_single_byte_corruption_is_loud() {
    let s = fixture().serialize();
    let mut corrupt = s.session.clone();
    let flip = corrupt.len() / 2;
    corrupt[flip] ^= 0xff;
    let err = MultiProfile::restore(&s.profile, &s.cvd, &corrupt).unwrap_err();
    assert!(
        matches!(
            err,
            RestoreError::Corrupt {
                section: "SESSION",
                ..
            } | RestoreError::Truncated { section: "SESSION" }
        ),
        "got {err}"
    );
}
