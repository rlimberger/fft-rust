//! Harness-side injected-gap fixture for the M1.5 gate.
//!
//! Builds an honest sequence discontinuity: seq 1..=41, Gap(expected=42,
//! observed=141), then post-gap tail 141.. . `write_with_injected_gap` cannot
//! resequence the resume side, so the spliced log is written directly.

use crate::identity::{replay_live_identity, sections_from_exit};
use crate::report::{GapCheck, panic_message};
use fft_book::{Book, RefreshState};
use fft_core::{CanonicalEvent, EventKind, InstrumentMeta, OrderId, Price, Seq, Side, Ts};
use fft_engine::{EngineCmd, EngineConfig, EngineService, Source};
use fft_log::LogWriter;
use fft_replay::ReplaySource;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

const TICK: i64 = 250_000_000;
const DAY_S: u64 = 86_400;
const TRADE_DATE: u32 = 20_663;
const ACTION_TIMEOUT: Duration = Duration::from_secs(10);
const SAMPLE_INTERVAL: Duration = Duration::from_millis(1);
const PRE_GAP_LAST_SEQ: u32 = 41;
const GAP_EXPECTED: u64 = 42;
const GAP_OBSERVED: u64 = 141;
const POST_GAP_COUNT: u32 = 40;
const MS: u64 = 1_000_000;

/// Honest gap fixture. Errors are diagnostic strings for FAIL evidence.
pub fn run_gap_fixture_result() -> Result<GapCheck, String> {
    let open = session_open_ns(TRADE_DATE);
    let nonce = unique_nonce();
    let spliced = std::env::temp_dir().join(format!("fft-m15-gap-spliced-{nonce}.fftlog"));
    let live_out = std::env::temp_dir().join(format!("fft-m15-gap-live-{nonce}.fftlog"));
    let last_post_gap_seq = GAP_OBSERVED + u64::from(POST_GAP_COUNT) - 1;
    let injected_gap_ts = match write_honest_gap_log(&spliced, open) {
        Ok(ts) => ts,
        Err(error) => {
            cleanup_gap_paths(&[&spliced, &live_out]);
            return Err(error);
        }
    };
    if let Err(error) = assert_stream_honesty(&spliced, GAP_EXPECTED, GAP_OBSERVED) {
        cleanup_gap_paths(&[&spliced, &live_out]);
        return Err(error);
    }

    let wakes = Arc::new(AtomicU64::new(0));
    let handle = match EngineService::spawn(
        EngineConfig {
            visible_tick_span: 64,
        },
        Box::new(move || {
            wakes.fetch_add(1, Ordering::SeqCst);
        }),
    ) {
        Ok(handle) => handle,
        Err(error) => {
            cleanup_gap_paths(&[&spliced, &live_out]);
            return Err(format!("spawn gap engine: {error}"));
        }
    };
    // Exact event timestamp (seq 31 at open+30ms); SimLive rejects non-event heads.
    let fixture_head = open + 30 * MS;
    if let Err(error) = handle.send(EngineCmd::SetSource(Source::SimLive {
        path: spliced.clone(),
        head_ts: fixture_head,
        live_out: live_out.clone(),
    })) {
        let _ = handle.shutdown();
        cleanup_gap_paths(&[&spliced, &live_out]);
        return Err(format!("gap SetSource: {error}"));
    }
    let reached_gap = wait_until(ACTION_TIMEOUT, || {
        handle.snapshots().load().coverage.gap_records >= 1
    });
    let reached_tip = wait_until(ACTION_TIMEOUT, || {
        handle.snapshots().load().applied_seq == last_post_gap_seq
    });
    let exit = match handle.shutdown() {
        Ok(exit) => exit,
        Err(payload) => {
            cleanup_gap_paths(&[&spliced, &live_out]);
            return Err(format!(
                "gap engine thread panicked: {}",
                panic_message(&*payload)
            ));
        }
    };
    let gap_records = exit.coverage.gap_records;
    let applied_seq = exit.watermarks.applied_seq;
    let logged_seq = exit.watermarks.logged_seq;
    let watermark_ok =
        gap_records == 1 && applied_seq == last_post_gap_seq && logged_seq == applied_seq;
    let refresh = match (
        exit.book_bytes.as_ref(),
        exit.flow_bytes.as_ref(),
        exit.refresh_bytes.as_ref(),
    ) {
        (Some(book), Some(flow), Some(refresh)) => {
            match refresh_unavailable_from_bytes(book, flow, refresh, OrderId(1)) {
                Ok(value) => value,
                Err(error) => {
                    cleanup_gap_paths(&[&spliced, &live_out]);
                    return Err(error);
                }
            }
        }
        _ => {
            cleanup_gap_paths(&[&spliced, &live_out]);
            return Err("gap fixture missing book/flow/refresh sections at shutdown".into());
        }
    };
    let identity = match sections_from_exit(&exit) {
        Ok(sections) => match replay_live_identity(&live_out, &sections) {
            Ok(check) => check,
            Err(error) => {
                cleanup_gap_paths(&[&spliced, &live_out]);
                return Err(format!("gap live-out identity: {error}"));
            }
        },
        Err(error) => {
            cleanup_gap_paths(&[&spliced, &live_out]);
            return Err(format!("gap sections_from_exit: {error}"));
        }
    };
    cleanup_gap_paths(&[&spliced, &live_out]);
    Ok(GapCheck {
        injected_gap_ts,
        injected_expected_seq: GAP_EXPECTED,
        injected_observed_seq: GAP_OBSERVED,
        gap_records,
        applied_seq,
        logged_seq,
        refresh_order_id: 1,
        refresh_unavailable: refresh,
        ok: reached_gap && reached_tip && watermark_ok && refresh && identity.ok,
    })
}

fn cleanup_gap_paths(paths: &[&Path]) {
    for path in paths {
        if let Err(error) = std::fs::remove_file(path) {
            eprintln!("m15-gate: WARNING remove {}: {error}", path.display());
        }
    }
}

fn refresh_unavailable_from_bytes(
    book: &[u8],
    flow: &[u8],
    refresh: &[u8],
    id: OrderId,
) -> Result<bool, String> {
    let book = Book::restore(book, flow, refresh).map_err(|e| format!("restore gap book: {e}"))?;
    Ok(matches!(book.refresh_state(id), RefreshState::Unavailable))
}

/// Write seq 1..=41, Gap(42→141), then seq 141.. continuing in time. Returns gap ts.
fn write_honest_gap_log(path: &Path, open: u64) -> Result<u64, String> {
    let meta = InstrumentMeta {
        symbol: "ESU6".into(),
        instrument_id: 42,
        dataset: "GLBX.MDP3".into(),
        min_price_increment: Price(TICK),
        unit_of_measure_qty: 50_000_000_000,
        display_factor: 1,
        trade_date: TRADE_DATE,
        session_open: Ts(open),
    };
    let mut writer =
        LogWriter::create(path, &meta).map_err(|e| format!("gap spliced create: {e}"))?;
    let mut events = Vec::with_capacity(
        usize::try_from(PRE_GAP_LAST_SEQ).unwrap() + 1 + usize::try_from(POST_GAP_COUNT).unwrap(),
    );
    for seq in 1..=PRE_GAP_LAST_SEQ {
        events.push(add_event(open, seq, u64::from(seq) - 1));
    }
    let gap_ts = open + u64::from(PRE_GAP_LAST_SEQ) * MS;
    events.push(CanonicalEvent::gap(Ts(gap_ts), GAP_EXPECTED, GAP_OBSERVED));
    for i in 0..POST_GAP_COUNT {
        let seq = u32::try_from(GAP_OBSERVED).expect("observed fits u32") + i;
        let ts_index = u64::from(PRE_GAP_LAST_SEQ) + u64::from(i);
        events.push(add_event(open, seq, ts_index));
    }
    writer
        .append_events(&events)
        .map_err(|e| format!("gap spliced append: {e}"))?;
    writer
        .close()
        .map_err(|e| format!("gap spliced close: {e}"))?;
    Ok(gap_ts)
}

fn add_event(open: u64, seq: u32, ts_index: u64) -> CanonicalEvent {
    let i = u64::from(seq);
    CanonicalEvent {
        kind: EventKind::Add,
        side: if i % 2 == 0 { Side::Bid } else { Side::Ask },
        flags: 0,
        size: 3,
        ts: Ts(open + ts_index * MS),
        seq: Seq(seq),
        price: Price(if i % 2 == 0 {
            (20_000 - 1) * TICK
        } else {
            (20_000 + 1) * TICK
        }),
        order_id: OrderId(u64::from(seq)),
    }
}

fn assert_stream_honesty(path: &Path, expected: u64, observed: u64) -> Result<(), String> {
    let mut source = ReplaySource::open(path)
        .map_err(|e| format!("reopen honest gap log {}: {e}", path.display()))?;
    let mut previous: Option<CanonicalEvent> = None;
    while let Some(event) = source
        .next_event()
        .map_err(|e| format!("read honest gap log: {e}"))?
    {
        if event.kind != EventKind::Gap {
            previous = Some(event);
            continue;
        }
        let (got_expected, got_observed) = event.gap_seqs();
        if (got_expected, got_observed) != (expected, observed) {
            return Err(format!(
                "gap_seqs mismatch: got ({got_expected},{got_observed}) want ({expected},{observed})"
            ));
        }
        let prior = previous.ok_or_else(|| "gap record has no prior event".to_string())?;
        let prior_seq = u64::from(prior.seq.0);
        if prior_seq != expected - 1 {
            return Err(format!(
                "pre-gap seq {prior_seq} != expected-1 ({})",
                expected - 1
            ));
        }
        let next = source
            .next_event()
            .map_err(|e| format!("read post-gap event: {e}"))?
            .ok_or_else(|| "gap record has no following event".to_string())?;
        let next_seq = u64::from(next.seq.0);
        if next_seq != observed {
            return Err(format!("post-gap seq {next_seq} != observed {observed}"));
        }
        return Ok(());
    }
    Err("honest gap log contains no Gap record".into())
}

fn session_open_ns(trade_date: u32) -> u64 {
    (u64::from(trade_date.saturating_sub(1)) * DAY_S + 22 * 3_600) * 1_000_000_000
}

fn unique_nonce() -> u128 {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| duration.as_nanos());
    (u128::from(std::process::id()) << 96) ^ now
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
