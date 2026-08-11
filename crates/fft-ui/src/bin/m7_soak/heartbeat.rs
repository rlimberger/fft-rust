//! Mid-cycle JSONL heartbeats (kind=heartbeat; non-summary, non-final).

use std::path::Path;
use std::time::{Duration, Instant};

use crate::metrics::{HeartbeatLine, append_json_line};
use crate::util::read_vm_status;

pub const HEARTBEAT_INTERVAL: Duration = Duration::from_secs(30);

pub struct HeartbeatCtx<'a> {
    pub out: &'a Path,
    pub cycle: u64,
    pub phase: &'static str,
}

pub fn emit_heartbeat(
    ctx: &HeartbeatCtx<'_>,
    phase_start: Instant,
    last_hb: &mut Instant,
    events_applied: u64,
    applied_ts: u64,
) {
    if last_hb.elapsed() < HEARTBEAT_INTERVAL {
        return;
    }
    let (_, rss) = read_vm_status();
    let secs_in_phase = phase_start.elapsed().as_secs_f64();
    append_json_line(
        ctx.out,
        &HeartbeatLine {
            kind: "heartbeat",
            cycle: ctx.cycle,
            phase: ctx.phase,
            events_applied,
            applied_ts,
            vm_rss_kb: rss,
            secs_in_phase,
        },
    );
    eprintln!(
        "m7-soak: heartbeat cycle={} phase={} applied={} applied_ts={} rss={} kB secs_in_phase={:.1}",
        ctx.cycle, ctx.phase, events_applied, applied_ts, rss, secs_in_phase
    );
    *last_hb = Instant::now();
}
