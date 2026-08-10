//! Encode + decode round-trip benchmark over a realistic synthetic MBO mix:
//! Add-heavy with cancels, modifies, trades and fills, ~350 ns median inter-event
//! spacing, tick-quantised random-walk prices. Full-path numbers (writer → file →
//! mmap reader), not micro-codec numbers, because that is what the replay budget buys.

use std::path::PathBuf;

use criterion::{Criterion, Throughput, criterion_group, criterion_main};
use fft_core::{CanonicalEvent, EventKind, OrderId, Price, Seq, Side, Ts};
use fft_log::{LogReader, LogWriter};

const EVENTS: usize = 100_000;
const EVENTS_PER_FRAME: usize = 8_192;

/// Deterministic xorshift64* — no rand dependency, stable event mix across runs.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.0 = x;
        x.wrapping_mul(0x2545_f491_4f6c_dd1d)
    }
}

fn side_of(roll: u64) -> Side {
    if roll.is_multiple_of(2) {
        Side::Bid
    } else {
        Side::Ask
    }
}

fn synthetic_events(n: usize) -> Vec<CanonicalEvent> {
    let mut rng = Rng(0x5eed_5eed_5eed_5eed);
    let mut ts: u64 = 1_785_000_000_000_000_000;
    let mut price_ticks: i64 = 20_001; // 5000.25 in 0.25 ticks
    let mut order_id: u64 = 1;
    (0..n)
        .map(|i| {
            ts += 100 + rng.next() % 500;
            let roll = rng.next() % 100;
            let (kind, side) = match roll {
                0..=39 => {
                    order_id += 1;
                    (EventKind::Add, side_of(roll))
                }
                40..=69 => (EventKind::Cancel, side_of(roll)),
                70..=79 => (EventKind::Modify, side_of(roll)),
                80..=94 => {
                    price_ticks += (rng.next() % 3) as i64 - 1;
                    (EventKind::Trade, Side::None)
                }
                _ => (EventKind::Fill, side_of(roll)),
            };
            CanonicalEvent {
                kind,
                side,
                flags: (i % 2) as u16,
                size: 1 + (rng.next() % 10) as u32,
                ts: Ts(ts),
                seq: Seq(i as u32 + 1),
                price: Price(price_ticks * 250_000_000),
                order_id: OrderId(1 + rng.next() % order_id),
            }
        })
        .collect()
}

fn bench_path(name: &str) -> PathBuf {
    std::env::temp_dir().join(format!(
        "fft-log-bench-{}-{name}.fftlog",
        std::process::id()
    ))
}

fn meta() -> fft_core::InstrumentMeta {
    fft_core::InstrumentMeta {
        symbol: "ESU6".into(),
        instrument_id: 42,
        dataset: "GLBX.MDP3".into(),
        min_price_increment: Price(250_000_000),
        unit_of_measure_qty: 50_000_000_000,
        display_factor: 1,
        trade_date: 20_662,
        session_open: Ts(1_785_000_000_000_000_000),
    }
}

fn write_log(path: &PathBuf, events: &[CanonicalEvent]) {
    let _ = std::fs::remove_file(path);
    let mut w = LogWriter::create(path, &meta()).unwrap();
    for chunk in events.chunks(EVENTS_PER_FRAME) {
        w.append_events(chunk).unwrap();
    }
    w.close().unwrap();
}

fn roundtrip(c: &mut Criterion) {
    let events = synthetic_events(EVENTS);
    let mut group = c.benchmark_group("roundtrip");
    group.throughput(Throughput::Elements(EVENTS as u64));

    let encode_path = bench_path("encode");
    group.bench_function("encode_100k", |b| {
        b.iter(|| write_log(&encode_path, &events));
    });

    let decode_path = bench_path("decode");
    write_log(&decode_path, &events);
    group.bench_function("decode_100k", |b| {
        b.iter(|| {
            let (reader, _) = LogReader::open(&decode_path).unwrap();
            let mut n = 0usize;
            for e in reader.events(0..reader.frame_count()) {
                std::hint::black_box(e.unwrap());
                n += 1;
            }
            assert_eq!(n, EVENTS);
        });
    });

    group.finish();
    let _ = std::fs::remove_file(&encode_path);
    let _ = std::fs::remove_file(&decode_path);
}

criterion_group!(benches, roundtrip);
criterion_main!(benches);
