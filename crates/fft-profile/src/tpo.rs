//! TPO period clock and lettering.
//!
//! Period boundaries derive from jiff-resolved `America/Chicago` wall-clock
//! anchors (Globex open 17:00 CT prior calendar day, RTH open 08:30 CT,
//! session end 16:00 CT) — never hand-rolled UTC offsets. CME DST transitions
//! happen Sunday 02:00 CT, inside the weekend trading halt, so a session never
//! straddles one; the constructor asserts the half-hour grid alignment anyway.

use fft_core::Ts;
use jiff::Span;
use jiff::civil::Date;
use jiff::tz::TimeZone;

/// TPO period length: 30 minutes in nanoseconds.
pub const PERIOD_NS: u64 = 1_800_000_000_000;

/// Session time model for one CT trade date: Globex open, RTH open, session
/// end, and the 30-minute period indices anchored on them.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct SessionClock {
    trade_date: u32,
    session_open: Ts,
    rth_open: Ts,
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
        let session_end = ts_of(date, 16, 0);
        assert!(
            (rth_open.0 - session_open.0) % PERIOD_NS == 0
                && (session_end.0 - session_open.0) % PERIOD_NS == 0,
            "session anchors for trade date {trade_date} are off the half-hour grid"
        );
        SessionClock {
            trade_date,
            session_open,
            rth_open,
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

    /// Session end: 16:00 CT of the trade date.
    pub fn session_end(&self) -> Ts {
        self.session_end
    }

    /// ETH-anchored period index (letter A = 0) from the Globex open. Runs
    /// continuously across the whole session, including RTH. Panics on a
    /// timestamp outside `[session_open, session_end)` — a mis-bucketed event.
    pub fn eth_period(&self, ts: Ts) -> u32 {
        assert!(
            ts >= self.session_open && ts < self.session_end,
            "ts {ts:?} outside session for trade date {}",
            self.trade_date
        );
        u32::try_from((ts.0 - self.session_open.0) / PERIOD_NS).expect("period index fits u32")
    }

    /// RTH-anchored period index (letter A = 0 at 08:30 CT); `None` before RTH.
    pub fn rth_period(&self, ts: Ts) -> Option<u32> {
        if ts < self.rth_open {
            return None;
        }
        assert!(
            ts < self.session_end,
            "ts {ts:?} outside session for trade date {}",
            self.trade_date
        );
        Some(u32::try_from((ts.0 - self.rth_open.0) / PERIOD_NS).expect("period index fits u32"))
    }

    /// Number of ETH periods before the RTH open (the ETH index of RTH letter A).
    pub fn rth_offset_periods(&self) -> u32 {
        u32::try_from((self.rth_open.0 - self.session_open.0) / PERIOD_NS)
            .expect("period offset fits u32")
    }

    /// Total 30-minute periods in the session.
    pub fn period_count(&self) -> u32 {
        u32::try_from((self.session_end.0 - self.session_open.0) / PERIOD_NS)
            .expect("period count fits u32")
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
