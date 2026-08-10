//! America/Chicago trade-date bucketing (doctrine rule 10). Databento timestamps are
//! UTC-only; a CME trade date runs from Globex open at 17:00 CT of the **prior**
//! calendar day to 17:00 CT of the trade date itself, so one trade date always spans
//! two UTC dates. All zone math goes through jiff's tz database — never hand-rolled
//! offsets (the legacy attempt's hand-rolled NY DST math is a documented killer).

use fft_core::Ts;
use jiff::Timestamp;
use jiff::civil::{Date, Time, date, time};

const CT: &str = "America/Chicago";

/// Globex session open, civil America/Chicago wall time.
pub const GLOBEX_OPEN_CT: Time = time(17, 0, 0, 0);

/// The America/Chicago trade date owning a UTC event timestamp: events at or after
/// 17:00 CT belong to the **next** calendar date's session.
pub fn trade_date(ts: Ts) -> Date {
    let zoned = Timestamp::from_nanosecond(i128::from(ts.0))
        .unwrap_or_else(|err| panic!("fft-ingest: ts {} outside jiff range: {err}", ts.0))
        .in_tz(CT)
        .unwrap_or_else(|err| panic!("fft-ingest: tz database missing {CT}: {err}"));
    if zoned.time() >= GLOBEX_OPEN_CT {
        zoned
            .date()
            .tomorrow()
            .unwrap_or_else(|err| panic!("fft-ingest: no day after {}: {err}", zoned.date()))
    } else {
        zoned.date()
    }
}

/// Globex session open for a trade date: 17:00 CT of the prior calendar day, as UTC ns.
pub fn session_open(trade_date: Date) -> Ts {
    let prior = trade_date
        .yesterday()
        .unwrap_or_else(|err| panic!("fft-ingest: no day before {trade_date}: {err}"));
    let zoned = prior
        .at(17, 0, 0, 0)
        .in_tz(CT)
        .unwrap_or_else(|err| panic!("fft-ingest: cannot zone {prior} 17:00 {CT}: {err}"));
    let ns = zoned.timestamp().as_nanosecond();
    Ts(u64::try_from(ns)
        .unwrap_or_else(|_| panic!("fft-ingest: session open {zoned} precedes Unix epoch")))
}

/// Days since Unix epoch of a trade date (the `InstrumentMeta::trade_date` encoding).
pub fn trade_date_days(trade_date: Date) -> u32 {
    let days = trade_date
        .since(date(1970, 1, 1))
        .unwrap_or_else(|err| panic!("fft-ingest: {trade_date} - epoch failed: {err}"))
        .get_days();
    u32::try_from(days)
        .unwrap_or_else(|_| panic!("fft-ingest: trade date {trade_date} precedes Unix epoch"))
}

/// Streaming trade-date assignment: caches the current session's UTC window so the hot
/// path is two comparisons, with full jiff zone math only at session boundaries.
#[derive(Default)]
pub struct TradeDateBucketer {
    window: Option<(u64, u64, Date)>,
}

impl TradeDateBucketer {
    pub fn bucket(&mut self, ts: Ts) -> Date {
        if let Some((lo, hi, d)) = self.window
            && ts.0 >= lo
            && ts.0 < hi
        {
            return d;
        }
        let d = trade_date(ts);
        let next = d
            .tomorrow()
            .unwrap_or_else(|err| panic!("fft-ingest: no day after {d}: {err}"));
        self.window = Some((session_open(d).0, session_open(next).0, d));
        d
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // 2026-07-28 is 20,662 days after the epoch; July CT is CDT (UTC-5), so
    // 17:00 CT = 22:00 UTC. Constants are hand-derived, not produced by jiff,
    // so these tests are an independent check of the zone math.
    const TUE_1700_CT_UTC_NS: u64 = (20_662 * 86_400 + 22 * 3_600) * 1_000_000_000;

    #[test]
    fn globex_open_boundary_starts_next_trade_date() {
        assert_eq!(trade_date(Ts(TUE_1700_CT_UTC_NS - 1)), date(2026, 7, 28));
        assert_eq!(trade_date(Ts(TUE_1700_CT_UTC_NS)), date(2026, 7, 29));
    }

    #[test]
    fn sunday_open_buckets_to_monday() {
        // Sunday 2026-07-26 17:00 CDT = 22:00 UTC, day 20,660.
        let sunday_open = (20_660 * 86_400 + 22 * 3_600) * 1_000_000_000;
        assert_eq!(trade_date(Ts(sunday_open)), date(2026, 7, 27));
        assert_eq!(trade_date(Ts(sunday_open - 1)), date(2026, 7, 26));
    }

    #[test]
    fn winter_boundary_uses_cst() {
        // 2026-01-15 is day 20,468; January CT is CST (UTC-6), so 17:00 CT = 23:00 UTC.
        // Hand-rolled UTC-5 would misbucket this by an hour.
        let thu_1700_cst = (20_468 * 86_400 + 23 * 3_600) * 1_000_000_000;
        assert_eq!(trade_date(Ts(thu_1700_cst - 1)), date(2026, 1, 15));
        assert_eq!(trade_date(Ts(thu_1700_cst)), date(2026, 1, 16));
    }

    #[test]
    fn session_open_is_prior_day_1700_ct() {
        assert_eq!(session_open(date(2026, 7, 29)), Ts(TUE_1700_CT_UTC_NS));
        // Monday's session opens Sunday evening.
        let sunday_open = (20_660 * 86_400 + 22 * 3_600) * 1_000_000_000;
        assert_eq!(session_open(date(2026, 7, 27)), Ts(sunday_open));
    }

    #[test]
    fn bucketer_matches_trade_date_across_boundary() {
        let mut b = TradeDateBucketer::default();
        for ts in [
            TUE_1700_CT_UTC_NS - 2,
            TUE_1700_CT_UTC_NS - 1,
            TUE_1700_CT_UTC_NS,
            TUE_1700_CT_UTC_NS + 1,
            TUE_1700_CT_UTC_NS + 86_400_000_000_000,
        ] {
            assert_eq!(b.bucket(Ts(ts)), trade_date(Ts(ts)), "ts {ts}");
        }
    }

    #[test]
    fn epoch_days_round_trip() {
        assert_eq!(trade_date_days(date(2026, 7, 29)), 20_663);
        assert_eq!(trade_date_days(date(1970, 1, 1)), 0);
    }
}
