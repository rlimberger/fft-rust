//! Seek / verify / throughput measurement loops.

use crate::report::{DistMs, LogInfo, Mismatch, Throughput, VerifyReport};
use fft_book::Book;
use fft_log::{KIND_CHECKPOINT, KIND_EVENTS, LogReader};
use fft_profile::MultiProfile;
use fft_replay::ReplaySource;
use std::path::Path;
use std::time::{Duration, Instant};

/// Per-seek outcome used by the verify pass: (target_ts, applied_seq, applied_ts).
pub type SeekOutcome = (u64, u64, u64);

/// xorshift64* — deterministic, no external RNG crate.
pub struct XorShift64(u64);

impl XorShift64 {
    pub fn new(seed: u64) -> Self {
        // Zero seed is a fixed point of xorshift; rehash.
        Self(if seed == 0 {
            0x9E37_79B9_7F4A_7C15
        } else {
            seed
        })
    }

    fn next_u64(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.0 = x;
        x
    }

    /// Uniform sample in `[lo, hi]` inclusive (tiny modulo bias accepted for gate targets).
    pub fn gen_inclusive(&mut self, lo: u64, hi: u64) -> u64 {
        if lo >= hi {
            return lo;
        }
        let span = hi - lo + 1;
        lo + (self.next_u64() % span)
    }
}

/// Event-time bounds + wire event-record count from the frame index/headers.
/// `event_count` includes TsReset wire records (header `count`); applied events are fewer.
pub fn inspect_log(path: &Path) -> (LogInfo, usize) {
    let (reader, report) = LogReader::open(path).unwrap_or_else(|e| {
        panic!("m2-gate: open log for inspection {}: {e}", path.display());
    });
    for w in &report.warnings {
        eprintln!("m2-gate: open warning: {w}");
    }
    let mut first_ts = None;
    let mut last_ts = 0u64;
    let mut event_count = 0u64;
    let mut checkpoint_count = 0usize;
    for i in 0..reader.frame_count() {
        let fh = reader.frame_header(i).unwrap_or_else(|e| {
            panic!("m2-gate: frame_header({i}): {e}");
        });
        if fh.kind == KIND_EVENTS {
            if first_ts.is_none() {
                first_ts = Some(fh.first_ts);
            }
            last_ts = fh.last_ts;
            event_count += u64::from(fh.count);
        } else if fh.kind == KIND_CHECKPOINT {
            checkpoint_count += 1;
        }
    }
    let first_ts = first_ts.unwrap_or_else(|| {
        panic!("m2-gate: log has no EVENTS frames: {}", path.display());
    });
    if last_ts < first_ts {
        panic!("m2-gate: last_ts < first_ts ({last_ts} < {first_ts})");
    }
    let span_s = (last_ts - first_ts) as f64 / 1e9;
    (
        LogInfo {
            path: path.display().to_string(),
            event_count,
            checkpoint_count,
            frame_count: reader.frame_count(),
            first_ts,
            last_ts,
            event_time_span_s: span_s,
        },
        checkpoint_count,
    )
}

pub fn fresh_state(src: &ReplaySource) -> (Book, MultiProfile) {
    let meta = src.meta();
    let book = Book::new(meta.min_price_increment);
    let mut profile = MultiProfile::new(meta.min_price_increment);
    profile.begin_session(meta.trade_date);
    (book, profile)
}

/// Six named section payloads used for order-exact equality.
struct Sections {
    book: Vec<u8>,
    flow: Vec<u8>,
    refresh: Vec<u8>,
    profile: Vec<u8>,
    cvd: Vec<u8>,
    session: Vec<u8>,
}

fn serialize_all(book: &Book, profile: &MultiProfile) -> Sections {
    let ps = profile.serialize();
    Sections {
        book: book.serialize_book(),
        flow: book.serialize_flow(),
        refresh: book.serialize_refresh(),
        profile: ps.profile,
        cvd: ps.cvd,
        session: ps.session,
    }
}

fn first_diff(a: &Sections, b: &Sections) -> Option<&'static str> {
    if a.book != b.book {
        return Some("BOOK");
    }
    if a.flow != b.flow {
        return Some("FLOW");
    }
    if a.refresh != b.refresh {
        return Some("REFRESH");
    }
    if a.profile != b.profile {
        return Some("PROFILE");
    }
    if a.cvd != b.cvd {
        return Some("CVD");
    }
    if a.session != b.session {
        return Some("SESSION");
    }
    None
}

/// Forward-apply every event with `ts <= target_ts` (same stop rule as `ReplaySource::seek`).
fn forward_to_ts(
    src: &mut ReplaySource,
    book: &mut Book,
    profile: &mut MultiProfile,
    target_ts: u64,
) {
    while let Some(next) = src
        .peek_event()
        .unwrap_or_else(|e| panic!("m2-gate: peek: {e}"))
    {
        if next.ts.0 > target_ts {
            break;
        }
        src.apply_next(book, profile)
            .unwrap_or_else(|e| panic!("m2-gate: apply_next: {e}"))
            .expect("event present after peek");
    }
    book.check_invariants();
}

pub fn sample_targets(seed: u64, n: usize, first_ts: u64, last_ts: u64) -> Vec<u64> {
    let mut rng = XorShift64::new(seed);
    let mut out = Vec::with_capacity(n);
    for _ in 0..n {
        out.push(rng.gen_inclusive(first_ts, last_ts));
    }
    out
}

pub fn run_seeks(path: &Path, targets: &[u64]) -> (Vec<Duration>, Vec<Duration>, Vec<SeekOutcome>) {
    // One long-lived source: cold = first seek of each (target) pair; warm = immediate
    // re-seek to the same target (mmap + checkpoint pages hot). Book/profile are always
    // fresh so measured work is restore + tail, not residual in-state mutation.
    let mut src = ReplaySource::open(path).unwrap_or_else(|e| {
        panic!("m2-gate: ReplaySource::open {}: {e}", path.display());
    });
    if src.checkpoint_count() == 0 {
        panic!(
            "m2-gate: log has zero CHECKPOINT frames — Seek is forbidden on a checkpoint-less log.\n\
             Run `fft-checkpoint {} <checkpointed.fftlog>` and pass the -ckpt copy.",
            path.display()
        );
    }

    let mut cold = Vec::with_capacity(targets.len());
    let mut warm = Vec::with_capacity(targets.len());
    let mut outcomes = Vec::with_capacity(targets.len());

    for (i, &target) in targets.iter().enumerate() {
        let (mut book, mut profile) = fresh_state(&src);
        let t0 = Instant::now();
        let report = src
            .seek(target, &mut book, &mut profile, || false)
            .unwrap_or_else(|e| panic!("m2-gate: cold seek[{i}] target={target}: {e}"));
        let cold_dt = t0.elapsed();
        if report.cancelled {
            panic!("m2-gate: cold seek[{i}] cancelled (cancel always false)");
        }
        cold.push(cold_dt);
        outcomes.push((target, report.applied_seq, report.applied_ts));

        let (mut book_w, mut profile_w) = fresh_state(&src);
        let t1 = Instant::now();
        let report_w = src
            .seek(target, &mut book_w, &mut profile_w, || false)
            .unwrap_or_else(|e| panic!("m2-gate: warm seek[{i}] target={target}: {e}"));
        let warm_dt = t1.elapsed();
        if report_w.cancelled {
            panic!("m2-gate: warm seek[{i}] cancelled");
        }
        warm.push(warm_dt);

        if (i + 1) % 100 == 0 || i + 1 == targets.len() {
            eprintln!(
                "m2-gate: seeks {}/{}  last cold={:.3} ms warm={:.3} ms  ckpt={:?} tail={}",
                i + 1,
                targets.len(),
                cold_dt.as_secs_f64() * 1e3,
                warm_dt.as_secs_f64() * 1e3,
                report.checkpoint_frame,
                report.tail_events,
            );
        }
    }
    (cold, warm, outcomes)
}

pub fn run_verify(path: &Path, outcomes: &[SeekOutcome], verify_n: usize) -> VerifyReport {
    let n = verify_n.min(outcomes.len());
    if n == 0 {
        return VerifyReport {
            count: 0,
            passed: 0,
            failed: 0,
            mismatch: None,
        };
    }
    let mut passed = 0usize;
    for (i, &(target_ts, seek_seq, seek_ts)) in outcomes.iter().take(n).enumerate() {
        eprintln!(
            "m2-gate: verify {}/{} target_ts={target_ts} (forward from open)…",
            i + 1,
            n
        );

        let mut seek_src = ReplaySource::open(path).unwrap_or_else(|e| {
            panic!("m2-gate: verify seek open: {e}");
        });
        let (mut seek_book, mut seek_profile) = fresh_state(&seek_src);
        let srep = seek_src
            .seek(target_ts, &mut seek_book, &mut seek_profile, || false)
            .unwrap_or_else(|e| panic!("m2-gate: verify seek: {e}"));
        assert!(!srep.cancelled);
        assert_eq!(srep.applied_seq, seek_seq);
        assert_eq!(srep.applied_ts, seek_ts);

        let mut fwd_src = ReplaySource::open(path).unwrap_or_else(|e| {
            panic!("m2-gate: verify forward open: {e}");
        });
        let (mut fwd_book, mut fwd_profile) = fresh_state(&fwd_src);
        forward_to_ts(&mut fwd_src, &mut fwd_book, &mut fwd_profile, target_ts);

        if fwd_src.applied_seq() != seek_src.applied_seq()
            || fwd_src.applied_ts() != seek_src.applied_ts()
        {
            return VerifyReport {
                count: n,
                passed,
                failed: 1,
                mismatch: Some(Mismatch {
                    target_ts,
                    section: "applied_seq/ts".into(),
                    seek_applied_seq: seek_src.applied_seq(),
                    seek_applied_ts: seek_src.applied_ts(),
                    forward_applied_seq: fwd_src.applied_seq(),
                    forward_applied_ts: fwd_src.applied_ts(),
                }),
            };
        }

        let seek_secs = serialize_all(&seek_book, &seek_profile);
        let fwd_secs = serialize_all(&fwd_book, &fwd_profile);
        if let Some(section) = first_diff(&seek_secs, &fwd_secs) {
            eprintln!("m2-gate: BIT-IDENTITY FAIL target_ts={target_ts} section={section}");
            return VerifyReport {
                count: n,
                passed,
                failed: 1,
                mismatch: Some(Mismatch {
                    target_ts,
                    section: section.into(),
                    seek_applied_seq: seek_src.applied_seq(),
                    seek_applied_ts: seek_src.applied_ts(),
                    forward_applied_seq: fwd_src.applied_seq(),
                    forward_applied_ts: fwd_src.applied_ts(),
                }),
            };
        }
        passed += 1;
    }
    VerifyReport {
        count: n,
        passed,
        failed: 0,
        mismatch: None,
    }
}

pub fn run_throughput(path: &Path, event_time_span_s: f64) -> Throughput {
    let mut src = ReplaySource::open(path).unwrap_or_else(|e| {
        panic!("m2-gate: throughput open: {e}");
    });
    let (mut book, mut profile) = fresh_state(&src);
    let t0 = Instant::now();
    let mut events = 0u64;
    loop {
        match src.apply_next(&mut book, &mut profile) {
            Ok(Some(_)) => events += 1,
            Ok(None) => break,
            Err(e) => panic!("m2-gate: throughput apply: {e}"),
        }
    }
    let wall_s = t0.elapsed().as_secs_f64();
    let events_per_sec = if wall_s > 0.0 {
        events as f64 / wall_s
    } else {
        f64::INFINITY
    };
    let realtime_multiple = if wall_s > 0.0 {
        event_time_span_s / wall_s
    } else {
        f64::INFINITY
    };
    Throughput {
        events,
        wall_s,
        events_per_sec,
        event_time_span_s,
        realtime_multiple,
    }
}

/// Gate: p95 seek-to-exact-state ≤ 250 ms (cold and warm).
pub const P95_BUDGET_MS: f64 = 250.0;
/// Gate: forward replay ≥ 60× realtime.
pub const REALTIME_BUDGET: f64 = 60.0;

pub fn decide_verdict(
    label: Option<&str>,
    cold: &DistMs,
    warm: &DistMs,
    verify: &VerifyReport,
    thr: &Throughput,
) -> (String, Option<String>) {
    if verify.failed > 0 {
        let m = verify.mismatch.as_ref().expect("failed implies mismatch");
        return (
            "FAIL".into(),
            Some(format!(
                "bit-identity mismatch at target_ts={} section={}",
                m.target_ts, m.section
            )),
        );
    }
    if label.is_some_and(|l| l.eq_ignore_ascii_case("SMOKE")) {
        return (
            "SMOKE".into(),
            Some(
                "SMOKE run — distributions recorded; do not claim M2 gate numbers from this output"
                    .into(),
            ),
        );
    }
    let mut fails = Vec::new();
    if cold.p95_ms > P95_BUDGET_MS {
        fails.push(format!(
            "cold p95 {:.3} ms > {P95_BUDGET_MS} ms",
            cold.p95_ms
        ));
    }
    if warm.p95_ms > P95_BUDGET_MS {
        fails.push(format!(
            "warm p95 {:.3} ms > {P95_BUDGET_MS} ms",
            warm.p95_ms
        ));
    }
    if thr.realtime_multiple < REALTIME_BUDGET {
        fails.push(format!(
            "realtime_multiple {:.2}× < {REALTIME_BUDGET}×",
            thr.realtime_multiple
        ));
    }
    if verify.count == 0 {
        fails.push("verify count is 0 (no bit-identity evidence)".into());
    }
    if fails.is_empty() {
        ("PASS".into(), None)
    } else {
        ("FAIL".into(), Some(fails.join("; ")))
    }
}
