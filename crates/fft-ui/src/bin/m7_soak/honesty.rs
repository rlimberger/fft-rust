//! Cycle honesty accounting + JSONL cycle/fail lines.

use std::time::Instant;

use fft_engine::EngineExit;

use crate::args::Args;
use crate::metrics::CycleLine;
use crate::util::read_vm_status;

#[derive(Debug, Clone)]
pub struct Honesty {
    pub current_ready: bool,
    pub seeks_final_answered: bool,
    pub retention_ok: bool,
    pub rss_ceiling_ok: bool,
    pub priors_slot_ok: bool,
    pub expected_priors_accepted: u64,
    pub expected_prior_skips: u64,
    pub re_set_source_sessions: usize,
    pub engine_priors_ok: bool,
}

impl Default for Honesty {
    fn default() -> Self {
        Self {
            current_ready: false,
            seeks_final_answered: false,
            retention_ok: false,
            rss_ceiling_ok: true,
            priors_slot_ok: false,
            expected_priors_accepted: 0,
            expected_prior_skips: 0,
            re_set_source_sessions: 0,
            engine_priors_ok: false,
        }
    }
}

/// Engine counters must match the date-rule expected set exactly.
pub fn finalize_honesty(h: &mut Honesty, exit: &EngineExit, notes: &mut Vec<String>) -> bool {
    h.engine_priors_ok = exit.priors_completed == h.expected_priors_accepted
        && exit.prior_skips == h.expected_prior_skips;
    if !h.engine_priors_ok {
        notes.push(format!(
            "engine priors mismatch: completed={} skips={} expected_accepted={} expected_skips={}",
            exit.priors_completed,
            exit.prior_skips,
            h.expected_priors_accepted,
            h.expected_prior_skips
        ));
    }
    h.current_ready
        && h.seeks_final_answered
        && h.retention_ok
        && h.rss_ceiling_ok
        && h.priors_slot_ok
        && h.engine_priors_ok
}

pub struct LineParts {
    pub events_applied: u64,
    pub events_read: u64,
    pub gap_records: u64,
    pub publications: u64,
    pub seeks_executed: u64,
    pub seeks_issued: u64,
    pub priors_completed: u64,
    pub prior_skips: u64,
    pub sessions: usize,
    pub peak_rss_kb: u64,
    pub rss: u64,
    pub hwm: u64,
    pub panic: Option<String>,
}

pub fn make_line(
    cycle: u64,
    args: &Args,
    started: Instant,
    ok: bool,
    p: LineParts,
    honesty: &Honesty,
    notes: &[String],
) -> CycleLine {
    CycleLine {
        kind: "cycle",
        cycle,
        ok,
        wall_secs: started.elapsed().as_secs_f64(),
        events_applied: p.events_applied,
        events_read: p.events_read,
        gap_records: p.gap_records,
        publications: p.publications,
        seeks_executed: p.seeks_executed,
        seeks_issued: p.seeks_issued,
        seeks_final_answered: honesty.seeks_final_answered,
        priors_completed: p.priors_completed,
        prior_skips: p.prior_skips,
        expected_priors_accepted: honesty.expected_priors_accepted,
        expected_prior_skips: honesty.expected_prior_skips,
        sessions: p.sessions,
        re_set_source_sessions: honesty.re_set_source_sessions,
        retention_ok: honesty.retention_ok,
        current_ready: honesty.current_ready,
        rss_ceiling_ok: honesty.rss_ceiling_ok,
        vm_rss_kb: p.rss,
        vm_hwm_kb: p.hwm,
        peak_rss_kb: p.peak_rss_kb,
        speed: args.speed,
        cycle_secs_cap: args.cycle_secs,
        panic: p.panic,
        notes: if notes.is_empty() {
            None
        } else {
            Some(notes.join("; "))
        },
    }
}

pub struct FailCtx<'a> {
    pub cycle: u64,
    pub args: &'a Args,
    pub started: Instant,
    pub peak_rss_kb: u64,
    pub peak_hwm_kb: u64,
    pub panic_msg: Option<String>,
    pub partial: Option<EngineExit>,
    pub honesty: Honesty,
}

pub struct CycleResult {
    pub line: CycleLine,
    pub peak_rss_kb: u64,
    pub peak_hwm_kb: u64,
}

pub fn fail_cycle(ctx: FailCtx<'_>) -> CycleResult {
    let (hwm, rss) = read_vm_status();
    let peak_rss_kb = ctx.peak_rss_kb.max(rss);
    let peak_hwm_kb = ctx.peak_hwm_kb.max(hwm);
    let mut notes = vec!["cycle failure".into()];
    if let Some(ref p) = ctx.panic_msg {
        notes.push(p.clone());
    }
    let partial = ctx.partial.as_ref();
    CycleResult {
        line: make_line(
            ctx.cycle,
            ctx.args,
            ctx.started,
            false,
            LineParts {
                events_applied: partial.map(|e| e.coverage.events_applied).unwrap_or(0),
                events_read: partial.map(|e| e.coverage.events_read).unwrap_or(0),
                gap_records: partial.map(|e| e.coverage.gap_records).unwrap_or(0),
                publications: partial.map(|e| e.publications).unwrap_or(0),
                seeks_executed: partial.map(|e| e.seeks_executed).unwrap_or(0),
                seeks_issued: 0,
                priors_completed: partial.map(|e| e.priors_completed).unwrap_or(0),
                prior_skips: partial.map(|e| e.prior_skips).unwrap_or(0),
                sessions: 0,
                peak_rss_kb,
                rss,
                hwm,
                panic: ctx.panic_msg,
            },
            &ctx.honesty,
            &notes,
        ),
        peak_rss_kb,
        peak_hwm_kb,
    }
}
