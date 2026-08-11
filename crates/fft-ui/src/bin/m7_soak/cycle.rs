//! One soak cycle: spawn engine, exercise product paths, record metrics.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use fft_engine::{EngineCmd, EngineConfig, EngineExit, EngineService, Source};

use crate::args::Args;
use crate::metrics::CycleLine;
use crate::util::{
    FIRST_GEN, RSS_BUDGET_BYTES, load_priors, log_trade_date, panic_message, play_until,
    read_vm_status, scrub_burst, send, speed_and_transport,
};

pub struct CycleResult {
    pub line: CycleLine,
    pub peak_rss_kb: u64,
    pub peak_hwm_kb: u64,
}

pub fn run_cycle(cycle: u64, args: &Args, first_ts: u64, last_ts: u64) -> CycleResult {
    // Fresh engine each cycle ⇒ seek generations restart at FIRST_GEN.
    let mut gen_base = FIRST_GEN;
    let started = Instant::now();
    let mut peak_rss_kb = 0u64;
    let mut peak_hwm_kb = 0u64;
    let mut notes = Vec::new();

    let wake_flag = Arc::new(AtomicBool::new(false));
    let wake = Arc::clone(&wake_flag);
    let handle = match EngineService::spawn(
        EngineConfig {
            visible_tick_span: 256,
        },
        Box::new(move || {
            wake.store(true, Ordering::Release);
        }),
    ) {
        Ok(h) => h,
        Err(e) => {
            return fail_cycle(
                cycle,
                args,
                started,
                peak_rss_kb,
                peak_hwm_kb,
                Some(format!("spawn: {e}")),
                None,
            );
        }
    };

    let cycle_deadline = if args.cycle_secs > 0 {
        started + Duration::from_secs(args.cycle_secs)
    } else {
        started + Duration::from_secs(86_400)
    };

    let mut sessions_after_priors = 0usize;
    let mut re_sessions = 0usize;
    let mut seeks_issued = 0u64;
    let mut cycle_err: Option<String> = None;

    if let Err(e) = send(
        &handle,
        EngineCmd::SetSource(Source::Replay {
            path: args.replay.clone(),
        }),
        "SetSource",
    ) {
        cycle_err = Some(e);
    }

    if cycle_err.is_none() {
        let snapshots = handle.snapshots();
        let wait_current = Instant::now();
        loop {
            if Instant::now() >= cycle_deadline {
                cycle_err = Some("timeout waiting for current session".into());
                break;
            }
            let (hwm, rss) = read_vm_status();
            peak_hwm_kb = peak_hwm_kb.max(hwm);
            peak_rss_kb = peak_rss_kb.max(rss);
            let _ = wake_flag.swap(false, Ordering::AcqRel);
            let snap = snapshots.load();
            if snap.generation > 0 || wait_current.elapsed() > Duration::from_millis(100) {
                break;
            }
            std::thread::sleep(crate::util::POLL);
        }
    }

    let current_trade_date = log_trade_date(&args.replay);

    if cycle_err.is_none()
        && let Err(e) = send(&handle, EngineCmd::Pause, "pre-prior Pause")
    {
        cycle_err = Some(e);
    }
    if cycle_err.is_none() {
        match load_priors(
            &handle,
            &wake_flag,
            &args.priors,
            current_trade_date,
            cycle_deadline,
            args.cycle_secs,
        ) {
            Ok(n) => sessions_after_priors = n,
            Err(e) => cycle_err = Some(e),
        }
    }

    if cycle_err.is_none()
        && let Err(e) = send(&handle, EngineCmd::SetSpeed(args.speed), "SetSpeed")
    {
        cycle_err = Some(e);
    }
    if cycle_err.is_none()
        && let Err(e) = send(&handle, EngineCmd::Play, "Play")
    {
        cycle_err = Some(e);
    }

    if cycle_err.is_none() {
        let mid = Instant::now()
            + Duration::from_secs(args.cycle_secs.max(1) / 3).min(Duration::from_secs(10));
        play_until(
            &handle,
            &wake_flag,
            mid.min(cycle_deadline),
            &mut peak_rss_kb,
            &mut peak_hwm_kb,
        );
    }

    if cycle_err.is_none() {
        match scrub_burst(
            &handle,
            &wake_flag,
            first_ts,
            last_ts,
            args.scrub_seeks,
            &mut gen_base,
        ) {
            Ok(n) => seeks_issued = n,
            Err(e) => cycle_err = Some(e),
        }
    }
    if cycle_err.is_none()
        && let Err(e) = speed_and_transport(&handle)
    {
        cycle_err = Some(e);
    }
    if cycle_err.is_none()
        && let Err(e) = send(&handle, EngineCmd::SetSpeed(args.speed), "restore speed")
    {
        cycle_err = Some(e);
    }
    if cycle_err.is_none()
        && let Err(e) = send(&handle, EngineCmd::Play, "resume Play")
    {
        cycle_err = Some(e);
    }

    if cycle_err.is_none() {
        play_until(
            &handle,
            &wake_flag,
            cycle_deadline,
            &mut peak_rss_kb,
            &mut peak_hwm_kb,
        );
    }

    // Coverage before re-SetSource (ENGINE.md §3: SetSource zeroes counters).
    let mut pre_reset_applied = 0u64;
    let mut pre_reset_read = 0u64;
    let mut pre_reset_gaps = 0u64;
    let mut pre_reset_pubs = 0u64;
    if cycle_err.is_none() {
        let snap = handle.snapshots().load();
        pre_reset_applied = snap.coverage.events_applied;
        pre_reset_read = snap.coverage.events_read;
        pre_reset_gaps = snap.coverage.gap_records;
        pre_reset_pubs = snap.generation;
        // Same-log SetSource exercises ENGINE.md §2 r4 retention. No Seek after:
        // checkpoint restore would rebuild PROFILE from the current-day checkpoint
        // and wipe retained priors in the published slot. re_set_source_sessions =
        // pre-reset session count (what should be retained).
        re_sessions = snap.profile.sessions.len();
        if let Err(e) = send(
            &handle,
            EngineCmd::SetSource(Source::Replay {
                path: args.replay.clone(),
            }),
            "SetSource re",
        ) {
            cycle_err = Some(e);
        } else {
            notes.push(format!(
                "re-SetSource issued; pre-reset sessions={re_sessions} (slot unpublished)"
            ));
        }
    }

    let shutdown = handle.shutdown();
    let (hwm, rss) = read_vm_status();
    peak_hwm_kb = peak_hwm_kb.max(hwm);
    peak_rss_kb = peak_rss_kb.max(rss);

    match (shutdown, cycle_err) {
        (Ok(exit), None) => {
            if rss * 1024 > RSS_BUDGET_BYTES || peak_rss_kb * 1024 > RSS_BUDGET_BYTES {
                notes.push("RSS_CEILING_EXCEEDED".into());
            }
            if sessions_after_priors == 0 && exit.priors_completed > 0 {
                sessions_after_priors = (exit.priors_completed as usize) + 1;
            }
            CycleResult {
                line: CycleLine {
                    kind: "cycle",
                    cycle,
                    ok: true,
                    wall_secs: started.elapsed().as_secs_f64(),
                    events_applied: pre_reset_applied,
                    events_read: pre_reset_read,
                    gap_records: pre_reset_gaps,
                    publications: exit.publications.max(pre_reset_pubs),
                    seeks_executed: exit.seeks_executed,
                    seeks_issued,
                    priors_completed: exit.priors_completed,
                    prior_skips: exit.prior_skips,
                    sessions: sessions_after_priors,
                    re_set_source_sessions: re_sessions,
                    vm_rss_kb: rss,
                    vm_hwm_kb: hwm,
                    peak_rss_kb,
                    speed: args.speed,
                    cycle_secs_cap: args.cycle_secs,
                    panic: None,
                    notes: if notes.is_empty() {
                        None
                    } else {
                        Some(notes.join("; "))
                    },
                },
                peak_rss_kb,
                peak_hwm_kb,
            }
        }
        (Ok(exit), Some(msg)) => {
            eprintln!("m7-soak: CYCLE {cycle} FAIL: {msg}");
            fail_cycle(
                cycle,
                args,
                started,
                peak_rss_kb,
                peak_hwm_kb,
                Some(msg),
                Some(exit),
            )
        }
        (Err(payload), prior) => {
            let msg = format!("engine panic: {}", panic_message(&*payload));
            let combined = match prior {
                Some(p) => format!("{p}; {msg}"),
                None => msg,
            };
            eprintln!("m7-soak: CYCLE {cycle} FAIL: {combined}");
            fail_cycle(
                cycle,
                args,
                started,
                peak_rss_kb,
                peak_hwm_kb,
                Some(combined),
                None,
            )
        }
    }
}

fn fail_cycle(
    cycle: u64,
    args: &Args,
    started: Instant,
    peak_rss_kb: u64,
    peak_hwm_kb: u64,
    panic_msg: Option<String>,
    partial: Option<EngineExit>,
) -> CycleResult {
    let (hwm, rss) = read_vm_status();
    let peak_rss_kb = peak_rss_kb.max(rss);
    let peak_hwm_kb = peak_hwm_kb.max(hwm);
    CycleResult {
        line: CycleLine {
            kind: "cycle",
            cycle,
            ok: false,
            wall_secs: started.elapsed().as_secs_f64(),
            events_applied: partial
                .as_ref()
                .map(|e| e.coverage.events_applied)
                .unwrap_or(0),
            events_read: partial
                .as_ref()
                .map(|e| e.coverage.events_read)
                .unwrap_or(0),
            gap_records: partial
                .as_ref()
                .map(|e| e.coverage.gap_records)
                .unwrap_or(0),
            publications: partial.as_ref().map(|e| e.publications).unwrap_or(0),
            seeks_executed: partial.as_ref().map(|e| e.seeks_executed).unwrap_or(0),
            seeks_issued: 0,
            priors_completed: partial.as_ref().map(|e| e.priors_completed).unwrap_or(0),
            prior_skips: partial.as_ref().map(|e| e.prior_skips).unwrap_or(0),
            sessions: 0,
            re_set_source_sessions: 0,
            vm_rss_kb: rss,
            vm_hwm_kb: hwm,
            peak_rss_kb,
            speed: args.speed,
            cycle_secs_cap: args.cycle_secs,
            panic: panic_msg,
            notes: Some("cycle failure".into()),
        },
        peak_rss_kb,
        peak_hwm_kb,
    }
}
