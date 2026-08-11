//! Shared helpers: /proc RSS, log bounds, command send.

use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::{Duration, Instant};

use fft_engine::{EngineCmd, EngineHandle};
use fft_log::{KIND_EVENTS, LogReader};

/// PRD / M7 RSS ceiling (bytes).
pub const RSS_BUDGET_BYTES: u64 = 2 * 1024 * 1024 * 1024;
pub const FIRST_GEN: u64 = 2;
pub const PRIOR_TIMEOUT: Duration = Duration::from_secs(180);
pub const SCRUB_SETTLE: Duration = Duration::from_secs(15);
pub const EOF_STABLE: Duration = Duration::from_millis(200);
pub const POLL: Duration = Duration::from_millis(2);

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

/// Load priors oldest-first; wait for each session insert (or timeout).
/// Later-date priors are issued (ENGINE.md §2 skip path) but waited only briefly.
/// Timed cycles cap prior wait so scrub/transport still run under short smoke caps.
pub fn load_priors(
    handle: &EngineHandle,
    wake: &AtomicBool,
    priors: &[std::path::PathBuf],
    current_trade_date: Option<u32>,
    deadline: Instant,
    cycle_secs: u64,
) -> Result<usize, String> {
    let snapshots = handle.snapshots();
    let expected_loads = priors
        .iter()
        .filter(|p| match (current_trade_date, log_trade_date(p)) {
            (Some(cur), Some(pd)) => pd < cur,
            _ => true,
        })
        .count()
        .max(1);
    let load_pool = if cycle_secs > 0 {
        Duration::from_secs((cycle_secs * 2 / 5).max(4))
    } else {
        PRIOR_TIMEOUT.saturating_mul(expected_loads as u32)
    };
    let per_load = if cycle_secs > 0 {
        (load_pool / expected_loads as u32).max(Duration::from_secs(2))
    } else {
        PRIOR_TIMEOUT
    };

    for (i, prior) in priors.iter().enumerate() {
        if Instant::now() >= deadline {
            eprintln!(
                "m7-soak: prior load cut by cycle deadline at {}/{}",
                i + 1,
                priors.len()
            );
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
        loop {
            if Instant::now() >= deadline || start.elapsed() > wait_budget {
                if !expect_skip {
                    eprintln!(
                        "m7-soak: prior {}/{} {} not complete in {:.1}s (sessions={before})",
                        i + 1,
                        priors.len(),
                        prior.display(),
                        start.elapsed().as_secs_f64()
                    );
                }
                break;
            }
            let _ = wake.swap(false, Ordering::AcqRel);
            if snapshots.load().profile.sessions.len() >= target {
                break;
            }
            thread::sleep(POLL);
        }
    }
    Ok(snapshots.load().profile.sessions.len())
}

/// Scrub burst: N seeks sweeping session bounds (m5-scrub-burst pattern).
pub fn scrub_burst(
    handle: &EngineHandle,
    wake: &AtomicBool,
    first_ts: u64,
    last_ts: u64,
    n: u32,
    gen_base: &mut u64,
) -> Result<u64, String> {
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
    let mut last = 0u64;
    while settle.elapsed() < SCRUB_SETTLE {
        let _ = wake.swap(false, Ordering::AcqRel);
        let g = snapshots.load().seek_generation;
        if g > last {
            last = g;
        }
        if last >= want {
            break;
        }
        thread::sleep(POLL);
    }
    Ok(u64::from(n))
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

/// Forward play until EOF-stable or wall deadline; poll snapshots continuously.
pub fn play_until(
    handle: &EngineHandle,
    wake: &AtomicBool,
    deadline: Instant,
    peak_rss_kb: &mut u64,
    peak_hwm_kb: &mut u64,
) {
    let snapshots = handle.snapshots();
    let mut stable_since: Option<Instant> = None;
    let mut last_applied = 0u64;
    while Instant::now() < deadline {
        let (hwm, rss) = read_vm_status();
        *peak_hwm_kb = (*peak_hwm_kb).max(hwm);
        *peak_rss_kb = (*peak_rss_kb).max(rss);
        if rss * 1024 > RSS_BUDGET_BYTES {
            eprintln!(
                "m7-soak: FAIL RSS CEILING — VmRSS={rss} kB ({:.2} MiB) > 2 GiB",
                rss as f64 / 1024.0
            );
        }
        let _ = wake.swap(false, Ordering::AcqRel);
        let snap = snapshots.load();
        let applied = snap.coverage.events_applied;
        if applied > last_applied {
            last_applied = applied;
            stable_since = None;
        } else if applied > 0 {
            match stable_since {
                Some(t) if t.elapsed() >= EOF_STABLE => return,
                Some(_) => {}
                None => stable_since = Some(Instant::now()),
            }
        }
        thread::sleep(POLL);
    }
}
