//! Shared tzdb probe + paint-time clock formatting (kept separate so transport stays under ~500 lines).

use std::sync::Once;
use std::sync::atomic::{AtomicBool, Ordering};

use jiff::Timestamp;

pub(crate) const NY: &str = "America/New_York";
pub(crate) const CT: &str = "America/Chicago";

/// Probe NY + CT once. Missing tzdb is a loud startup failure (doctrine §7).
pub fn ensure_tzdb_available() {
    static ONCE: Once = Once::new();
    ONCE.call_once(|| {
        let now = Timestamp::now();
        now.in_tz(NY).unwrap_or_else(|err| {
            panic!("fft-ui: tz database missing {NY} (required at startup): {err}")
        });
        now.in_tz(CT).unwrap_or_else(|err| {
            panic!("fft-ui: tz database missing {CT} (required at startup): {err}")
        });
    });
}

pub(crate) fn warn_once(flag: &AtomicBool, message: String) {
    if flag
        .compare_exchange(false, true, Ordering::Relaxed, Ordering::Relaxed)
        .is_ok()
    {
        eprintln!("{message}");
    }
}

/// Paint-time clock: out-of-range / zone failures ⇒ `--:--:--` + once-per-cause warn.
pub fn format_zone_clock_ns(ts_ns: i128, zone: &'static str) -> String {
    ensure_tzdb_available();
    if ts_ns == 0 {
        return "--:--:--".to_string();
    }
    static WARNED_RANGE: AtomicBool = AtomicBool::new(false);
    static WARNED_NY: AtomicBool = AtomicBool::new(false);
    static WARNED_CT: AtomicBool = AtomicBool::new(false);
    let Ok(ts) = Timestamp::from_nanosecond(ts_ns) else {
        warn_once(
            &WARNED_RANGE,
            format!("fft-ui: WARNING applied_ts {ts_ns} outside jiff range; clock shows --:--:--"),
        );
        return "--:--:--".to_string();
    };
    let Ok(zoned) = ts.in_tz(zone) else {
        // Startup probe should have aborted already; keep paint alive if tzdb vanishes later.
        let flag = if zone == NY { &WARNED_NY } else { &WARNED_CT };
        warn_once(
            flag,
            format!("fft-ui: WARNING cannot zone applied_ts into {zone}; clock shows --:--:--"),
        );
        return "--:--:--".to_string();
    };
    let time = zoned.time();
    format!(
        "{:02}:{:02}:{:02}",
        time.hour(),
        time.minute(),
        time.second()
    )
}
