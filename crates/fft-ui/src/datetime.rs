//! Civil-date helpers for gate timestamps and `--replay-at` parsing.
//!
//! Hinnant's `civil_from_days` / `days_from_civil` pair (inverse of each other). Kept
//! dependency-free: gate evidence and CLI parsing must not pull in a calendar crate.

/// Days since the Unix epoch to `(year, month, day)` (Hinnant's `civil_from_days`).
pub(crate) fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let month = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    let year = yoe + era * 400 + i64::from(month <= 2);
    (year, month, day)
}

/// `(year, month, day)` to days since the Unix epoch (Hinnant's `days_from_civil`).
pub fn days_from_civil(year: i64, month: u32, day: u32) -> i64 {
    let y = year - i64::from(month <= 2);
    let era = y.div_euclid(400);
    let yoe = (y - era * 400) as u32;
    let mp = if month > 2 { month - 3 } else { month + 9 };
    let doy = (153 * mp + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + i64::from(doe) - 719_468
}

/// Parse `--replay-at <ts>`: all-digits ⇒ ns UTC; `YYYY-MM-DDTHH:MM:SSZ` ⇒ ns UTC.
pub fn parse_replay_at(raw: &str) -> Result<u64, String> {
    if raw.is_empty() {
        return Err("empty --replay-at value".to_string());
    }
    if raw.bytes().all(|b| b.is_ascii_digit()) {
        return raw
            .parse::<u64>()
            .map_err(|_| format!("invalid --replay-at nanoseconds: {raw}"));
    }
    parse_rfc3339_utc_seconds(raw).map(|secs| secs.saturating_mul(1_000_000_000))
}

fn parse_rfc3339_utc_seconds(raw: &str) -> Result<u64, String> {
    // Exact shape: YYYY-MM-DDTHH:MM:SSZ (20 chars, trailing Z required).
    if raw.len() != 20 || !raw.is_ascii() || !raw.ends_with('Z') {
        return Err(format!(
            "invalid --replay-at timestamp (want YYYY-MM-DDTHH:MM:SSZ or ns): {raw}"
        ));
    }
    let bytes = raw.as_bytes();
    if bytes[4] != b'-'
        || bytes[7] != b'-'
        || bytes[10] != b'T'
        || bytes[13] != b':'
        || bytes[16] != b':'
    {
        return Err(format!(
            "invalid --replay-at timestamp (want YYYY-MM-DDTHH:MM:SSZ or ns): {raw}"
        ));
    }
    let year = parse_fixed_u32(&raw[0..4], "year", raw)?;
    let month = parse_fixed_u32(&raw[5..7], "month", raw)?;
    let day = parse_fixed_u32(&raw[8..10], "day", raw)?;
    let hour = parse_fixed_u32(&raw[11..13], "hour", raw)?;
    let minute = parse_fixed_u32(&raw[14..16], "minute", raw)?;
    let second = parse_fixed_u32(&raw[17..19], "second", raw)?;

    if !(1..=12).contains(&month) {
        return Err(format!("--replay-at month out of range: {raw}"));
    }
    let max_day = days_in_month(year, month);
    if day < 1 || day > max_day {
        return Err(format!("--replay-at day out of range: {raw}"));
    }
    if hour > 23 {
        return Err(format!("--replay-at hour out of range: {raw}"));
    }
    if minute > 59 {
        return Err(format!("--replay-at minute out of range: {raw}"));
    }
    if second > 59 {
        return Err(format!("--replay-at second out of range: {raw}"));
    }

    let days = days_from_civil(i64::from(year), month, day);
    if days < 0 {
        return Err(format!("--replay-at predates the Unix epoch: {raw}"));
    }
    let secs = u64::try_from(days)
        .unwrap_or_else(|_| panic!("fft: --replay-at day count overflow: {raw}"))
        .checked_mul(86_400)
        .and_then(|day_secs| {
            day_secs
                .checked_add(u64::from(hour) * 3600 + u64::from(minute) * 60 + u64::from(second))
        })
        .unwrap_or_else(|| panic!("fft: --replay-at second count overflow: {raw}"));
    Ok(secs)
}

fn parse_fixed_u32(field: &str, name: &str, raw: &str) -> Result<u32, String> {
    if !field.bytes().all(|b| b.is_ascii_digit()) {
        return Err(format!("--replay-at {name} is not digits: {raw}"));
    }
    field
        .parse::<u32>()
        .map_err(|_| format!("--replay-at {name} is not digits: {raw}"))
}

fn days_in_month(year: u32, month: u32) -> u32 {
    match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 => {
            if is_leap_year(year) {
                29
            } else {
                28
            }
        }
        _ => 0,
    }
}

fn is_leap_year(year: u32) -> bool {
    (year.is_multiple_of(4) && !year.is_multiple_of(100)) || year.is_multiple_of(400)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn days_from_civil_round_trips_civil_from_days() {
        for days in [
            0_i64, 1, 31, 59, 60, 365, 366, 10_000, 20_000, 40_000, // 2026-07-29
            20_663, // leap day 2024-02-29
            19_782, // 2000-03-01
            11_017,
        ] {
            let (y, m, d) = civil_from_days(days);
            assert_eq!(
                days_from_civil(y, m, d),
                days,
                "round-trip failed for days={days} → {y}-{m:02}-{d:02}"
            );
        }
    }

    #[test]
    fn days_from_civil_pins_known_instants() {
        // 2026-07-29T13:50:00Z = 1785333000 s (PRD §6 sim-live anchor in UTC).
        assert_eq!(days_from_civil(2026, 7, 29), 20_663);
        assert_eq!(
            u64::try_from(days_from_civil(2026, 7, 29)).unwrap() * 86_400 + 13 * 3600 + 50 * 60,
            1_785_333_000
        );
        assert_eq!(days_from_civil(1970, 1, 1), 0);
        assert_eq!(days_from_civil(2000, 3, 1), 11_017);
    }

    #[test]
    fn parse_replay_at_accepts_ns_and_rfc3339() {
        assert_eq!(
            parse_replay_at("1785333000000000000").unwrap(),
            1_785_333_000_000_000_000
        );
        assert_eq!(
            parse_replay_at("2026-07-29T13:50:00Z").unwrap(),
            1_785_333_000_000_000_000
        );
        assert_eq!(parse_replay_at("0").unwrap(), 0);
        assert_eq!(parse_replay_at("1970-01-01T00:00:00Z").unwrap(), 0);
    }

    #[test]
    fn parse_replay_at_rejects_bad_shapes_and_ranges() {
        assert!(parse_replay_at("2026-07-29T13:50:00").is_err()); // missing Z
        assert!(parse_replay_at("2026-07-29 13:50:00Z").is_err());
        assert!(parse_replay_at("2026-13-01T00:00:00Z").is_err()); // bad month
        assert!(parse_replay_at("2026-02-30T00:00:00Z").is_err()); // bad day
        assert!(parse_replay_at("2026-07-29T24:00:00Z").is_err()); // bad hour
        assert!(parse_replay_at("2026-07-29T13:60:00Z").is_err()); // bad minute
        assert!(parse_replay_at("2026-07-29T13:50:60Z").is_err()); // bad second
        assert!(parse_replay_at("not-a-timestamp").is_err());
        assert!(parse_replay_at("12ab").is_err());
        assert!(parse_replay_at("").is_err());
    }
}
