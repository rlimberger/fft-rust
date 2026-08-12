//! Scrub-release → rendered-exact-book latency gate (PRD §4 claim 1 letter).
//!
//! Enabled only by `--scrub-latency-gate`. Measures wall ms from scrub release (dirty
//! pending target, drag ended) through the Seek generation bind to the first
//! `Shell::render` that adopts a matching `seek_generation` — the same bar as
//! `startup_trace::note_first_interactive` (snapshot already loaded for paint).
//!
//! Zero overhead when disabled aside from one atomic load per hook.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::Instant;

use serde::Serialize;

use crate::gate_report::{GitInfo, Verdict, command_line, now_rfc3339_utc, round3};

/// Default RNG seed: ASCII "SCRUB" as big-endian digits.
/// ASCII "SCRUB" as big-endian (low 5 bytes).
pub const DEFAULT_SEED: u64 = 0x0053_4352_5542;
/// PRD §4 claim-1 letter budget.
pub const BUDGET_P95_MS: f64 = 250.0;
/// No matching `seek_generation` within this window ⇒ wedge FAIL.
const WEDGE_TIMEOUT_MS: f64 = 30_000.0;

const METHODOLOGY: &str = "T0=scrub-release (end_scrub/script dirty pending); \
bind gen on take_coalesced_seek; T1=Shell::render adopt matching seek_generation \
(pre-paint bar as startup-trace interactive). GPUI present path, not headless.";

static ENABLED: AtomicBool = AtomicBool::new(false);
static STATE: OnceLock<Mutex<State>> = OnceLock::new();

#[derive(Debug)]
struct State {
    n: u32,
    out_path: PathBuf,
    seed: u64,
    rng: u64,
    budget_p95_ms: f64,
    log: PathBuf,
    binary: String,
    git: GitInfo,
    first_ts: Option<u64>,
    last_ts: Option<u64>,
    pending_t0: Option<Instant>,
    bound: Option<(u64, Instant)>,
    samples_ms: Vec<f64>,
    evidence_written: bool,
    verdict: Option<Verdict>,
    quit: bool,
}

#[derive(Debug, Clone, Serialize)]
struct Evidence {
    gate: &'static str,
    date: String,
    binary: String,
    git_sha: String,
    git_dirty: Option<bool>,
    log: String,
    n: u32,
    seed: u64,
    first_ts: u64,
    last_ts: u64,
    samples_ms: Vec<f64>,
    p50_ms: f64,
    p95_ms: f64,
    p99_ms: f64,
    max_ms: f64,
    min_ms: f64,
    mean_ms: f64,
    budget_p95_ms: f64,
    methodology: &'static str,
    verdict: Verdict,
}

/// Validate CLI mutual requirements. Returns `(n, out)` or a usage message.
pub fn validate_cli(
    gate: Option<u32>,
    out: Option<PathBuf>,
    startup_trace: bool,
    frame_gate: bool,
    is_replay: bool,
) -> Result<Option<(u32, PathBuf)>, String> {
    match (gate, out) {
        (None, None) => Ok(None),
        (Some(_), None) => Err("--scrub-latency-gate requires --scrub-latency-out".into()),
        (None, Some(_)) => Err("--scrub-latency-out requires --scrub-latency-gate".into()),
        (Some(n), Some(path)) => {
            if n == 0 {
                return Err("--scrub-latency-gate requires N ≥ 1".into());
            }
            if startup_trace {
                return Err(
                    "--scrub-latency-gate is mutually exclusive with --startup-trace".into(),
                );
            }
            if frame_gate {
                return Err("--scrub-latency-gate is mutually exclusive with --gate".into());
            }
            if !is_replay {
                return Err("--scrub-latency-gate requires --replay (checkpointed log)".into());
            }
            Ok(Some((n, path)))
        }
    }
}

/// Arm the gate before the window opens. Panics if `n == 0` or the out path is not writable.
pub fn enable(n: u32, out_path: PathBuf, seed: u64, budget_ms: f64, log: PathBuf) {
    assert!(n > 0, "fft: --scrub-latency-gate requires N ≥ 1");
    assert!(
        budget_ms.is_finite() && budget_ms > 0.0,
        "fft: scrub-latency budget must be finite and > 0"
    );
    std::fs::File::create(&out_path).unwrap_or_else(|err| {
        panic!(
            "fft: cannot create --scrub-latency-out {}: {err}",
            out_path.display()
        )
    });
    let rng = if seed == 0 { 1 } else { seed };
    let fresh = State {
        n,
        out_path,
        seed,
        rng,
        budget_p95_ms: budget_ms,
        log,
        binary: command_line(std::env::args()),
        git: GitInfo::capture(),
        first_ts: None,
        last_ts: None,
        pending_t0: None,
        bound: None,
        samples_ms: Vec::with_capacity(n as usize),
        evidence_written: false,
        verdict: None,
        quit: false,
    };
    match STATE.get() {
        Some(cell) => {
            let mut guard = cell.lock().unwrap_or_else(|e| e.into_inner());
            *guard = fresh;
        }
        None => {
            let _ = STATE.set(Mutex::new(fresh));
        }
    }
    ENABLED.store(true, Ordering::Release);
}

#[inline]
pub fn enabled() -> bool {
    ENABLED.load(Ordering::Acquire)
}

fn with_state<R>(f: impl FnOnce(&mut State) -> R) -> R {
    let cell = STATE
        .get()
        .expect("fft: scrub_latency enabled without state");
    let mut guard = cell.lock().unwrap_or_else(|e| e.into_inner());
    f(&mut guard)
}

/// T0: scrub release with a dirty pending target.
#[inline]
pub fn note_release() {
    if !enabled() {
        return;
    }
    with_state(|s| {
        s.pending_t0 = Some(Instant::now());
    });
}

/// Bind the pending T0 to the Seek generation issued by `take_coalesced_seek`.
#[inline]
pub fn bind_generation(seek_gen: u64) {
    if !enabled() {
        return;
    }
    with_state(|s| {
        let Some(t0) = s.pending_t0.take() else {
            return;
        };
        s.bound = Some((seek_gen, t0));
    });
}

/// T1: snapshot with matching `seek_generation` adopted for paint.
#[inline]
pub fn note_rendered(seek_generation: u64) {
    if !enabled() {
        return;
    }
    with_state(|s| {
        let Some((bound_gen, t0)) = s.bound else {
            return;
        };
        if bound_gen != seek_generation {
            return;
        }
        let ms = round3(t0.elapsed().as_secs_f64() * 1e3);
        s.bound = None;
        s.samples_ms.push(ms);
    });
}

#[inline]
pub fn complete() -> bool {
    enabled() && with_state(|s| s.complete())
}

#[inline]
pub fn should_quit() -> bool {
    enabled() && with_state(|s| s.quit)
}

/// Process exit code after evidence is written (`None` if the gate was never armed).
pub fn exit_failure() -> Option<bool> {
    if !enabled() {
        return None;
    }
    Some(with_state(|s| s.verdict != Some(Verdict::Pass)))
}

/// Next scripted scrub target, or `None` while a sample is in flight / complete / range unknown.
pub fn next_script_target(first_ts: u64, last_ts: u64) -> Option<u64> {
    if !enabled() {
        return None;
    }
    with_state(|s| {
        if s.complete() || s.quit || s.pending_t0.is_some() || s.bound.is_some() {
            return None;
        }
        if first_ts == 0 && last_ts <= 1 {
            return None;
        }
        s.first_ts.get_or_insert(first_ts);
        s.last_ts.get_or_insert(last_ts);
        let (lo, hi) = if first_ts <= last_ts {
            (first_ts, last_ts)
        } else {
            (last_ts, first_ts)
        };
        Some(map_rng_to_range(&mut s.rng, lo, hi))
    })
}

/// Write evidence JSON and return the verdict. Idempotent after the first write.
pub fn write_evidence_and_verdict() -> Verdict {
    assert!(enabled(), "fft: scrub-latency write without enable");
    with_state(|s| {
        if let Some(v) = s.verdict {
            s.quit = true;
            return v;
        }
        let verdict = decide_verdict(s);
        let evidence = build_evidence(s, verdict);
        write_evidence(&s.out_path, &evidence);
        s.evidence_written = true;
        s.verdict = Some(verdict);
        s.quit = true;
        eprintln!(
            "fft: scrub-latency n={} collected={} p95_ms={:.3} budget={:.1} verdict={verdict:?}",
            evidence.n,
            evidence.samples_ms.len(),
            evidence.p95_ms,
            evidence.budget_p95_ms
        );
        verdict
    })
}

/// Fail closed (wedge / incomplete) and request quit. Safe to call once.
pub fn fail_and_quit(reason: &str) {
    if !enabled() {
        return;
    }
    eprintln!("fft: scrub-latency FAIL: {reason}");
    with_state(|s| {
        if s.verdict.is_some() {
            s.quit = true;
            return;
        }
        let verdict = Verdict::Fail;
        let evidence = build_evidence(s, verdict);
        write_evidence(&s.out_path, &evidence);
        s.evidence_written = true;
        s.verdict = Some(verdict);
        s.quit = true;
    });
}

/// Check wedge timeout on an in-flight bound sample.
pub fn check_wedge() {
    if !enabled() {
        return;
    }
    let wedged = with_state(|s| {
        s.bound
            .map(|(_, t0)| t0.elapsed().as_secs_f64() * 1e3 > WEDGE_TIMEOUT_MS)
            .unwrap_or(false)
    });
    if wedged {
        fail_and_quit(&format!(
            "engine wedge: no matching seek_generation within {WEDGE_TIMEOUT_MS} ms"
        ));
    }
}

/// After T1 + `take_coalesced_seek`: wedge-check, finish, or script the next release.
///
/// Returns true when the shell should quit (evidence written).
pub fn drive_script_if_needed(
    first_ts: u64,
    last_ts: u64,
    script_release: impl FnOnce(u64),
) -> bool {
    if !enabled() {
        return false;
    }
    check_wedge();
    if should_quit() {
        return true;
    }
    if complete() {
        let _ = write_evidence_and_verdict();
        return true;
    }
    if let Some(ts) = next_script_target(first_ts, last_ts) {
        script_release(ts);
    }
    should_quit()
}

impl State {
    fn complete(&self) -> bool {
        self.samples_ms.len() as u32 >= self.n
    }
}

fn decide_verdict(s: &State) -> Verdict {
    if !s.complete() {
        return Verdict::Fail;
    }
    let p95 = percentile_nearest_rank(&s.samples_ms, 0.95);
    if p95 <= s.budget_p95_ms {
        Verdict::Pass
    } else {
        Verdict::Fail
    }
}

fn build_evidence(s: &State, verdict: Verdict) -> Evidence {
    let samples = &s.samples_ms;
    let (min_ms, max_ms, mean_ms) = if samples.is_empty() {
        (0.0, 0.0, 0.0)
    } else {
        let min = samples.iter().copied().fold(f64::INFINITY, f64::min);
        let max = samples.iter().copied().fold(f64::NEG_INFINITY, f64::max);
        let mean = samples.iter().sum::<f64>() / samples.len() as f64;
        (round3(min), round3(max), round3(mean))
    };
    Evidence {
        gate: "claim1-scrub-release-to-rendered",
        date: now_rfc3339_utc(),
        binary: s.binary.clone(),
        git_sha: s.git.sha.clone(),
        git_dirty: s.git.dirty,
        log: s.log.display().to_string(),
        n: s.n,
        seed: s.seed,
        first_ts: s.first_ts.unwrap_or(0),
        last_ts: s.last_ts.unwrap_or(0),
        samples_ms: samples.clone(),
        p50_ms: percentile_nearest_rank(samples, 0.50),
        p95_ms: percentile_nearest_rank(samples, 0.95),
        p99_ms: percentile_nearest_rank(samples, 0.99),
        max_ms,
        min_ms,
        mean_ms,
        budget_p95_ms: s.budget_p95_ms,
        methodology: METHODOLOGY,
        verdict,
    }
}

fn write_evidence(path: &Path, evidence: &Evidence) {
    let json = serde_json::to_string_pretty(evidence)
        .unwrap_or_else(|err| panic!("fft: scrub-latency serialize: {err}"));
    std::fs::write(path, format!("{json}\n"))
        .unwrap_or_else(|err| panic!("fft: scrub-latency write {}: {err}", path.display()));
    eprintln!("fft: scrub-latency evidence written to {}", path.display());
}

/// Nearest-rank percentile (inclusive, ceil(p·N)). Empty → 0.
fn percentile_nearest_rank(samples: &[f64], p: f64) -> f64 {
    assert!((0.0..=1.0).contains(&p), "percentile out of range: {p}");
    if samples.is_empty() {
        return 0.0;
    }
    let mut v = samples.to_vec();
    v.sort_by(|a, b| a.total_cmp(b));
    let n = v.len();
    let rank = ((p * n as f64).ceil() as usize).max(1).min(n);
    round3(v[rank - 1])
}

/// xorshift64* step, then map into inclusive `[lo, hi]`.
fn map_rng_to_range(rng: &mut u64, lo: u64, hi: u64) -> u64 {
    *rng ^= *rng << 13;
    *rng ^= *rng >> 7;
    *rng ^= *rng << 17;
    if hi <= lo {
        return lo;
    }
    let span = hi - lo;
    lo + (*rng % (span + 1))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    static TEST_LOCK: Mutex<()> = Mutex::new(());

    fn reset_for_test(n: u32, out: PathBuf) {
        enable(
            n,
            out,
            DEFAULT_SEED,
            BUDGET_P95_MS,
            PathBuf::from("/tmp/scrub-latency-test.fftlog"),
        );
        with_state(|s| {
            s.pending_t0 = None;
            s.bound = None;
            s.samples_ms.clear();
            s.evidence_written = false;
            s.verdict = None;
            s.quit = false;
            s.first_ts = None;
            s.last_ts = None;
            s.rng = if s.seed == 0 { 1 } else { s.seed };
        });
    }

    #[test]
    fn note_release_bind_rendered_records_one_sample() {
        let _g = TEST_LOCK.lock().unwrap();
        let out = std::env::temp_dir().join("scrub-latency-unit-one.json");
        reset_for_test(1, out);
        note_release();
        bind_generation(7);
        note_rendered(7);
        assert!(complete());
        assert_eq!(with_state(|s| s.samples_ms.len()), 1);
    }

    #[test]
    fn mismatched_generation_does_not_complete() {
        let _g = TEST_LOCK.lock().unwrap();
        let out = std::env::temp_dir().join("scrub-latency-unit-mismatch.json");
        reset_for_test(1, out);
        note_release();
        bind_generation(7);
        note_rendered(8);
        assert!(!complete());
        assert!(with_state(|s| s.samples_ms.is_empty()));
        note_rendered(7);
        assert!(complete());
    }

    #[test]
    fn p95_math_on_known_vector() {
        let samples: Vec<f64> = (1..=20).map(|i| i as f64).collect();
        assert_eq!(percentile_nearest_rank(&samples, 0.95), 19.0);
        assert_eq!(percentile_nearest_rank(&samples, 0.50), 10.0);
        assert_eq!(percentile_nearest_rank(&samples, 0.99), 20.0);
    }

    #[test]
    fn next_script_target_maps_into_range() {
        let _g = TEST_LOCK.lock().unwrap();
        let out = std::env::temp_dir().join("scrub-latency-unit-range.json");
        reset_for_test(3, out);
        let first = 1_000u64;
        let last = 2_000u64;
        for i in 0..3 {
            let ts = next_script_target(first, last).expect("target");
            assert!((first..=last).contains(&ts));
            note_release();
            bind_generation(10 + i);
            note_rendered(10 + i);
        }
        assert!(complete());
        assert!(next_script_target(first, last).is_none());
    }
}
