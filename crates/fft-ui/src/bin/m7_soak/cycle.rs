//! One soak cycle: spawn engine, exercise product paths, record metrics.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use fft_engine::{EngineCmd, EngineConfig, EngineService, Source};

use crate::args::Args;
use crate::honesty::{FailCtx, Honesty, LineParts, fail_cycle, finalize_honesty, make_line};
use crate::util::{
    FIRST_GEN, HeartbeatCtx, POLL, PeakRss, RSS_BUDGET_BYTES, SCRUB_SETTLE,
    eof_cycle_deadline_secs, load_priors, log_trade_date, observe_retention, panic_message,
    play_until, read_vm_status, scrub_burst, send, speed_and_transport, wait_current_ready,
};

pub use crate::honesty::CycleResult;

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
            return fail_cycle(FailCtx {
                cycle,
                args,
                started,
                peak_rss_kb,
                peak_hwm_kb,
                panic_msg: Some(format!("spawn: {e}")),
                partial: None,
                honesty: Honesty::default(),
            });
        }
    };

    // cycle_secs=0 ⇒ EOF-driven finite cap: (session_span_ns / speed)×2 + 120 s.
    let cycle_deadline = if args.cycle_secs > 0 {
        started + Duration::from_secs(args.cycle_secs)
    } else {
        let secs = eof_cycle_deadline_secs(first_ts, last_ts, args.speed);
        eprintln!("m7-soak: cycle {cycle} EOF deadline={secs}s ((session_span_ns/speed)×2 + 120)");
        started + Duration::from_secs(secs)
    };

    let mut honesty = Honesty::default();
    let mut sessions_after_priors = 0usize;
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
        let hb = HeartbeatCtx {
            out: &args.out,
            cycle,
            phase: "wait_current_ready",
        };
        let mut peaks = PeakRss {
            rss_kb: &mut peak_rss_kb,
            hwm_kb: &mut peak_hwm_kb,
        };
        match wait_current_ready(
            &handle,
            &wake_flag,
            first_ts,
            &mut gen_base,
            &hb,
            &mut peaks,
        ) {
            Ok(()) => honesty.current_ready = true,
            Err(e) => {
                honesty.current_ready = false;
                cycle_err = Some(e);
            }
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
            Ok(outcome) => {
                sessions_after_priors = outcome.sessions;
                honesty.expected_priors_accepted = outcome.expected_accepted;
                honesty.expected_prior_skips = outcome.expected_skips;
                honesty.priors_slot_ok = outcome.incomplete.is_empty()
                    && outcome.accepted_seen == outcome.expected_accepted;
                if !outcome.incomplete.is_empty() {
                    notes.push(format!("prior_incomplete={}", outcome.incomplete.join("|")));
                    cycle_err = Some(format!(
                        "prior load incomplete: {}",
                        outcome.incomplete.join("; ")
                    ));
                } else if outcome.accepted_seen != outcome.expected_accepted {
                    let msg = format!(
                        "prior accepted_seen={} expected={}",
                        outcome.accepted_seen, outcome.expected_accepted
                    );
                    notes.push(msg.clone());
                    cycle_err = Some(msg);
                }
            }
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
        // Pre-scrub forward slice: never 0 when cycle_secs=0.
        let pre_scrub = pre_scrub_play_budget(args.cycle_secs, args.speed, first_ts, last_ts);
        let hb = HeartbeatCtx {
            out: &args.out,
            cycle,
            phase: "play_pre_scrub",
        };
        let mut peaks = PeakRss {
            rss_kb: &mut peak_rss_kb,
            hwm_kb: &mut peak_hwm_kb,
        };
        play_until(
            &handle,
            &wake_flag,
            (Instant::now() + pre_scrub).min(cycle_deadline),
            last_ts,
            &hb,
            &mut peaks,
        );
    }

    if cycle_err.is_none() {
        let hb = HeartbeatCtx {
            out: &args.out,
            cycle,
            phase: "scrub_settle",
        };
        match scrub_burst(
            &handle,
            &wake_flag,
            first_ts,
            last_ts,
            args.scrub_seeks,
            &mut gen_base,
            &hb,
        ) {
            Ok((n, answered)) => {
                seeks_issued = n;
                honesty.seeks_final_answered = answered;
            }
            Err(e) => {
                honesty.seeks_final_answered = false;
                cycle_err = Some(e);
            }
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

    // cycle_secs=0: scrub ends at last_ts; rewind so play_to_eof is a real forward run.
    if cycle_err.is_none() && args.cycle_secs == 0 {
        let generation = gen_base;
        gen_base += 1;
        if let Err(e) = send(
            &handle,
            EngineCmd::Seek {
                ts: first_ts,
                generation,
            },
            "EOF rewind Seek",
        ) {
            cycle_err = Some(e);
        } else {
            let snapshots = handle.snapshots();
            let start = Instant::now();
            let mut answered = false;
            while start.elapsed() < SCRUB_SETTLE {
                let _ = wake_flag.swap(false, Ordering::AcqRel);
                if snapshots.load().seek_generation >= generation {
                    answered = true;
                    break;
                }
                std::thread::sleep(POLL);
            }
            if !answered {
                cycle_err = Some(format!("EOF rewind seek gen {generation} unanswered"));
            } else if let Err(e) = send(&handle, EngineCmd::Play, "EOF rewind Play") {
                cycle_err = Some(e);
            } else {
                seeks_issued += 1;
                notes.push("eof_rewind_seek".into());
            }
        }
    }

    if cycle_err.is_none() {
        let hb = HeartbeatCtx {
            out: &args.out,
            cycle,
            phase: "play_to_eof",
        };
        let mut peaks = PeakRss {
            rss_kb: &mut peak_rss_kb,
            hwm_kb: &mut peak_hwm_kb,
        };
        play_until(
            &handle,
            &wake_flag,
            cycle_deadline,
            last_ts,
            &hb,
            &mut peaks,
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
        let re_sessions_pre = snap.profile.sessions.len();
        if let Err(e) = send(
            &handle,
            EngineCmd::SetSource(Source::Replay {
                path: args.replay.clone(),
            }),
            "SetSource re",
        ) {
            cycle_err = Some(e);
        } else {
            // Same-date SetSource must retain completed priors (ENGINE.md §2 r4).
            let hb = HeartbeatCtx {
                out: &args.out,
                cycle,
                phase: "observe_retention",
            };
            let mut peaks = PeakRss {
                rss_kb: &mut peak_rss_kb,
                hwm_kb: &mut peak_hwm_kb,
            };
            match observe_retention(
                &handle,
                &wake_flag,
                first_ts,
                &mut gen_base,
                cycle_deadline,
                &hb,
                &mut peaks,
            ) {
                Ok(post) => {
                    honesty.re_set_source_sessions = post;
                    honesty.retention_ok = post >= re_sessions_pre && re_sessions_pre > 0;
                    if !honesty.retention_ok {
                        let msg = format!(
                            "re-SetSource retention failed: pre={re_sessions_pre} post={post}"
                        );
                        notes.push(msg.clone());
                        cycle_err = Some(msg);
                    } else {
                        notes.push(format!(
                            "re-SetSource retention ok pre={re_sessions_pre} post={post}"
                        ));
                    }
                }
                Err(e) => {
                    honesty.retention_ok = false;
                    cycle_err = Some(e);
                }
            }
        }
    }

    honesty.rss_ceiling_ok = peak_rss_kb * 1024 <= RSS_BUDGET_BYTES;
    if !honesty.rss_ceiling_ok {
        notes.push("RSS_CEILING_EXCEEDED".into());
        if cycle_err.is_none() {
            cycle_err = Some(format!("RSS ceiling breached: peak_rss={} kB", peak_rss_kb));
        }
    }

    let shutdown = handle.shutdown();
    let (hwm, rss) = read_vm_status();
    peak_hwm_kb = peak_hwm_kb.max(hwm);
    peak_rss_kb = peak_rss_kb.max(rss);
    // Post-shutdown residual RSS is diagnostic only — honesty uses in-cycle peak.

    match (shutdown, cycle_err) {
        (Ok(exit), None) => {
            let ok = finalize_honesty(&mut honesty, &exit, &mut notes);
            if !ok {
                eprintln!(
                    "m7-soak: CYCLE {cycle} FAIL: honesty checks failed notes={:?}",
                    notes
                );
            }
            CycleResult {
                line: make_line(
                    cycle,
                    args,
                    started,
                    ok,
                    LineParts {
                        events_applied: pre_reset_applied,
                        events_read: pre_reset_read,
                        gap_records: pre_reset_gaps,
                        publications: exit.publications.max(pre_reset_pubs),
                        seeks_executed: exit.seeks_executed,
                        seeks_issued,
                        priors_completed: exit.priors_completed,
                        prior_skips: exit.prior_skips,
                        sessions: sessions_after_priors,
                        peak_rss_kb,
                        rss,
                        hwm,
                        panic: None,
                    },
                    &honesty,
                    &notes,
                ),
                peak_rss_kb,
                peak_hwm_kb,
            }
        }
        (Ok(exit), Some(msg)) => {
            eprintln!("m7-soak: CYCLE {cycle} FAIL: {msg}");
            let _ = finalize_honesty(&mut honesty, &exit, &mut notes);
            fail_cycle(FailCtx {
                cycle,
                args,
                started,
                peak_rss_kb,
                peak_hwm_kb,
                panic_msg: Some(msg),
                partial: Some(exit),
                honesty,
            })
        }
        (Err(payload), prior) => {
            let msg = format!("engine panic: {}", panic_message(&*payload));
            let combined = match prior {
                Some(p) => format!("{p}; {msg}"),
                None => msg,
            };
            eprintln!("m7-soak: CYCLE {cycle} FAIL: {combined}");
            fail_cycle(FailCtx {
                cycle,
                args,
                started,
                peak_rss_kb,
                peak_hwm_kb,
                panic_msg: Some(combined),
                partial: None,
                honesty,
            })
        }
    }
}

/// Pre-scrub play slice. cycle_secs>0 keeps historical max(1)/3 capped at 10 s;
/// cycle_secs=0 uses session_wall/3 (session_span_ns/speed) capped at 10 s, floor 1 s.
fn pre_scrub_play_budget(cycle_secs: u64, speed: f64, first_ts: u64, last_ts: u64) -> Duration {
    if cycle_secs > 0 {
        return Duration::from_secs(cycle_secs.max(1) / 3).min(Duration::from_secs(10));
    }
    let span_ns = last_ts.saturating_sub(first_ts) as f64;
    let session_wall_secs = (span_ns / 1_000_000_000.0) / speed.max(f64::MIN_POSITIVE);
    let slice = (session_wall_secs / 3.0).clamp(1.0, 10.0);
    Duration::from_secs_f64(slice)
}
