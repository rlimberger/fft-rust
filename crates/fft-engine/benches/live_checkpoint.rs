//! SimLive 60 s wall-clock checkpoint cost vs the engine's 4 ms slice budget.
//!
//! The live path runs six-section serialize + zstd + frame write synchronously
//! inside apply (`live_log.rs` → `write_state_checkpoint`). That helper is
//! `pub(crate)` and unreachable from this external bench crate; this measures
//! the equivalent public path: `Book`/`MultiProfile` serialize +
//! [`LogWriter::write_checkpoint`] with the same section ids/versions/order.
//!
//! ```text
//! cargo bench -p fft-engine --bench live_checkpoint -- \
//!   --warm-up-time 1 --measurement-time 2 --sample-size 20
//! ```

use criterion::{Criterion, criterion_group, criterion_main};
use fft_book::{
    BOOK_SECTION_ID, BOOK_SECTION_VERSION, Book, FLOW_SECTION_ID, FLOW_SECTION_VERSION,
    REFRESH_SECTION_ID, REFRESH_SECTION_VERSION,
};
use fft_core::{CanonicalEvent, EventKind, InstrumentMeta, OrderId, Price, Seq, Side, Ts};
use fft_log::{LogWriter, SectionRef};
use fft_profile::{
    CVD_SECTION_ID, CVD_SECTION_VERSION, MultiProfile, PROFILE_SECTION_ID, PROFILE_SECTION_VERSION,
    SESSION_SECTION_ID, SESSION_SECTION_VERSION, SessionClock,
};
use std::hint::black_box;
use std::time::Duration;
use tempfile::TempDir;

const TICK: i64 = 250_000_000;
const BASE: i64 = 20_000;
const TRADE_DATE: u32 = 20_663;
const DAY_S: u64 = 86_400;
const SESSION_OPEN_NS: u64 = (20_662 * DAY_S + 22 * 3_600) * 1_000_000_000;

/// Engine apply-slice budget the synchronous checkpoint must not blow (ENGINE.md).
const SLICE_BUDGET_MS: f64 = 4.0;

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

fn es_meta() -> InstrumentMeta {
    InstrumentMeta {
        symbol: "ESU6".into(),
        instrument_id: 42,
        dataset: "GLBX.MDP3".into(),
        min_price_increment: Price(TICK),
        unit_of_measure_qty: 50_000_000_000,
        display_factor: 1,
        trade_date: TRADE_DATE,
        session_open: Ts(SESSION_OPEN_NS),
    }
}

/// Dense 512-level book + 5-day week profile (same shape as `benches/snapshot.rs`),
/// plus modest Cancel/Fill activity so FLOW is non-trivial.
fn populated() -> (Book, MultiProfile) {
    let mut book = Book::new(Price(TICK));
    let mut seq = 0u32;
    for offset in 0..512u32 {
        seq += 1;
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
            seq,
            u64::from(offset) + 1,
        ));
    }
    // Pull / trade-at-touch samples across both sides (FLOW section payload).
    // Bid adds used ids 1..=256; Ask adds used ids 257..=512 — keep id/side aligned.
    for offset in 0..64u32 {
        seq += 1;
        let (side, add_offset) = if offset < 32 {
            (Side::Bid, offset)
        } else {
            (Side::Ask, 256 + (offset - 32))
        };
        let id = u64::from(add_offset) + 1;
        let tick = if side == Side::Bid {
            BASE - 1 - i64::from(add_offset)
        } else {
            BASE + 1 + i64::from(add_offset - 256)
        };
        let ts = 1_000 + u64::from(offset);
        if offset % 2 == 0 {
            book.apply(&event(
                EventKind::Cancel,
                side,
                tick,
                1 + offset % 5,
                ts,
                seq,
                id,
            ));
        } else {
            book.apply(&event(
                EventKind::Fill,
                side,
                tick,
                1 + offset % 3,
                ts,
                seq,
                id,
            ));
        }
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

/// Public equivalent of `fft_engine::checkpoint::write_state_checkpoint`
/// (`pub(crate)` — not callable from this bench crate).
fn write_public_state_checkpoint(
    writer: &mut LogWriter,
    book: &Book,
    profile: &MultiProfile,
) -> Result<(), fft_log::LogError> {
    let book_bytes = book.serialize_book();
    let flow_bytes = book.serialize_flow();
    let refresh_bytes = book.serialize_refresh();
    let secs = profile.serialize();
    writer.write_checkpoint([
        SectionRef {
            id: BOOK_SECTION_ID,
            version: BOOK_SECTION_VERSION,
            flags: 0,
            bytes: &book_bytes,
        },
        SectionRef {
            id: FLOW_SECTION_ID,
            version: FLOW_SECTION_VERSION,
            flags: 0,
            bytes: &flow_bytes,
        },
        SectionRef {
            id: PROFILE_SECTION_ID,
            version: PROFILE_SECTION_VERSION,
            flags: 0,
            bytes: &secs.profile,
        },
        SectionRef {
            id: CVD_SECTION_ID,
            version: CVD_SECTION_VERSION,
            flags: 0,
            bytes: &secs.cvd,
        },
        SectionRef {
            id: REFRESH_SECTION_ID,
            version: REFRESH_SECTION_VERSION,
            flags: 0,
            bytes: &refresh_bytes,
        },
        SectionRef {
            id: SESSION_SECTION_ID,
            version: SESSION_SECTION_VERSION,
            flags: 0,
            bytes: &secs.session,
        },
    ])
}

fn print_section_sizes(book: &Book, profile: &MultiProfile) {
    let book_bytes = book.serialize_book();
    let flow_bytes = book.serialize_flow();
    let refresh_bytes = book.serialize_refresh();
    let secs = profile.serialize();
    let total = book_bytes.len()
        + flow_bytes.len()
        + refresh_bytes.len()
        + secs.profile.len()
        + secs.cvd.len()
        + secs.session.len();
    println!(
        "live_checkpoint section bytes (uncompressed payloads): \
         book={} flow={} refresh={} profile={} cvd={} session={} total={} \
         slice_budget_ms={SLICE_BUDGET_MS} \
         note=\"write_state_checkpoint is pub(crate); measured public LogWriter::write_checkpoint equivalent\"",
        book_bytes.len(),
        flow_bytes.len(),
        refresh_bytes.len(),
        secs.profile.len(),
        secs.cvd.len(),
        secs.session.len(),
        total,
    );
}

fn live_checkpoint_benchmark(c: &mut Criterion) {
    let (book, profiles) = populated();
    print_section_sizes(&book, &profiles);

    let mut group = c.benchmark_group("live_checkpoint");
    group.warm_up_time(Duration::from_secs(1));
    group.measurement_time(Duration::from_secs(2));
    group.sample_size(20);

    group.bench_function("serialize_book", |b| {
        b.iter(|| black_box(book.serialize_book()));
    });
    group.bench_function("serialize_flow", |b| {
        b.iter(|| black_box(book.serialize_flow()));
    });
    group.bench_function("serialize_refresh", |b| {
        b.iter(|| black_box(book.serialize_refresh()));
    });
    group.bench_function("serialize_multiprofile", |b| {
        b.iter(|| black_box(profiles.serialize()));
    });

    let dir = TempDir::new().expect("tempdir");
    let path = dir.path().join("live_checkpoint.fftlog");
    let mut writer = LogWriter::create(&path, &es_meta()).expect("create live log");
    group.bench_function("six_section_checkpoint_write", |b| {
        b.iter(|| {
            write_public_state_checkpoint(
                black_box(&mut writer),
                black_box(&book),
                black_box(&profiles),
            )
            .expect("checkpoint write");
        });
    });
    // Leave LIVE (no close): Drop is fine for a temp bench file; close would sync.
    drop(writer);
    drop(dir);

    group.finish();
}

criterion_group!(benches, live_checkpoint_benchmark);
criterion_main!(benches);
