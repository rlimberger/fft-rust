//! PRD §4 claim 4 — headless native-refresh census over a full-day fftlog.
//!
//! ```text
//! claim4-census <path.fftlog> [--out <evidence.json>]
//! ```
//! Default `--out` is stdout. Fail loudly on I/O or invariant breach.

mod census;
mod report;

use census::{count_events, run};
use fft_book::REFRESH_WINDOW_NS;
use report::{Evidence, die, git_info, utc_date_string, write_out};
use std::path::PathBuf;
use std::process::exit;
use std::time::Instant;

fn usage(msg: &str) -> ! {
    eprintln!(
        "claim4-census: {msg}\n\
         usage: claim4-census <path.fftlog> [--out <evidence.json>]"
    );
    exit(2);
}

fn main() {
    let (log_path, out_path) = parse_args();
    let (git_sha, git_dirty) = git_info();
    let date = utc_date_string();
    eprintln!(
        "claim4-census: log={} git={git_sha} dirty={git_dirty:?}",
        log_path.display()
    );

    let expected = count_events(&log_path).unwrap_or_else(|e| die(&format!("count: {e}")));
    eprintln!("claim4-census: expected_events={expected}");

    let t0 = Instant::now();
    let r = run(&log_path).unwrap_or_else(|e| die(&format!("replay: {e}")));
    let wall_s = t0.elapsed().as_secs_f64();
    let c = &r.census;
    let (distinct, h) = c.hist();
    eprintln!(
        "claim4-census: applied={} gaps={} class={} ids={} hidden={} unavail={} wall={wall_s:.3}s",
        r.events,
        c.gaps,
        c.total,
        distinct,
        c.hidden,
        c.unavail.len()
    );

    let mut notes = Vec::new();
    let coverage = r.events == expected && r.eof;
    if !coverage {
        notes.push(format!(
            "coverage FAIL: applied={} expected={} eof={}",
            r.events, expected, r.eof
        ));
    }
    let a_ok = c.inv_a == 0;
    if !a_ok {
        notes.push(format!("check_a FAIL: {} breaches", c.inv_a));
    }
    let b_ok = c.sig_checked == c.sig_ok && (c.total == 0 || c.sig_checked > 0);
    if !b_ok {
        notes.push(format!(
            "check_b FAIL: ok={} checked={}",
            c.sig_ok, c.sig_checked
        ));
    }
    let c_ok = c.gaps > 0 || c.unavail.is_empty();
    if !c_ok {
        notes.push(format!("check_c FAIL: gaps=0 unavail={}", c.unavail.len()));
    }
    let agg_ok = r.sec_count == c.total && r.sec_hidden == c.hidden;
    if !agg_ok {
        notes.push(format!(
            "section agg mismatch: section={}/{} census={}/{}",
            r.sec_count, r.sec_hidden, c.total, c.hidden
        ));
    }
    let zero_bad = expected > 1_000_000 && c.total == 0;
    if zero_bad {
        notes.push("zero native refreshes on multi-million-event day".into());
    }
    let gaps_ok = c.gaps == r.book_gaps;
    if !gaps_ok {
        notes.push(format!(
            "gaps mismatch: census={} book={}",
            c.gaps, r.book_gaps
        ));
    }

    let pass = coverage && a_ok && b_ok && c_ok && agg_ok && gaps_ok && !zero_bad;
    let verdict = if pass { "PASS" } else { "FAIL" };
    if notes.is_empty() {
        notes.push(format!(
            "refresh_window_ns={REFRESH_WINDOW_NS}; book_apply only; live_orders_eod={}",
            r.live_eod
        ));
    }

    let evidence = Evidence {
        gate: "claim4-refresh-census",
        date,
        git_sha,
        git_dirty,
        log: log_path.display().to_string(),
        symbol: r.symbol,
        trade_date: r.trade_date,
        expected_events: expected,
        events_applied: r.events,
        eof: r.eof,
        applied_seq: r.applied_seq,
        applied_ts: r.applied_ts,
        wall_s,
        events_per_sec: if wall_s > 0.0 {
            r.events as f64 / wall_s
        } else {
            0.0
        },
        refresh: serde_json::json!({
            "total_classifications": c.total,
            "distinct_refreshed_order_ids": distinct,
            "max_reloads": c.max_reloads,
            "reload_histogram": {"1": h[0], "2_4": h[1], "5_9": h[2], "10_plus": h[3]},
            "cumulative_hidden_volume": c.hidden,
            "refresh_unavailable_count": c.unavail.len() as u64,
            "gaps_encountered": c.gaps,
            "eod_native_live_orders": r.eod_native,
            "eod_unavailable_live_orders": r.eod_unavail,
            "live_orders_eod": r.live_eod,
            "refresh_window_ns": REFRESH_WINDOW_NS,
        }),
        checks: serde_json::json!({
            "coverage": if coverage {"PASS"} else {"FAIL"},
            "hidden_implies_reload": if a_ok {"PASS"} else {"FAIL"},
            "same_order_id_signature": if b_ok {"PASS"} else {"FAIL"},
            "zero_gap_unavailable": if c_ok {"PASS"} else {"FAIL"},
            "signature_checked": c.sig_checked,
            "signature_ok": c.sig_ok,
        }),
        notes: notes.join("; "),
        verdict,
    };

    let json = serde_json::to_string_pretty(&evidence).unwrap_or_else(|e| die(&e.to_string()));
    write_out(out_path.as_deref(), &format!("{json}\n"));
    eprintln!("claim4-census: verdict={verdict}");
    if verdict != "PASS" {
        exit(1);
    }
}

fn parse_args() -> (PathBuf, Option<PathBuf>) {
    let mut log = None;
    let mut out = None;
    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        match a.as_str() {
            "--out" => {
                out = Some(PathBuf::from(
                    args.next().unwrap_or_else(|| usage("missing --out path")),
                ))
            }
            "-h" | "--help" => usage("help"),
            flag if flag.starts_with('-') => usage(&format!("unknown flag {flag}")),
            path => {
                if log.is_some() {
                    usage("multiple log paths");
                }
                log = Some(PathBuf::from(path));
            }
        }
    }
    let log = log.unwrap_or_else(|| usage("missing <path.fftlog>"));
    if !log.is_file() {
        usage(&format!("log not found: {}", log.display()));
    }
    (log, out)
}
