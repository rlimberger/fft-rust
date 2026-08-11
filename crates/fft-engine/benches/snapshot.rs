//! Snapshot construction criterion bench + optional p99 gate.
//!
//! Criterion path (default): measures `build_snapshot` on a dense 512-level book
//! + 5-day week profile fixture.
//!
//! Gate path (claimable numbers only on the quiet box per `docs/PERF-RUNNER.md`;
//! shared CI must NOT run it — perf gates never run in shared CI):
//!
//! ```text
//! cargo bench -p fft-engine --bench snapshot -- --gate
//! # or: FFT_SNAPSHOT_GATE=1 cargo bench -p fft-engine --bench snapshot
//! ```
//!
//! Samples N=10_000 builds, prints one JSON line (p50/p95/p99/max µs + budget +
//! verdict), exits nonzero if p99 > 300 µs. Also asserts estimated_heap_bytes()
//! ≤ 8 MiB (defense in depth; runtime publish assert remains primary).

use criterion::{Criterion, criterion_group};
use fft_book::Book;
use fft_core::{CanonicalEvent, EventKind, OrderId, Price, Seq, Side, Ts};
use fft_engine::build_snapshot;
use fft_profile::{MultiProfile, SessionClock};
use std::env;
use std::hint::black_box;
use std::process::ExitCode;
use std::time::Instant;

const TICK: i64 = 250_000_000;
const BASE: i64 = 20_000;

/// ENGINE.md §3.5 construction budget (µs).
const P99_BUDGET_US: u64 = 300;
/// ENGINE.md §3.5 steady-state heap budget.
const HEAP_BUDGET_BYTES: usize = 8 * 1024 * 1024;
const GATE_SAMPLES: usize = 10_000;

fn event(
    kind: EventKind,
    side: Side,
    price_tick: i64,
    size: u32,
    ts: u64,
    seq: u32,
    id: u64,
) -> CanonicalEvent {
    CanonicalEvent {
        kind,
        side,
        flags: 0,
        size,
        ts: Ts(ts),
        seq: Seq(seq),
        price: Price(price_tick * TICK),
        order_id: OrderId(id),
    }
}

fn populated() -> (Book, MultiProfile) {
    let mut book = Book::new(Price(TICK));
    for offset in 0..512u32 {
        let (side, tick) = if offset < 256 {
            (Side::Bid, BASE - 1 - i64::from(offset))
        } else {
            (Side::Ask, BASE + 1 + i64::from(offset - 256))
        };
        book.apply(&event(
            EventKind::Add,
            side,
            tick,
            10 + offset,
            u64::from(offset) + 1,
            offset + 1,
            u64::from(offset) + 1,
        ));
    }

    let mut profiles = MultiProfile::new(Price(TICK));
    for day in 20_662..20_667 {
        profiles.begin_session(day);
        let open = SessionClock::for_trade_date(day).session_open().0;
        for offset in 0..512u32 {
            profiles.apply(&event(
                EventKind::Trade,
                if offset % 2 == 0 {
                    Side::Bid
                } else {
                    Side::Ask
                },
                BASE - 256 + i64::from(offset),
                1 + offset % 10,
                open + u64::from(offset) * 1_000_000,
                0,
                0,
            ));
        }
    }
    (book, profiles)
}

fn snapshot_benchmark(c: &mut Criterion) {
    let (book, profiles) = populated();
    c.bench_function("render_snapshot_dense_512_week_profile", |b| {
        b.iter(|| {
            black_box(build_snapshot(
                1,
                512,
                512,
                0,
                512,
                black_box(&book),
                black_box(&profiles),
            ))
        });
    });
}

fn percentile_us(sorted_ns: &[u64], p: f64) -> u64 {
    assert!(!sorted_ns.is_empty(), "empty sample set");
    assert!((0.0..=1.0).contains(&p), "percentile out of range: {p}");
    // 1-based ceiling rank (matches house frame-stats quantile).
    let rank = ((p * sorted_ns.len() as f64).ceil() as usize).max(1);
    let ns = sorted_ns[rank - 1];
    // Round half-up to whole µs so sub-µs builds don't report as 0.
    (ns + 500) / 1_000
}

fn run_gate() -> ExitCode {
    let (book, profiles) = populated();

    // Warm one construction so the measured set is steady-state.
    let warm = build_snapshot(1, 512, 512, 0, 512, &book, &profiles);
    let heap = warm.estimated_heap_bytes();
    if heap > HEAP_BUDGET_BYTES {
        eprintln!(
            "{{\"gate\":\"snapshot_construction\",\"samples\":0,\"p50_us\":null,\"p95_us\":null,\"p99_us\":null,\"max_us\":null,\"budget_p99_us\":{P99_BUDGET_US},\"heap_bytes\":{heap},\"budget_heap_bytes\":{HEAP_BUDGET_BYTES},\"verdict\":\"FAIL\",\"reason\":\"heap_over_budget\"}}"
        );
        return ExitCode::FAILURE;
    }

    let mut samples_ns = Vec::with_capacity(GATE_SAMPLES);
    for i in 0..GATE_SAMPLES {
        let t0 = Instant::now();
        let snap = build_snapshot(
            1 + i as u64,
            512,
            512,
            0,
            512,
            black_box(&book),
            black_box(&profiles),
        );
        let elapsed = t0.elapsed();
        black_box(snap);
        samples_ns.push(elapsed.as_nanos() as u64);
    }

    samples_ns.sort_unstable();
    let p50 = percentile_us(&samples_ns, 0.50);
    let p95 = percentile_us(&samples_ns, 0.95);
    let p99 = percentile_us(&samples_ns, 0.99);
    let max = percentile_us(&samples_ns, 1.0);
    let pass = p99 <= P99_BUDGET_US;
    let verdict = if pass { "PASS" } else { "FAIL" };

    // One JSON line: numbers + budget + verdict (house style).
    println!(
        "{{\"gate\":\"snapshot_construction\",\"samples\":{GATE_SAMPLES},\"p50_us\":{p50},\"p95_us\":{p95},\"p99_us\":{p99},\"max_us\":{max},\"budget_p99_us\":{P99_BUDGET_US},\"heap_bytes\":{heap},\"budget_heap_bytes\":{HEAP_BUDGET_BYTES},\"verdict\":\"{verdict}\"}}"
    );

    if pass {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    }
}

fn gate_requested() -> bool {
    if env::var_os("FFT_SNAPSHOT_GATE").is_some_and(|v| v != "0") {
        return true;
    }
    env::args().any(|a| a == "--gate")
}

fn main() -> ExitCode {
    if gate_requested() {
        return run_gate();
    }
    // Criterion path: keep measurement name/shape unchanged.
    benches();
    ExitCode::SUCCESS
}

criterion_group!(benches, snapshot_benchmark);
// criterion_main! expands to its own main; we provide main above for the gate path.
// The group is still registered so criterion sees the same bench function.
