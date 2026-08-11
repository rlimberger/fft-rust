//! M1 data-plane gate evidence harness (IMPLEMENTATION-PLAN.md "M1 — Data plane").
//!
//! Headless: consumes pre-ingested fftlog v2 files (ingest stays on `fft-ingest write`),
//! measures (b) busiest-day book+profile apply time, (c) N-chunk ≡ one-shot differential
//! on real data, (d) bytes/event (and legacy size ratio when legacy logs are readable).
//!
//! Quiet-box runs produce claimable numbers; concurrent-build runs must pass `--smoke`
//! so the evidence blob labels timings SMOKE (builds pollute wall-clock).
//!
//! ```text
//! m1-gate --out <evidence.json> [--smoke] [--seed N] [--legacy-dir DIR] [--diff-trials N] \
//!         <day1.fftlog> [day2.fftlog ...]
//! ```

use fft_book::Book;
use fft_log::LogReader;
use fft_profile::MultiProfile;
use fft_replay::ReplaySource;
use std::fs::{self, File};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, exit};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

const APPLY_BUDGET: Duration = Duration::from_secs(3600);
const DEFAULT_DIFF_TRIALS: usize = 7;
const DEFAULT_SEED: u64 = 0xC0FF_EE42_A1A7_E011;

fn usage(msg: &str) -> ! {
    eprintln!(
        "m1-gate: {msg}\n\
         usage: m1-gate --out <evidence.json> [--smoke] [--seed N] [--legacy-dir DIR] \
         [--diff-trials N] <day.fftlog>..."
    );
    exit(2);
}

#[derive(Debug, Clone)]
struct DayStat {
    path: PathBuf,
    trade_date: u32,
    trade_date_ymd: String,
    symbol: String,
    event_count: u64,
    file_bytes: u64,
    bytes_per_event: f64,
    legacy_bytes: Option<u64>,
    legacy_ratio: Option<f64>,
    legacy_status: String,
}

#[derive(Debug)]
struct ApplyResult {
    events: u64,
    seconds: f64,
    book_bytes: Vec<u8>,
    flow_bytes: Vec<u8>,
    refresh_bytes: Vec<u8>,
    profile_bytes: Vec<u8>,
    cvd_bytes: Vec<u8>,
    session_bytes: Vec<u8>,
    applied_seq: u64,
    applied_ts: u64,
}

#[derive(Debug)]
struct DiffTrial {
    trial: usize,
    n_chunks: usize,
    chunk_sizes: Vec<usize>,
    match_oneshot: bool,
    seconds: f64,
    fail_reason: Option<String>,
}

fn main() {
    let mut out: Option<PathBuf> = None;
    let mut smoke = false;
    let mut seed = DEFAULT_SEED;
    let mut legacy_dir: Option<PathBuf> = None;
    let mut diff_trials = DEFAULT_DIFF_TRIALS;
    let mut logs: Vec<PathBuf> = Vec::new();

    let mut args = std::env::args().skip(1).peekable();
    while let Some(a) = args.next() {
        match a.as_str() {
            "--out" => {
                out = Some(PathBuf::from(
                    args.next().unwrap_or_else(|| usage("missing --out path")),
                ));
            }
            "--smoke" => smoke = true,
            "--seed" => {
                let s = args.next().unwrap_or_else(|| usage("missing --seed value"));
                seed = parse_u64(&s, "--seed");
            }
            "--legacy-dir" => {
                legacy_dir = Some(PathBuf::from(
                    args.next()
                        .unwrap_or_else(|| usage("missing --legacy-dir path")),
                ));
            }
            "--diff-trials" => {
                let s = args
                    .next()
                    .unwrap_or_else(|| usage("missing --diff-trials value"));
                diff_trials = parse_u64(&s, "--diff-trials") as usize;
                if diff_trials == 0 {
                    usage("--diff-trials must be ≥ 1");
                }
            }
            "-h" | "--help" => usage("help"),
            flag if flag.starts_with('-') => usage(&format!("unknown flag {flag}")),
            path => logs.push(PathBuf::from(path)),
        }
    }

    let out = out.unwrap_or_else(|| usage("missing --out"));
    if logs.is_empty() {
        usage("need at least one .fftlog path");
    }

    let git_sha = capture_git_sha();
    let date_utc = utc_date_string();
    let timing_label = if smoke { "SMOKE" } else { "QUIET_BOX" };

    eprintln!(
        "m1-gate: {} log(s), seed={seed:#x}, diff_trials={diff_trials}, label={timing_label}, git={git_sha}",
        logs.len()
    );

    let mut days: Vec<DayStat> = Vec::with_capacity(logs.len());
    for path in &logs {
        match inspect_day(path, legacy_dir.as_deref()) {
            Ok(d) => {
                eprintln!(
                    "  day {} {} events={} bytes={} B/ev={:.3} legacy={}",
                    d.trade_date_ymd,
                    d.symbol,
                    d.event_count,
                    d.file_bytes,
                    d.bytes_per_event,
                    d.legacy_status
                );
                days.push(d);
            }
            Err(e) => {
                eprintln!("m1-gate: inspect {}: {e}", path.display());
                exit(1);
            }
        }
    }

    let busiest_idx = days
        .iter()
        .enumerate()
        .max_by_key(|(_, d)| d.event_count)
        .map(|(i, _)| i)
        .expect("non-empty days");
    let busiest = &days[busiest_idx];
    eprintln!(
        "m1-gate: busiest day {} ({} events) — forward apply…",
        busiest.trade_date_ymd, busiest.event_count
    );

    let oneshot = match apply_oneshot(&busiest.path) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("m1-gate: oneshot apply failed: {e}");
            exit(1);
        }
    };
    if oneshot.events != busiest.event_count {
        eprintln!(
            "m1-gate: WARN oneshot events {} ≠ inspected {}",
            oneshot.events, busiest.event_count
        );
    }
    eprintln!(
        "m1-gate: oneshot apply {:.6}s for {} events ({timing_label})",
        oneshot.seconds, oneshot.events
    );

    // ≤ 3 s: revised budget (René 2026-08-11, IMPLEMENTATION-PLAN M1 gate) — the
    // original < 2 s predates per-event refresh/flow/profile state.
    let apply_budget_s = 3.0_f64;
    let apply_pass = oneshot.seconds <= apply_budget_s;
    eprintln!(
        "m1-gate: apply gate (<= {apply_budget_s} s): {} ({timing_label})",
        if apply_pass { "PASS" } else { "FAIL" }
    );

    eprintln!("m1-gate: differential {diff_trials} random chunk splits…");
    let mut rng = XorShift64::new(seed);
    let mut trials: Vec<DiffTrial> = Vec::with_capacity(diff_trials);
    let mut all_match = true;
    for t in 0..diff_trials {
        let n_chunks = 2 + (rng.next() as usize % 15); // 2..=16
        let chunk_sizes = split_sizes(oneshot.events as usize, n_chunks, &mut rng);
        let trial = match apply_chunked(&busiest.path, &chunk_sizes, &oneshot) {
            Ok(mut tr) => {
                tr.trial = t;
                tr.n_chunks = chunk_sizes.len();
                tr.chunk_sizes = chunk_sizes;
                tr
            }
            Err(e) => DiffTrial {
                trial: t,
                n_chunks: chunk_sizes.len(),
                chunk_sizes,
                match_oneshot: false,
                seconds: 0.0,
                fail_reason: Some(e),
            },
        };
        if !trial.match_oneshot {
            all_match = false;
        }
        eprintln!(
            "  trial {t}: chunks={} sizes={:?} match={} {:.6}s{}",
            trial.n_chunks,
            trial.chunk_sizes,
            trial.match_oneshot,
            trial.seconds,
            trial
                .fail_reason
                .as_ref()
                .map(|r| format!(" reason={r}"))
                .unwrap_or_default()
        );
        trials.push(trial);
    }
    eprintln!(
        "m1-gate: differential verdict: {}",
        if all_match { "PASS" } else { "FAIL" }
    );

    let mut size_ratio_unverified = false;
    for d in &days {
        if d.legacy_ratio.is_none() {
            size_ratio_unverified = true;
        }
    }
    let size_claim = if size_ratio_unverified {
        "UNVERIFIED"
    } else {
        let max_ratio = days
            .iter()
            .filter_map(|d| d.legacy_ratio)
            .fold(0.0_f64, f64::max);
        if max_ratio <= 0.5 { "PASS" } else { "FAIL" }
    };

    let json = render_json(
        &git_sha,
        &date_utc,
        timing_label,
        smoke,
        seed,
        &days,
        busiest,
        &oneshot,
        apply_pass,
        apply_budget_s,
        &trials,
        all_match,
        size_claim,
        size_ratio_unverified,
    );

    if let Some(parent) = out.parent()
        && !parent.as_os_str().is_empty()
    {
        fs::create_dir_all(parent).unwrap_or_else(|e| {
            eprintln!("m1-gate: create {}: {e}", parent.display());
            exit(1);
        });
    }
    let mut f = File::create(&out).unwrap_or_else(|e| {
        eprintln!("m1-gate: write {}: {e}", out.display());
        exit(1);
    });
    f.write_all(json.as_bytes()).unwrap_or_else(|e| {
        eprintln!("m1-gate: write {}: {e}", out.display());
        exit(1);
    });
    eprintln!("m1-gate: wrote {}", out.display());

    // Non-zero exit only on hard failures (I/O already exited). Timing FAIL under
    // --smoke is still exit 0 so CI smoke can land; quiet-box FAIL exits 1.
    if !smoke && (!apply_pass || !all_match) {
        exit(1);
    }
}

fn inspect_day(path: &Path, legacy_dir: Option<&Path>) -> Result<DayStat, String> {
    let file_bytes = fs::metadata(path).map_err(|e| format!("stat: {e}"))?.len();
    let (reader, report) = LogReader::open(path).map_err(|e| format!("open: {e}"))?;
    for w in &report.warnings {
        eprintln!("  open warning ({}): {w}", path.display());
    }
    let meta = reader.meta().clone();
    let event_count = count_events(&reader)?;
    let trade_date_ymd = ymd_from_unix_days(meta.trade_date);
    let bytes_per_event = if event_count == 0 {
        0.0
    } else {
        file_bytes as f64 / event_count as f64
    };

    let (legacy_bytes, legacy_ratio, legacy_status) =
        match resolve_legacy(legacy_dir, &trade_date_ymd, &meta.symbol) {
            LegacyLookup::NotRequested => (None, None, "not_requested".into()),
            LegacyLookup::Missing(p) => (None, None, format!("missing:{}", p.display())),
            LegacyLookup::Unreadable(p, e) => {
                (None, None, format!("unreadable:{}:{e}", p.display()))
            }
            LegacyLookup::Ok(p, nbytes) => {
                let ratio = file_bytes as f64 / nbytes as f64;
                (
                    Some(nbytes),
                    Some(ratio),
                    format!("ok:{}:{}B", p.display(), nbytes),
                )
            }
        };

    Ok(DayStat {
        path: path.to_path_buf(),
        trade_date: meta.trade_date,
        trade_date_ymd,
        symbol: meta.symbol,
        event_count,
        file_bytes,
        bytes_per_event,
        legacy_bytes,
        legacy_ratio,
        legacy_status,
    })
}

enum LegacyLookup {
    NotRequested,
    Missing(PathBuf),
    Unreadable(PathBuf, String),
    Ok(PathBuf, u64),
}

fn resolve_legacy(legacy_dir: Option<&Path>, ymd: &str, symbol: &str) -> LegacyLookup {
    let Some(dir) = legacy_dir else {
        return LegacyLookup::NotRequested;
    };
    let name = format!("{ymd}-{symbol}.fftlog");
    let p = dir.join(name);
    if !p.exists() {
        return LegacyLookup::Missing(p);
    }
    match fs::metadata(&p) {
        Ok(m) => LegacyLookup::Ok(p, m.len()),
        Err(e) => LegacyLookup::Unreadable(p, e.to_string()),
    }
}

/// Canonical event count (TsReset framing records are internal to fft-log and
/// never surface as `CanonicalEvent`s — count via the decode path so numbers
/// match `apply_forward` / HANDOFF expected totals).
fn count_events(reader: &LogReader) -> Result<u64, String> {
    let mut n = 0u64;
    for ev in reader.events(0..reader.frame_count()) {
        ev.map_err(|e| format!("events: {e}"))?;
        n += 1;
    }
    Ok(n)
}

fn apply_oneshot(path: &Path) -> Result<ApplyResult, String> {
    let mut src = ReplaySource::open(path).map_err(|e| format!("open: {e}"))?;
    let mut book = Book::new(src.meta().min_price_increment);
    let mut profile = MultiProfile::new(src.meta().min_price_increment);
    profile.begin_session(src.meta().trade_date);

    let t0 = Instant::now();
    let progress = src
        .apply_forward(&mut book, &mut profile, usize::MAX, APPLY_BUDGET)
        .map_err(|e| format!("apply_forward: {e}"))?;
    let seconds = t0.elapsed().as_secs_f64();
    if !progress.eof {
        return Err(format!(
            "oneshot did not reach EOF after {} events (budget?)",
            progress.events
        ));
    }
    book.check_invariants();
    let secs = profile.serialize();
    Ok(ApplyResult {
        events: progress.events,
        seconds,
        book_bytes: book.serialize_book(),
        flow_bytes: book.serialize_flow(),
        refresh_bytes: book.serialize_refresh(),
        profile_bytes: secs.profile,
        cvd_bytes: secs.cvd,
        session_bytes: secs.session,
        applied_seq: progress.applied_seq,
        applied_ts: progress.applied_ts,
    })
}

fn apply_chunked(
    path: &Path,
    chunk_sizes: &[usize],
    oneshot: &ApplyResult,
) -> Result<DiffTrial, String> {
    let mut src = ReplaySource::open(path).map_err(|e| format!("open: {e}"))?;
    let mut book = Book::new(src.meta().min_price_increment);
    let mut profile = MultiProfile::new(src.meta().min_price_increment);
    profile.begin_session(src.meta().trade_date);

    let t0 = Instant::now();
    let mut total = 0u64;
    let mut eof = false;
    for &max in chunk_sizes {
        if eof {
            break;
        }
        let progress = src
            .apply_forward(&mut book, &mut profile, max, APPLY_BUDGET)
            .map_err(|e| format!("chunk apply: {e}"))?;
        total += progress.events;
        eof = progress.eof;
        if progress.events == 0 && !eof {
            return Err("chunk made no progress".into());
        }
    }
    // Drain any remainder if split undershot (duplicate cut points / rounding).
    if !eof {
        let progress = src
            .apply_forward(&mut book, &mut profile, usize::MAX, APPLY_BUDGET)
            .map_err(|e| format!("tail drain: {e}"))?;
        total += progress.events;
        eof = progress.eof;
    }
    let seconds = t0.elapsed().as_secs_f64();
    if !eof {
        return Err(format!("chunked did not reach EOF after {total} events"));
    }
    book.check_invariants();
    let secs = profile.serialize();

    let mut fail_reason = None;
    if total != oneshot.events {
        fail_reason = Some(format!("event count {total} != oneshot {}", oneshot.events));
    } else if book.serialize_book() != oneshot.book_bytes {
        fail_reason = Some("serialize_book mismatch".into());
    } else if book.serialize_flow() != oneshot.flow_bytes {
        fail_reason = Some("serialize_flow mismatch".into());
    } else if book.serialize_refresh() != oneshot.refresh_bytes {
        fail_reason = Some("serialize_refresh mismatch".into());
    } else if secs.profile != oneshot.profile_bytes {
        fail_reason = Some("profile section mismatch".into());
    } else if secs.cvd != oneshot.cvd_bytes {
        fail_reason = Some("cvd section mismatch".into());
    } else if secs.session != oneshot.session_bytes {
        fail_reason = Some("session section mismatch".into());
    } else if src.applied_seq() != oneshot.applied_seq || src.applied_ts() != oneshot.applied_ts {
        fail_reason = Some(format!(
            "cursor mismatch seq/ts {}/{} vs {}/{}",
            src.applied_seq(),
            src.applied_ts(),
            oneshot.applied_seq,
            oneshot.applied_ts
        ));
    }

    Ok(DiffTrial {
        trial: 0,
        n_chunks: chunk_sizes.len(),
        chunk_sizes: chunk_sizes.to_vec(),
        match_oneshot: fail_reason.is_none(),
        seconds,
        fail_reason,
    })
}

/// Split `n` events into `n_chunks` positive sizes (last chunk absorbs remainder).
fn split_sizes(n: usize, n_chunks: usize, rng: &mut XorShift64) -> Vec<usize> {
    if n == 0 {
        return vec![0; n_chunks.max(1)];
    }
    let k = n_chunks.clamp(1, n);
    if k == 1 {
        return vec![n];
    }
    // k-1 distinct cut points in 1..n
    let mut cuts: Vec<usize> = Vec::with_capacity(k - 1);
    while cuts.len() < k - 1 {
        let c = 1 + (rng.next() as usize % (n - 1));
        if !cuts.contains(&c) {
            cuts.push(c);
        }
    }
    cuts.sort_unstable();
    let mut sizes = Vec::with_capacity(k);
    let mut prev = 0usize;
    for c in cuts {
        sizes.push(c - prev);
        prev = c;
    }
    sizes.push(n - prev);
    sizes
}

/// xorshift64* — deterministic, no external rng dep.
struct XorShift64(u64);

impl XorShift64 {
    fn new(seed: u64) -> Self {
        Self(if seed == 0 {
            0x9E37_79B9_7F4A_7C15
        } else {
            seed
        })
    }
    fn next(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.0 = x;
        x
    }
}

fn parse_u64(s: &str, flag: &str) -> u64 {
    if let Some(hex) = s.strip_prefix("0x").or_else(|| s.strip_prefix("0X")) {
        u64::from_str_radix(hex, 16).unwrap_or_else(|_| usage(&format!("bad {flag} hex: {s}")))
    } else {
        s.parse::<u64>()
            .unwrap_or_else(|_| usage(&format!("bad {flag}: {s}")))
    }
}

fn capture_git_sha() -> String {
    let output = Command::new("git")
        .args(["rev-parse", "HEAD"])
        .output()
        .ok();
    match output {
        Some(o) if o.status.success() => String::from_utf8_lossy(&o.stdout).trim().to_string(),
        _ => "unknown".into(),
    }
}

fn utc_date_string() -> String {
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let days = (secs / 86_400) as u32;
    ymd_from_unix_days(days)
}

/// Civil YYYY-MM-DD from days since Unix epoch (UTC). Howard Hinnant algorithm.
fn ymd_from_unix_days(days: u32) -> String {
    let z = i64::from(days) + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    format!("{y:04}-{m:02}-{d:02}")
}

fn json_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 8);
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if c.is_control() => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out
}

#[allow(clippy::too_many_arguments)]
fn render_json(
    git_sha: &str,
    date_utc: &str,
    timing_label: &str,
    smoke: bool,
    seed: u64,
    days: &[DayStat],
    busiest: &DayStat,
    oneshot: &ApplyResult,
    apply_pass: bool,
    apply_budget_s: f64,
    trials: &[DiffTrial],
    differential_pass: bool,
    size_claim: &str,
    size_ratio_unverified: bool,
) -> String {
    let mut s = String::with_capacity(4096);
    s.push_str("{\n");
    s.push_str("  \"gate\": \"M1-data-plane\",\n");
    s.push_str(&format!("  \"git_sha\": \"{}\",\n", json_escape(git_sha)));
    s.push_str(&format!("  \"date\": \"{}\",\n", json_escape(date_utc)));
    s.push_str(&format!(
        "  \"timing_label\": \"{}\",\n",
        json_escape(timing_label)
    ));
    s.push_str(&format!(
        "  \"smoke\": {},\n",
        if smoke { "true" } else { "false" }
    ));
    s.push_str(&format!("  \"seed\": {seed},\n"));
    s.push_str("  \"note\": \"SMOKE timings are not claimable; only QUIET_BOX runs on an idle host count as gate evidence\",\n");

    s.push_str("  \"days\": [\n");
    for (i, d) in days.iter().enumerate() {
        s.push_str("    {\n");
        s.push_str(&format!(
            "      \"path\": \"{}\",\n",
            json_escape(&d.path.display().to_string())
        ));
        s.push_str(&format!("      \"trade_date\": {},\n", d.trade_date));
        s.push_str(&format!(
            "      \"trade_date_ymd\": \"{}\",\n",
            json_escape(&d.trade_date_ymd)
        ));
        s.push_str(&format!(
            "      \"symbol\": \"{}\",\n",
            json_escape(&d.symbol)
        ));
        s.push_str(&format!("      \"event_count\": {},\n", d.event_count));
        s.push_str(&format!("      \"file_bytes\": {},\n", d.file_bytes));
        s.push_str(&format!(
            "      \"bytes_per_event\": {:.6},\n",
            d.bytes_per_event
        ));
        match d.legacy_bytes {
            Some(b) => s.push_str(&format!("      \"legacy_bytes\": {b},\n")),
            None => s.push_str("      \"legacy_bytes\": null,\n"),
        }
        match d.legacy_ratio {
            Some(r) => s.push_str(&format!("      \"legacy_ratio\": {r:.6},\n")),
            None => s.push_str("      \"legacy_ratio\": null,\n"),
        }
        s.push_str(&format!(
            "      \"legacy_status\": \"{}\"\n",
            json_escape(&d.legacy_status)
        ));
        s.push_str("    }");
        if i + 1 < days.len() {
            s.push(',');
        }
        s.push('\n');
    }
    s.push_str("  ],\n");

    s.push_str("  \"busiest_day\": {\n");
    s.push_str(&format!(
        "    \"trade_date_ymd\": \"{}\",\n",
        json_escape(&busiest.trade_date_ymd)
    ));
    s.push_str(&format!("    \"event_count\": {},\n", busiest.event_count));
    s.push_str(&format!(
        "    \"path\": \"{}\"\n",
        json_escape(&busiest.path.display().to_string())
    ));
    s.push_str("  },\n");

    s.push_str("  \"forward_apply\": {\n");
    s.push_str(&format!("    \"events\": {},\n", oneshot.events));
    s.push_str(&format!("    \"seconds\": {:.9},\n", oneshot.seconds));
    s.push_str(&format!("    \"budget_seconds\": {apply_budget_s},\n"));
    s.push_str(&format!(
        "    \"verdict\": \"{}\",\n",
        if apply_pass { "PASS" } else { "FAIL" }
    ));
    s.push_str(&format!(
        "    \"timing_label\": \"{}\",\n",
        json_escape(timing_label)
    ));
    s.push_str(&format!("    \"applied_seq\": {},\n", oneshot.applied_seq));
    s.push_str(&format!("    \"applied_ts\": {},\n", oneshot.applied_ts));
    s.push_str(&format!(
        "    \"serialize_book_bytes\": {},\n",
        oneshot.book_bytes.len()
    ));
    s.push_str(&format!(
        "    \"serialize_flow_bytes\": {},\n",
        oneshot.flow_bytes.len()
    ));
    s.push_str(&format!(
        "    \"serialize_refresh_bytes\": {},\n",
        oneshot.refresh_bytes.len()
    ));
    s.push_str(&format!(
        "    \"profile_section_bytes\": {},\n",
        oneshot.profile_bytes.len()
    ));
    s.push_str(&format!(
        "    \"cvd_section_bytes\": {},\n",
        oneshot.cvd_bytes.len()
    ));
    s.push_str(&format!(
        "    \"session_section_bytes\": {}\n",
        oneshot.session_bytes.len()
    ));
    s.push_str("  },\n");

    s.push_str("  \"differential\": {\n");
    s.push_str(&format!(
        "    \"verdict\": \"{}\",\n",
        if differential_pass { "PASS" } else { "FAIL" }
    ));
    s.push_str(&format!("    \"trials\": {},\n", trials.len()));
    s.push_str("    \"results\": [\n");
    for (i, t) in trials.iter().enumerate() {
        s.push_str("      {\n");
        s.push_str(&format!("        \"trial\": {},\n", t.trial));
        s.push_str(&format!("        \"n_chunks\": {},\n", t.n_chunks));
        s.push_str("        \"chunk_sizes\": [");
        for (j, c) in t.chunk_sizes.iter().enumerate() {
            if j > 0 {
                s.push_str(", ");
            }
            s.push_str(&c.to_string());
        }
        s.push_str("],\n");
        s.push_str(&format!(
            "        \"match_oneshot\": {},\n",
            if t.match_oneshot { "true" } else { "false" }
        ));
        s.push_str(&format!("        \"seconds\": {:.9},\n", t.seconds));
        match &t.fail_reason {
            Some(r) => s.push_str(&format!(
                "        \"fail_reason\": \"{}\"\n",
                json_escape(r)
            )),
            None => s.push_str("        \"fail_reason\": null\n"),
        }
        s.push_str("      }");
        if i + 1 < trials.len() {
            s.push(',');
        }
        s.push('\n');
    }
    s.push_str("    ]\n");
    s.push_str("  },\n");

    s.push_str("  \"log_size\": {\n");
    s.push_str(&format!(
        "    \"claim_leq_half_legacy\": \"{}\",\n",
        json_escape(size_claim)
    ));
    s.push_str(&format!(
        "    \"ratio_unverified\": {},\n",
        if size_ratio_unverified {
            "true"
        } else {
            "false"
        }
    ));
    s.push_str("    \"bytes_per_event_table\": [\n");
    for (i, d) in days.iter().enumerate() {
        s.push_str("      {\n");
        s.push_str(&format!(
            "        \"trade_date_ymd\": \"{}\",\n",
            json_escape(&d.trade_date_ymd)
        ));
        s.push_str(&format!("        \"event_count\": {},\n", d.event_count));
        s.push_str(&format!("        \"v2_bytes\": {},\n", d.file_bytes));
        s.push_str(&format!(
            "        \"v2_bytes_per_event\": {:.6},\n",
            d.bytes_per_event
        ));
        match d.legacy_bytes {
            Some(b) => s.push_str(&format!("        \"legacy_bytes\": {b},\n")),
            None => s.push_str("        \"legacy_bytes\": null,\n"),
        }
        match d.legacy_ratio {
            Some(r) => s.push_str(&format!("        \"v2_over_legacy\": {r:.6},\n")),
            None => s.push_str("        \"v2_over_legacy\": null,\n"),
        }
        s.push_str(&format!(
            "        \"legacy_status\": \"{}\"\n",
            json_escape(&d.legacy_status)
        ));
        s.push_str("      }");
        if i + 1 < days.len() {
            s.push(',');
        }
        s.push('\n');
    }
    s.push_str("    ]\n");
    s.push_str("  }\n");
    s.push_str("}\n");
    s
}
