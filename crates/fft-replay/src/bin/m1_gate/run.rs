//! Day inspection, oneshot/chunked apply, and deterministic split helpers.

use crate::report::{ApplyResult, DayStat, DiffTrial, ymd_from_unix_days};
use fft_book::Book;
use fft_log::LogReader;
use fft_profile::MultiProfile;
use fft_replay::ReplaySource;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

const APPLY_BUDGET: Duration = Duration::from_secs(3600);

pub fn inspect_day(path: &Path, legacy_dir: Option<&Path>) -> Result<DayStat, String> {
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

pub fn apply_oneshot(path: &Path) -> Result<ApplyResult, String> {
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

pub fn apply_chunked(
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
pub fn split_sizes(n: usize, n_chunks: usize, rng: &mut XorShift64) -> Vec<usize> {
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
pub struct XorShift64(u64);

impl XorShift64 {
    pub fn new(seed: u64) -> Self {
        Self(if seed == 0 {
            0x9E37_79B9_7F4A_7C15
        } else {
            seed
        })
    }
    pub fn next(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.0 = x;
        x
    }
}
