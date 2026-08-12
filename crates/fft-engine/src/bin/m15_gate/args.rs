//! CLI argument parsing for the M1.5 sim-live gate.

use std::env;
use std::path::PathBuf;
use std::process::exit;

pub struct Args {
    pub replay: PathBuf,
    pub head_ts: u64,
    pub live_out: PathBuf,
    pub gate_secs: u64,
    pub out: PathBuf,
}

pub fn parse_args() -> Args {
    let mut replay = None;
    let mut head = None;
    let mut live_out = None;
    let mut gate_secs = None;
    let mut out = None;
    let mut args = env::args().skip(1);
    while let Some(a) = args.next() {
        match a.as_str() {
            "--replay" => {
                replay = Some(PathBuf::from(
                    args.next()
                        .unwrap_or_else(|| usage("missing --replay path")),
                ));
            }
            "--head" => {
                head = Some(args.next().unwrap_or_else(|| usage("missing --head value")));
            }
            "--live-out" => {
                live_out = Some(PathBuf::from(
                    args.next()
                        .unwrap_or_else(|| usage("missing --live-out path")),
                ));
            }
            "--gate-secs" => {
                gate_secs = Some(
                    args.next()
                        .unwrap_or_else(|| usage("missing --gate-secs value"))
                        .parse::<u64>()
                        .unwrap_or_else(|_| usage("--gate-secs must be u64")),
                );
            }
            "--out" => {
                out = Some(PathBuf::from(
                    args.next().unwrap_or_else(|| usage("missing --out path")),
                ));
            }
            "--help" | "-h" => usage("M1.5 sim-live evidence gate"),
            other => usage(&format!("unknown arg {other}")),
        }
    }
    let head = head.unwrap_or_else(|| usage("missing --head"));
    let gate_secs = gate_secs.unwrap_or_else(|| usage("missing --gate-secs"));
    if gate_secs == 0 {
        usage("--gate-secs must be greater than zero");
    }
    let replay = replay.unwrap_or_else(|| usage("missing --replay"));
    let live_out = live_out.unwrap_or_else(|| usage("missing --live-out"));
    let out = out.unwrap_or_else(|| usage("missing --out"));
    if replay == live_out || replay == out || live_out == out {
        usage("--replay, --live-out, and --out must be distinct paths");
    }
    Args {
        replay,
        head_ts: parse_head(&head),
        live_out,
        gate_secs,
        out,
    }
}

fn usage(msg: &str) -> ! {
    eprintln!(
        "m15-gate: {msg}\n\
         usage: m15-gate --replay <log> --head <ts|RFC3339Z> --live-out <path> \
                --gate-secs <n> --out <json>"
    );
    exit(2)
}

fn parse_head(raw: &str) -> u64 {
    if let Ok(ns) = raw.parse::<u64>() {
        return ns;
    }
    if raw.len() == 20
        && raw.as_bytes()[4] == b'-'
        && raw.as_bytes()[7] == b'-'
        && raw.as_bytes()[10] == b'T'
        && raw.as_bytes()[13] == b':'
        && raw.as_bytes()[16] == b':'
        && raw.ends_with('Z')
    {
        let y: i64 = raw[0..4]
            .parse()
            .unwrap_or_else(|_| usage("bad --head year"));
        let mo: u32 = raw[5..7]
            .parse()
            .unwrap_or_else(|_| usage("bad --head month"));
        let d: u32 = raw[8..10]
            .parse()
            .unwrap_or_else(|_| usage("bad --head day"));
        let hh: u64 = raw[11..13]
            .parse()
            .unwrap_or_else(|_| usage("bad --head hour"));
        let mm: u64 = raw[14..16]
            .parse()
            .unwrap_or_else(|_| usage("bad --head minute"));
        let ss: u64 = raw[17..19]
            .parse()
            .unwrap_or_else(|_| usage("bad --head second"));
        if !(1..=12).contains(&mo)
            || d == 0
            || d > days_in_month(y, mo)
            || hh > 23
            || mm > 59
            || ss > 59
        {
            usage("bad --head calendar/time value");
        }
        let days = days_from_civil(y, mo, d);
        if days < 0 {
            usage("--head must not predate Unix epoch");
        }
        return (days as u64 * 86_400 + hh * 3600 + mm * 60 + ss) * 1_000_000_000;
    }
    usage("bad --head (want u64 ns or YYYY-MM-DDTHH:MM:SSZ)")
}

fn days_in_month(year: i64, month: u32) -> u32 {
    match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 if year % 4 == 0 && (year % 100 != 0 || year % 400 == 0) => 29,
        2 => 28,
        _ => 0,
    }
}

fn days_from_civil(y: i64, m: u32, d: u32) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = (y - era * 400) as u64;
    let mp = if m > 2 { m - 3 } else { m + 9 };
    let doy = (153 * mp as u64 + 2) / 5 + u64::from(d) - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    (era * 146_097 + doe as i64) - 719_468
}
