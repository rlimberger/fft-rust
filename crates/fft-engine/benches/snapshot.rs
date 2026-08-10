use criterion::{Criterion, criterion_group, criterion_main};
use fft_book::Book;
use fft_core::{CanonicalEvent, EventKind, OrderId, Price, Seq, Side, Ts};
use fft_engine::build_snapshot;
use fft_profile::{MultiProfile, SessionClock};
use std::hint::black_box;

const TICK: i64 = 250_000_000;
const BASE: i64 = 20_000;

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

criterion_group!(benches, snapshot_benchmark);
criterion_main!(benches);
