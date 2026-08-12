//! Sim-live gate measurement loops (`docs/ENGINE.md` §5).

use crate::args::Args;
use crate::fixture::run_gap_fixture_result;
use crate::identity::{append_check, probe_live_during, replay_live_identity, sections_from_exit};
use crate::report::{
    Budgets, DistNs, Evidence, EvidenceInput, GoLiveCheck, JoinCheck, LagCheck, PartialChecks,
    RuntimeFail, SourceCheck, assemble_evidence, panic_message, runtime_fail_evidence,
};
use fft_engine::{APPLY_BUDGET, EngineCmd, EngineConfig, EngineHandle, EngineService, Source};
use fft_replay::ReplaySource;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::thread;
use std::time::{Duration, Instant};

const JOIN_TIMEOUT: Duration = Duration::from_secs(600);
const ACTION_TIMEOUT: Duration = Duration::from_secs(10);
const SAMPLE_INTERVAL: Duration = Duration::from_millis(1);

fn budgets_for(gate_secs: u64) -> Budgets {
    Budgets {
        apply_budget_ns: APPLY_BUDGET.as_nanos() as u64,
        gate_secs,
        join_timeout_s: JOIN_TIMEOUT.as_secs(),
    }
}

pub fn run(args: Args) -> Evidence {
    let budgets = budgets_for(args.gate_secs);
    let replay = args.replay.display().to_string();
    let live_out = args.live_out.display().to_string();

    let source = match inspect_source(&args.replay, args.head_ts) {
        Ok(source) => source,
        Err(diagnostic) => {
            return runtime_fail_evidence(RuntimeFail {
                replay: &replay,
                head_ts: args.head_ts,
                live_out: &live_out,
                budgets,
                dimension: "source/head validation",
                diagnostic: &diagnostic,
                partial: PartialChecks::default(),
            });
        }
    };
    if !source.ok {
        return runtime_fail_evidence(RuntimeFail {
            replay: &replay,
            head_ts: args.head_ts,
            live_out: &live_out,
            budgets,
            dimension: "source/head validation",
            diagnostic: &source_fail_diagnostic(&source),
            partial: PartialChecks {
                source: Some(source),
                ..PartialChecks::default()
            },
        });
    }

    let wakes = Arc::new(AtomicU64::new(0));
    let wake = {
        let wakes = wakes.clone();
        Box::new(move || {
            wakes.fetch_add(1, Ordering::SeqCst);
        })
    };
    let handle = match EngineService::spawn(
        EngineConfig {
            visible_tick_span: 64,
        },
        wake,
    ) {
        Ok(handle) => handle,
        Err(error) => {
            return runtime_fail_evidence(RuntimeFail {
                replay: &replay,
                head_ts: args.head_ts,
                live_out: &live_out,
                budgets,
                dimension: "engine spawn",
                diagnostic: &format!("spawn engine: {error}"),
                partial: PartialChecks {
                    source: Some(source),
                    ..PartialChecks::default()
                },
            });
        }
    };

    let join_start = Instant::now();
    if let Err(error) = handle.send(EngineCmd::SetSource(Source::SimLive {
        path: args.replay.clone(),
        head_ts: source.last_ts_through_head,
        live_out: args.live_out.clone(),
    })) {
        return fail_after_handle(
            handle,
            RuntimeFail {
                replay: &replay,
                head_ts: args.head_ts,
                live_out: &live_out,
                budgets,
                dimension: "SetSource(SimLive)",
                diagnostic: &format!("SetSource(SimLive): {error}"),
                partial: PartialChecks {
                    source: Some(source),
                    ..PartialChecks::default()
                },
            },
        );
    }

    let reached_head = wait_until(JOIN_TIMEOUT, || {
        let snap = handle.snapshots().load();
        snap.applied_ts >= source.last_ts_through_head
            && snap.coverage.events_applied >= source.events_through_head
    });
    let join_wall_s = join_start.elapsed().as_secs_f64();
    let joined = handle.snapshots().load();
    let clean_join_coverage = joined.coverage.events_read == joined.coverage.events_applied;
    let applied_from_open = joined.seek_generation == 0
        && joined.coverage.events_applied == source.events_through_head
        && source.starts_at_session_open;
    let join = JoinCheck {
        pinned_head_ts: source.pinned_event_ts,
        applied_ts: joined.applied_ts,
        applied_seq: joined.applied_seq,
        events_read: joined.coverage.events_read,
        events_applied: joined.coverage.events_applied,
        seek_generation: joined.seek_generation,
        join_wall_s,
        reached_head: reached_head && joined.applied_ts >= source.last_ts_through_head,
        applied_from_open,
        clean_coverage: clean_join_coverage,
        ok: reached_head && applied_from_open && clean_join_coverage,
    };

    let lag = measure_wall_pin(&handle, args.gate_secs, joined.applied_ts);
    let go_live = exercise_go_live(&handle, &source, source.pinned_event_ts, lag.ok);
    let (during_is_live, during_index_source_live_recovery) = probe_live_during(&args.live_out);

    let exit = match handle.shutdown() {
        Ok(exit) => exit,
        Err(payload) => {
            let msg = panic_message(&*payload);
            return runtime_fail_evidence(RuntimeFail {
                replay: &replay,
                head_ts: args.head_ts,
                live_out: &live_out,
                budgets,
                dimension: "engine thread panic",
                diagnostic: &format!("engine thread panicked: {msg}"),
                partial: PartialChecks {
                    source: Some(source),
                    join: Some(join),
                    lag: Some(lag),
                    go_live: Some(go_live),
                    gap: None,
                },
            });
        }
    };

    let mut notes = Vec::new();
    let append = append_check(
        &args.live_out,
        &exit,
        during_is_live,
        during_index_source_live_recovery,
    );
    let identity = match sections_from_exit(&exit)
        .and_then(|sections| replay_live_identity(&args.live_out, &sections))
    {
        Ok(check) => check,
        Err(diagnostic) => {
            notes.push(diagnostic);
            crate::report::IdentityCheck::unavailable()
        }
    };
    let gap = match run_gap_fixture_result() {
        Ok(check) => check,
        Err(diagnostic) => {
            notes.push(diagnostic);
            crate::report::GapCheck::unavailable()
        }
    };

    let note = if notes.is_empty() {
        crate::report::NOTE_BASE.to_string()
    } else {
        format!("{}; {}", crate::report::NOTE_BASE, notes.join("; "))
    };
    assemble_evidence(EvidenceInput {
        replay: &replay,
        head_ts: args.head_ts,
        live_out: &live_out,
        source,
        join,
        lag,
        go_live,
        gap,
        append,
        identity,
        budgets,
        notes: Some(note),
    })
}

fn source_fail_diagnostic(source: &SourceCheck) -> String {
    format!(
        "invalid head/source: head_in_log={} starts_at_session_open={} checkpoint_count={} events_through_head={} last_ts_through_head={}",
        source.head_in_log,
        source.starts_at_session_open,
        source.checkpoint_count,
        source.events_through_head,
        source.last_ts_through_head
    )
}

fn fail_after_handle(handle: EngineHandle, fail: RuntimeFail<'_>) -> Evidence {
    match handle.shutdown() {
        Ok(_) => runtime_fail_evidence(fail),
        Err(payload) => {
            let msg = panic_message(&*payload);
            let diagnostic = format!("{}; engine thread panicked: {msg}", fail.diagnostic);
            runtime_fail_evidence(RuntimeFail {
                replay: fail.replay,
                head_ts: fail.head_ts,
                live_out: fail.live_out,
                budgets: fail.budgets,
                dimension: "engine thread panic",
                diagnostic: &diagnostic,
                partial: fail.partial,
            })
        }
    }
}

fn inspect_source(path: &Path, head_ts: u64) -> Result<SourceCheck, String> {
    let mut replay =
        ReplaySource::open(path).map_err(|error| format!("open source for inventory: {error}"))?;
    let session_open_ts = replay.meta().session_open.0;
    let checkpoint_count = replay.checkpoint_count();
    let mut first_event_ts = 0;
    let mut last_event_ts = 0;
    let mut events_through_head = 0;
    let mut last_ts_through_head = 0;
    let mut in_join_prefix = true;
    while let Some(event) = replay
        .next_event()
        .map_err(|error| format!("source inventory: {error}"))?
    {
        if first_event_ts == 0 {
            first_event_ts = event.ts.0;
        }
        last_event_ts = last_event_ts.max(event.ts.0);
        if in_join_prefix && event.ts.0 <= head_ts {
            events_through_head += 1;
            last_ts_through_head = event.ts.0;
        } else {
            in_join_prefix = false;
        }
    }
    let head_in_log = events_through_head > 0
        && head_ts >= first_event_ts
        && last_ts_through_head <= head_ts
        && head_ts < last_event_ts;
    let starts_at_session_open = first_event_ts >= session_open_ts
        && first_event_ts < session_open_ts.saturating_add(60_000_000_000);
    Ok(SourceCheck {
        requested_head_ts: head_ts,
        pinned_event_ts: last_ts_through_head,
        head_snap_back_ns: head_ts.saturating_sub(last_ts_through_head),
        session_open_ts,
        first_event_ts,
        last_event_ts,
        events_through_head,
        last_ts_through_head,
        checkpoint_count,
        head_in_log,
        starts_at_session_open,
        ok: head_in_log && starts_at_session_open && checkpoint_count > 0,
    })
}

fn measure_wall_pin(handle: &EngineHandle, gate_secs: u64, joined_ts: u64) -> LagCheck {
    let gate_start = Instant::now();
    let gate_for = Duration::from_secs(gate_secs);
    let baseline = handle.snapshots().load();
    let baseline_generation = baseline.generation;
    let mut abs_lags = Vec::new();
    let mut last_generation = baseline_generation;
    let mut final_ts = joined_ts;
    let mut clean_coverage = true;
    while gate_start.elapsed() < gate_for {
        let snap = handle.snapshots().load();
        final_ts = final_ts.max(snap.applied_ts);
        clean_coverage &= snap.coverage.events_read == snap.coverage.events_applied;
        if snap.generation > last_generation {
            abs_lags.push(snap.coverage.head_lag_ns.unsigned_abs());
            last_generation = snap.generation;
        }
        thread::sleep(SAMPLE_INTERVAL);
    }
    let abs_head_lag = DistNs::from_abs_samples(abs_lags);
    let apply_budget_ns = APPLY_BUDGET.as_nanos() as u64;
    let advanced_ts_ns = final_ts.saturating_sub(joined_ts);
    let lag_within_budget = abs_head_lag
        .as_ref()
        .is_some_and(|dist| dist.p99_ns <= apply_budget_ns);
    LagCheck {
        distinct_publications_sampled: abs_head_lag.as_ref().map_or(0, |dist| dist.n),
        abs_head_lag,
        apply_budget_ns,
        advanced_ts_ns,
        clean_coverage,
        ok: lag_within_budget && clean_coverage && advanced_ts_ns > 0,
    }
}

fn exercise_go_live(
    handle: &EngineHandle,
    source: &SourceCheck,
    head_ts: u64,
    wall_pin_ready: bool,
) -> GoLiveCheck {
    let tip_before_scrub_ts = handle.snapshots().load().applied_ts;
    let scrub_target_ts = choose_scrub_target(source, head_ts, tip_before_scrub_ts);
    if !wall_pin_ready || scrub_target_ts == 0 {
        return GoLiveCheck {
            scrub_target_ts,
            scrubbed_ts: 0,
            tip_before_scrub_ts,
            resumed_ts: 0,
            resumed_seek_generation: 0,
            resumed_abs_head_lag_ns: u64::MAX,
            scrubbed_behind_tip: false,
            reached_prior_tip: false,
            ok: false,
        };
    }
    if handle
        .send(EngineCmd::Seek {
            ts: scrub_target_ts,
            generation: 1,
        })
        .is_err()
    {
        return GoLiveCheck::unavailable();
    }
    let seek_landed = wait_until(ACTION_TIMEOUT, || {
        handle.snapshots().load().seek_generation == 1
    });
    let scrubbed = handle.snapshots().load();
    let scrubbed_ts = scrubbed.applied_ts;
    let scrubbed_behind_tip = seek_landed && scrubbed_ts < tip_before_scrub_ts;
    if handle.send(EngineCmd::SetSpeed(0.25)).is_err() || handle.send(EngineCmd::Play).is_err() {
        return GoLiveCheck {
            scrub_target_ts,
            scrubbed_ts,
            tip_before_scrub_ts,
            resumed_ts: 0,
            resumed_seek_generation: 0,
            resumed_abs_head_lag_ns: u64::MAX,
            scrubbed_behind_tip,
            reached_prior_tip: false,
            ok: false,
        };
    }
    thread::sleep(Duration::from_millis(25));
    if handle.send(EngineCmd::GoLive).is_err() {
        return GoLiveCheck {
            scrub_target_ts,
            scrubbed_ts,
            tip_before_scrub_ts,
            resumed_ts: 0,
            resumed_seek_generation: 0,
            resumed_abs_head_lag_ns: u64::MAX,
            scrubbed_behind_tip,
            reached_prior_tip: false,
            ok: false,
        };
    }
    let reached_prior_tip = wait_until(ACTION_TIMEOUT, || {
        let snap = handle.snapshots().load();
        snap.seek_generation == 0 && snap.applied_ts >= tip_before_scrub_ts
    });
    // Crossing the pre-scrub tip is not the end of GoLive: the wall target kept
    // advancing during the scrub, so the engine is legally still CatchingToWall
    // here and an instantaneous lag sample mid-catch-up over-reads. §5.2's bound
    // is that the pin settles ("caught up on the next slice") — assert settle
    // within the action timeout, then record the settled lag.
    let lag_ok = reached_prior_tip
        && wait_until(ACTION_TIMEOUT, || {
            handle
                .snapshots()
                .load()
                .coverage
                .head_lag_ns
                .unsigned_abs()
                <= APPLY_BUDGET.as_nanos() as u64
        });
    let resumed = handle.snapshots().load();
    GoLiveCheck {
        scrub_target_ts,
        scrubbed_ts,
        tip_before_scrub_ts,
        resumed_ts: resumed.applied_ts,
        resumed_seek_generation: resumed.seek_generation,
        resumed_abs_head_lag_ns: resumed.coverage.head_lag_ns.unsigned_abs(),
        scrubbed_behind_tip,
        reached_prior_tip,
        ok: scrubbed_behind_tip && reached_prior_tip && resumed.seek_generation == 0 && lag_ok,
    }
}

fn choose_scrub_target(source: &SourceCheck, head_ts: u64, tip_ts: u64) -> u64 {
    if source.checkpoint_count == 0 || tip_ts <= source.first_event_ts {
        return 0;
    }
    let span = tip_ts.saturating_sub(source.first_event_ts);
    let candidate = tip_ts.saturating_sub((span / 2).max(1));
    candidate
        .min(head_ts.saturating_sub(1))
        .max(source.first_event_ts)
}

fn wait_until(timeout: Duration, mut pred: impl FnMut() -> bool) -> bool {
    let start = Instant::now();
    while !pred() {
        if start.elapsed() >= timeout {
            return false;
        }
        thread::sleep(SAMPLE_INTERVAL);
    }
    true
}
