//! Shared helpers: /proc RSS, log bounds, prior load, scrub, transport, heartbeats.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::{Duration, Instant};

use fft_engine::{EngineCmd, EngineHandle};
use fft_log::{KIND_EVENTS, LogReader};

pub use crate::heartbeat::HeartbeatCtx;
use crate::heartbeat::emit_heartbeat;

pub const RSS_BUDGET_BYTES: u64 = 2 * 1024 * 1024 * 1024;
pub const FIRST_GEN: u64 = 2;
pub const PRIOR_TIMEOUT: Duration = Duration::from_secs(180);
pub const SCRUB_SETTLE: Duration = Duration::from_secs(15);
pub const EOF_STABLE: Duration = Duration::from_millis(200);
pub const POLL: Duration = Duration::from_millis(2);
pub const CURRENT_READY_TIMEOUT: Duration = Duration::from_secs(60);

pub struct PeakRss<'a> {
    pub rss_kb: &'a mut u64,
    pub hwm_kb: &'a mut u64,
}

impl PeakRss<'_> {
    fn sample(&mut self) -> u64 {
        let (hwm, rss) = read_vm_status();
        *self.hwm_kb = (*self.hwm_kb).max(hwm);
        *self.rss_kb = (*self.rss_kb).max(rss);
        rss
    }
}

/// `(session_span_ns / speed) × 2 + 120` seconds.
pub fn eof_cycle_deadline_secs(first_ts: u64, last_ts: u64, speed: f64) -> u64 {
    let span_secs = (last_ts.saturating_sub(first_ts) as f64) / 1_000_000_000.0;
    let secs = (span_secs / speed) * 2.0 + 120.0;
    secs.ceil().max(1.0) as u64
}

pub fn read_vm_status() -> (u64, u64) {
    let text = std::fs::read_to_string("/proc/self/status").unwrap_or_else(|e| {
        panic!("m7-soak: read /proc/self/status: {e} (Linux-only probe)");
    });
    let mut hwm = None;
    let mut rss = None;
    for line in text.lines() {
        if let Some(rest) = line.strip_prefix("VmHWM:") {
            hwm = Some(parse_kb(rest, "VmHWM"));
        } else if let Some(rest) = line.strip_prefix("VmRSS:") {
            rss = Some(parse_kb(rest, "VmRSS"));
        }
    }
    (
        hwm.expect("m7-soak: VmHWM missing from /proc/self/status"),
        rss.expect("m7-soak: VmRSS missing from /proc/self/status"),
    )
}

fn parse_kb(rest: &str, name: &str) -> u64 {
    rest.split_whitespace()
        .next()
        .unwrap_or_else(|| panic!("m7-soak: empty {name}"))
        .parse::<u64>()
        .unwrap_or_else(|_| panic!("m7-soak: bad {name}: {rest}"))
}

pub fn event_time_bounds(path: &Path) -> (u64, u64) {
    let (reader, report) = LogReader::open(path).unwrap_or_else(|e| {
        panic!("m7-soak: open {}: {e}", path.display());
    });
    for w in &report.warnings {
        eprintln!("m7-soak: open warning: {w}");
    }
    let mut first_ts = None;
    let mut last_ts = 0u64;
    let mut ckpts = 0usize;
    for i in 0..reader.frame_count() {
        let fh = reader
            .frame_header(i)
            .unwrap_or_else(|e| panic!("m7-soak: frame_header({i}): {e}"));
        if fh.kind == KIND_EVENTS {
            if first_ts.is_none() {
                first_ts = Some(fh.first_ts);
            }
            last_ts = fh.last_ts;
        } else if fh.kind == fft_log::KIND_CHECKPOINT {
            ckpts += 1;
        }
    }
    let first_ts =
        first_ts.unwrap_or_else(|| panic!("m7-soak: no EVENTS frames in {}", path.display()));
    if ckpts == 0 {
        panic!(
            "m7-soak: zero checkpoints in {} — run fft-checkpoint first",
            path.display()
        );
    }
    (first_ts, last_ts)
}

pub fn sweep_targets(first: u64, last: u64, n: u32) -> Vec<u64> {
    if n == 1 {
        return vec![first];
    }
    let span = last.saturating_sub(first);
    (0..n)
        .map(|i| {
            let num = u128::from(span) * u128::from(i);
            let den = u128::from(n - 1);
            first + (num / den) as u64
        })
        .collect()
}

pub fn panic_message(payload: &(dyn std::any::Any + Send)) -> String {
    payload
        .downcast_ref::<&str>()
        .map(|msg| (*msg).to_string())
        .or_else(|| payload.downcast_ref::<String>().cloned())
        .unwrap_or_else(|| "non-string panic payload".into())
}

pub fn send(handle: &EngineHandle, cmd: EngineCmd, what: &str) -> Result<(), String> {
    handle
        .send(cmd)
        .map_err(|_| format!("engine stopped during {what}"))
}

pub fn log_trade_date(path: &Path) -> Option<u32> {
    LogReader::open(path).ok().map(|(r, _)| r.meta().trade_date)
}

pub fn expected_prior_sets(priors: &[PathBuf], current_trade_date: Option<u32>) -> (u64, u64) {
    let mut accepted = 0u64;
    let mut skips = 0u64;
    for p in priors {
        match (current_trade_date, log_trade_date(p)) {
            (Some(cur), Some(pd)) if pd >= cur => skips += 1,
            _ => accepted += 1,
        }
    }
    (accepted, skips)
}

#[derive(Debug, Clone)]
pub struct PriorLoadOutcome {
    pub sessions: usize,
    pub expected_accepted: u64,
    pub expected_skips: u64,
    pub accepted_seen: u64,
    pub incomplete: Vec<String>,
}

pub fn load_priors(
    handle: &EngineHandle,
    wake: &AtomicBool,
    priors: &[PathBuf],
    current_trade_date: Option<u32>,
    deadline: Instant,
    cycle_secs: u64,
) -> Result<PriorLoadOutcome, String> {
    let snapshots = handle.snapshots();
    let (expected_accepted, expected_skips) = expected_prior_sets(priors, current_trade_date);
    let accept_n = expected_accepted.max(1) as u32;
    let load_pool = if cycle_secs > 0 {
        Duration::from_secs((cycle_secs * 2 / 5).max(4))
    } else {
        PRIOR_TIMEOUT.saturating_mul(accept_n)
    };
    let per_load = if cycle_secs > 0 {
        (load_pool / accept_n).max(Duration::from_secs(2))
    } else {
        PRIOR_TIMEOUT
    };
    let mut accepted_seen = 0u64;
    let mut incomplete = Vec::new();
    for (i, prior) in priors.iter().enumerate() {
        if Instant::now() >= deadline {
            eprintln!(
                "m7-soak: prior load cut by cycle deadline at {}/{}",
                i + 1,
                priors.len()
            );
            for rest in &priors[i..] {
                let expect_skip = match (current_trade_date, log_trade_date(rest)) {
                    (Some(cur), Some(p)) => p >= cur,
                    _ => false,
                };
                if !expect_skip {
                    incomplete.push(format!(
                        "deadline before LoadPriorSession {}",
                        rest.display()
                    ));
                }
            }
            break;
        }
        let before = snapshots.load().profile.sessions.len();
        let target = before + 1;
        let prior_date = log_trade_date(prior);
        let expect_skip = match (current_trade_date, prior_date) {
            (Some(cur), Some(p)) => p >= cur,
            _ => false,
        };
        let wait_budget = if expect_skip {
            Duration::from_secs(2)
        } else {
            per_load
        };
        send(
            handle,
            EngineCmd::LoadPriorSession {
                path: prior.clone(),
            },
            "LoadPriorSession",
        )?;
        let start = Instant::now();
        let mut inserted = false;
        loop {
            if Instant::now() >= deadline || start.elapsed() > wait_budget {
                break;
            }
            let _ = wake.swap(false, Ordering::AcqRel);
            if snapshots.load().profile.sessions.len() >= target {
                inserted = true;
                break;
            }
            thread::sleep(POLL);
        }
        if expect_skip {
            if inserted {
                incomplete.push(format!(
                    "expected skip but session inserted for {}",
                    prior.display()
                ));
            }
        } else if inserted {
            accepted_seen += 1;
        } else {
            let msg = format!(
                "prior {}/{} {} not complete in {:.1}s (sessions={before})",
                i + 1,
                priors.len(),
                prior.display(),
                start.elapsed().as_secs_f64()
            );
            eprintln!("m7-soak: {msg}");
            incomplete.push(msg);
        }
    }
    Ok(PriorLoadOutcome {
        sessions: snapshots.load().profile.sessions.len(),
        expected_accepted,
        expected_skips,
        accepted_seen,
        incomplete,
    })
}

/// SetSource does not publish; force a Seek so readiness is observable.
pub fn wait_current_ready(
    handle: &EngineHandle,
    wake: &AtomicBool,
    first_ts: u64,
    gen_base: &mut u64,
    hb: &HeartbeatCtx<'_>,
    peaks: &mut PeakRss<'_>,
) -> Result<(), String> {
    let generation = *gen_base;
    *gen_base += 1;
    send(
        handle,
        EngineCmd::Seek {
            ts: first_ts,
            generation,
        },
        "ready Seek",
    )?;
    let snapshots = handle.snapshots();
    let deadline = Instant::now() + CURRENT_READY_TIMEOUT;
    let phase_start = Instant::now();
    let mut last_hb = Instant::now();
    loop {
        if Instant::now() >= deadline {
            return Err(format!(
                "timeout waiting for current session ready ({:.0}s)",
                CURRENT_READY_TIMEOUT.as_secs_f64()
            ));
        }
        let _rss = peaks.sample();
        let _ = wake.swap(false, Ordering::AcqRel);
        let snap = snapshots.load();
        emit_heartbeat(
            hb,
            phase_start,
            &mut last_hb,
            snap.coverage.events_applied,
            snap.applied_ts,
        );
        if snap.seek_generation >= generation
            && snap.generation > 0
            && !snap.profile.sessions.is_empty()
        {
            return Ok(());
        }
        thread::sleep(POLL);
    }
}

pub fn scrub_burst(
    handle: &EngineHandle,
    wake: &AtomicBool,
    first_ts: u64,
    last_ts: u64,
    n: u32,
    gen_base: &mut u64,
    hb: &HeartbeatCtx<'_>,
) -> Result<(u64, bool), String> {
    send(handle, EngineCmd::Pause, "scrub Pause")?;
    let targets = sweep_targets(first_ts, last_ts, n);
    let interval = Duration::from_millis(16);
    for ts in targets {
        let generation = *gen_base;
        *gen_base += 1;
        send(handle, EngineCmd::Seek { ts, generation }, "Seek")?;
        thread::sleep(interval);
    }
    let want = *gen_base - 1;
    let snapshots = handle.snapshots();
    let settle = Instant::now();
    let mut last_hb = Instant::now();
    let mut last = 0u64;
    while settle.elapsed() < SCRUB_SETTLE {
        let _ = wake.swap(false, Ordering::AcqRel);
        let snap = snapshots.load();
        let g = snap.seek_generation;
        if g > last {
            last = g;
        }
        emit_heartbeat(
            hb,
            settle,
            &mut last_hb,
            snap.coverage.events_applied,
            snap.applied_ts,
        );
        if last >= want {
            return Ok((u64::from(n), true));
        }
        thread::sleep(POLL);
    }
    Err(format!(
        "scrub final generation unanswered: want={want} got={last} after {:.1}s",
        SCRUB_SETTLE.as_secs_f64()
    ))
}

pub fn speed_and_transport(handle: &EngineHandle) -> Result<(), String> {
    for s in [1.0, 4.0, 16.0, 64.0] {
        send(handle, EngineCmd::SetSpeed(s), "SetSpeed")?;
        thread::sleep(Duration::from_millis(50));
    }
    send(handle, EngineCmd::Pause, "transport Pause")?;
    thread::sleep(Duration::from_millis(50));
    send(handle, EngineCmd::Play, "transport Play")?;
    thread::sleep(Duration::from_millis(50));
    send(handle, EngineCmd::Pause, "transport Pause2")?;
    thread::sleep(Duration::from_millis(50));
    send(handle, EngineCmd::Play, "transport Play2")?;
    Ok(())
}

/// EOF without requiring events_applied>0: applied unchanged for EOF_STABLE AND
/// (events_applied>0 OR applied_ts>=last_ts).
pub fn play_until(
    handle: &EngineHandle,
    wake: &AtomicBool,
    deadline: Instant,
    last_ts: u64,
    hb: &HeartbeatCtx<'_>,
    peaks: &mut PeakRss<'_>,
) {
    let snapshots = handle.snapshots();
    let phase_start = Instant::now();
    let mut last_hb = Instant::now();
    let mut stable_since: Option<Instant> = None;
    let mut last_applied = 0u64;
    let mut seeded = false;
    while Instant::now() < deadline {
        let rss = peaks.sample();
        if rss * 1024 > RSS_BUDGET_BYTES {
            eprintln!(
                "m7-soak: FAIL RSS CEILING — VmRSS={rss} kB ({:.2} MiB) > 2 GiB",
                rss as f64 / 1024.0
            );
        }
        let _ = wake.swap(false, Ordering::AcqRel);
        let snap = snapshots.load();
        let applied = snap.coverage.events_applied;
        let applied_ts = snap.applied_ts;
        emit_heartbeat(hb, phase_start, &mut last_hb, applied, applied_ts);
        if !seeded {
            last_applied = applied;
            seeded = true;
            if applied_ts >= last_ts {
                stable_since = Some(Instant::now());
            }
        } else if applied > last_applied {
            last_applied = applied;
            stable_since = None;
        } else if applied > 0 || applied_ts >= last_ts {
            match stable_since {
                Some(t) if t.elapsed() >= EOF_STABLE => return,
                Some(_) => {}
                None => stable_since = Some(Instant::now()),
            }
        }
        thread::sleep(POLL);
    }
}

pub fn observe_retention(
    handle: &EngineHandle,
    wake: &AtomicBool,
    first_ts: u64,
    gen_base: &mut u64,
    deadline: Instant,
    hb: &HeartbeatCtx<'_>,
    peaks: &mut PeakRss<'_>,
) -> Result<usize, String> {
    let generation = *gen_base;
    *gen_base += 1;
    send(
        handle,
        EngineCmd::Seek {
            ts: first_ts,
            generation,
        },
        "retention Seek",
    )?;
    let snapshots = handle.snapshots();
    let start = Instant::now();
    let mut last_hb = Instant::now();
    loop {
        if Instant::now() >= deadline || start.elapsed() > SCRUB_SETTLE {
            return Err(format!(
                "retention observe: seek gen {generation} unanswered"
            ));
        }
        let _rss = peaks.sample();
        let _ = wake.swap(false, Ordering::AcqRel);
        let snap = snapshots.load();
        emit_heartbeat(
            hb,
            start,
            &mut last_hb,
            snap.coverage.events_applied,
            snap.applied_ts,
        );
        if snap.seek_generation >= generation && !snap.profile.sessions.is_empty() {
            return Ok(snap.profile.sessions.len());
        }
        thread::sleep(POLL);
    }
}
