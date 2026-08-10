//! TPO period clock and lettering.
//!
//! Period boundaries derive from jiff-resolved `America/Chicago` wall-clock
//! anchors (Globex open 17:00 CT prior calendar day, RTH 08:30–15:00 CT,
//! ETH lettering end 16:00 CT, Globex admission end 17:00 CT) — never hand-rolled
//! UTC offsets. CME DST transitions happen Sunday 02:00 CT, inside the weekend
//! trading halt, so a session never straddles one; the constructor asserts the
//! half-hour grid alignment anyway.
//!
//! PROFILE-WAVE (2026-08-10): RTH letters stop at 15:00 CT (A..M = 13 periods);
//! admission extends to 17:00 CT while ETH lettering remains the 46 periods
//! ending 16:00 CT.

use fft_core::Ts;
use jiff::Span;
use jiff::civil::Date;
use jiff::tz::TimeZone;

/// TPO period length: 30 minutes in nanoseconds.
pub const PERIOD_NS: u64 = 1_800_000_000_000;

/// RTH lettering periods: 08:30–15:00 CT = 13 half-hours (letters A..M).
pub const RTH_PERIOD_COUNT: u32 = 13;

/// ETH lettering periods: Globex open → 16:00 CT = 46 half-hours.
pub const ETH_PERIOD_COUNT: u32 = 46;

/// Session time model for one CT trade date: Globex open, RTH window, ETH
/// lettering end, and Globex admission end.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct SessionClock {
    trade_date: u32,
    session_open: Ts,
    rth_open: Ts,
    rth_close: Ts,
    /// ETH lettering end (16:00 CT). Not the Globex admission end.
    eth_end: Ts,
    /// Globex session admission end: 17:00 CT of the trade date.
    session_end: Ts,
}

impl SessionClock {
    /// Build the clock for a CT trade date (days since Unix epoch, matching
    /// [`fft_core::InstrumentMeta::trade_date`]). Panics on an unresolvable
    /// date or a session that is off the half-hour grid — both are bugs.
    pub fn for_trade_date(trade_date: u32) -> SessionClock {
        let tz = TimeZone::get("America/Chicago").expect("tzdb must contain America/Chicago");
        let date = jiff::civil::date(1970, 1, 1)
            .checked_add(Span::new().days(i64::from(trade_date)))
            .expect("trade date out of civil range");
        let prior = date
            .checked_sub(Span::new().days(1))
            .expect("trade date out of civil range");
        let ts_of = |d: Date, hour: i8, minute: i8| -> Ts {
            let zoned = d
                .at(hour, minute, 0, 0)
                .to_zoned(tz.clone())
                .expect("civil session anchor must resolve in America/Chicago");
            Ts(u64::try_from(zoned.timestamp().as_nanosecond())
                .expect("session anchor before Unix epoch"))
        };
        let session_open = ts_of(prior, 17, 0);
        let rth_open = ts_of(date, 8, 30);
        let rth_close = ts_of(date, 15, 0);
        let eth_end = ts_of(date, 16, 0);
        let session_end = ts_of(date, 17, 0);
        assert!(
            (rth_open.0 - session_open.0) % PERIOD_NS == 0
                && (rth_close.0 - session_open.0) % PERIOD_NS == 0
                && (eth_end.0 - session_open.0) % PERIOD_NS == 0
                && (session_end.0 - session_open.0) % PERIOD_NS == 0,
            "session anchors for trade date {trade_date} are off the half-hour grid"
        );
        assert_eq!(
            (eth_end.0 - session_open.0) / PERIOD_NS,
            u64::from(ETH_PERIOD_COUNT),
            "ETH lattice must be exactly {ETH_PERIOD_COUNT} periods"
        );
        assert_eq!(
            (rth_close.0 - rth_open.0) / PERIOD_NS,
            u64::from(RTH_PERIOD_COUNT),
            "RTH lettering must be exactly {RTH_PERIOD_COUNT} periods (A..M)"
        );
        SessionClock {
            trade_date,
            session_open,
            rth_open,
            rth_close,
            eth_end,
            session_end,
        }
    }

    pub fn trade_date(&self) -> u32 {
        self.trade_date
    }

    /// Globex open: 17:00 CT of the prior calendar day.
    pub fn session_open(&self) -> Ts {
        self.session_open
    }

    /// RTH open: 08:30 CT of the trade date.
    pub fn rth_open(&self) -> Ts {
        self.rth_open
    }

    /// RTH lettering close: 15:00 CT of the trade date (PRD §5 / PROFILE-WAVE).
    pub fn rth_close(&self) -> Ts {
        self.rth_close
    }

    /// ETH lettering end: 16:00 CT of the trade date (46-period lattice).
    pub fn eth_end(&self) -> Ts {
        self.eth_end
    }

    /// Globex admission end: 17:00 CT of the trade date. Events in
    /// `[eth_end, session_end)` are volume-only (no TPO/PV).
    pub fn session_end(&self) -> Ts {
        self.session_end
    }

    /// ETH-anchored period index (letter A = 0) from the Globex open. Runs
    /// continuously through 16:00 CT. Panics on a timestamp outside
    /// `[session_open, eth_end)`.
    pub fn eth_period(&self, ts: Ts) -> u32 {
        assert!(
            ts >= self.session_open && ts < self.eth_end,
            "ts {ts:?} outside ETH lettering for trade date {}",
            self.trade_date
        );
        u32::try_from((ts.0 - self.session_open.0) / PERIOD_NS).expect("period index fits u32")
    }

    /// RTH-anchored period index (letter A = 0 at 08:30 CT); `None` before
    /// 08:30 or at/after 15:00 CT (PROFILE-WAVE: RTH letters A..M only).
    pub fn rth_period(&self, ts: Ts) -> Option<u32> {
        if ts < self.rth_open || ts >= self.rth_close {
            return None;
        }
        Some(u32::try_from((ts.0 - self.rth_open.0) / PERIOD_NS).expect("period index fits u32"))
    }

    /// Number of ETH periods before the RTH open (the ETH index of RTH letter A).
    pub fn rth_offset_periods(&self) -> u32 {
        u32::try_from((self.rth_open.0 - self.session_open.0) / PERIOD_NS)
            .expect("period offset fits u32")
    }

    /// Total 30-minute ETH lettering periods in the session (always 46).
    pub fn period_count(&self) -> u32 {
        ETH_PERIOD_COUNT
    }

    /// True when `ts` is inside Globex admission `[session_open, session_end)`.
    pub fn admits(&self, ts: Ts) -> bool {
        ts >= self.session_open && ts < self.session_end
    }

    /// True when `ts` is in the post-ETH volume-only window `[eth_end, session_end)`.
    pub fn is_post_close(&self, ts: Ts) -> bool {
        ts >= self.eth_end && ts < self.session_end
    }
}

/// TPO period letter: 0→A … 25→Z, 26→a … 51→z, else '?'.
pub fn period_letter(period: u32) -> char {
    if period < 26 {
        char::from(b'A' + u8::try_from(period).expect("checked"))
    } else if period < 52 {
        char::from(b'a' + u8::try_from(period - 26).expect("checked"))
    } else {
        '?'
    }
}

/// Compact TPO letters for a period bitset (bit 0 = letter A); truncates with
/// `…` past `max_chars`.
pub fn tpo_letters(periods: u64, max_chars: usize) -> String {
    let mut out = String::new();
    let mut count = 0usize;
    let mut bits = periods;
    while bits != 0 && count < max_chars {
        let p = bits.trailing_zeros();
        bits &= bits - 1;
        out.push(period_letter(p));
        count += 1;
    }
    if bits != 0 {
        out.push('…');
    }
    out
}
