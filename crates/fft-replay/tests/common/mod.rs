//! Shared fftlog fixtures with real BOOK/PROFILE/CVD checkpoints.

#![allow(dead_code)]

use fft_book::{
    BOOK_SECTION_ID, BOOK_SECTION_VERSION, Book, FLOW_SECTION_ID, FLOW_SECTION_VERSION,
    REFRESH_SECTION_ID, REFRESH_SECTION_VERSION,
};
use fft_core::{CanonicalEvent, EventKind, InstrumentMeta, OrderId, Price, Seq, Side, Ts};
use fft_log::{LogWriter, SectionRef};
use fft_profile::{
    CVD_SECTION_ID, CVD_SECTION_VERSION, MultiProfile, PROFILE_SECTION_ID, PROFILE_SECTION_VERSION,
};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

/// ES tick: 0.25 in 1e-9 units.
pub const TICK: i64 = 250_000_000;
/// Wed 2026-07-29 CT trade date.
pub const TRADE_DATE: u32 = 20_663;
const DAY_S: u64 = 86_400;
/// Globex open: Tue 2026-07-28 17:00 CDT = 22:00 UTC.
pub const SESSION_OPEN_NS: u64 = (20_662 * DAY_S + 22 * 3_600) * 1_000_000_000;

pub struct TempPath(pub PathBuf);

impl TempPath {
    pub fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TempPath {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

pub fn temp_path(name: &str) -> TempPath {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    TempPath(std::env::temp_dir().join(format!(
        "fft-replay-test-{}-{n}-{name}.fftlog",
        std::process::id()
    )))
}

pub fn es_meta() -> InstrumentMeta {
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

pub fn px(ticks: i64) -> Price {
    Price(ticks * TICK)
}

pub fn add(id: u64, side: Side, ticks: i64, size: u32, ts: u64, seq: u32) -> CanonicalEvent {
    CanonicalEvent {
        kind: EventKind::Add,
        side,
        flags: 0,
        size,
        ts: Ts(ts),
        seq: Seq(seq),
        price: px(ticks),
        order_id: OrderId(id),
    }
}

pub fn trade(aggressor: Side, ticks: i64, size: u32, ts: u64, seq: u32) -> CanonicalEvent {
    CanonicalEvent {
        kind: EventKind::Trade,
        side: aggressor,
        flags: 0,
        size,
        ts: Ts(ts),
        seq: Seq(seq),
        price: px(ticks),
        order_id: OrderId(0),
    }
}

/// Scripted tape: adds + trades, with checkpoints after `checkpoint_every` applied events.
pub fn write_checkpointed_log(path: &Path, event_count: usize, checkpoint_every: usize) {
    assert!(event_count > 0);
    assert!(checkpoint_every > 0);
    let meta = es_meta();
    let mut writer = LogWriter::create(path, &meta).expect("create log");
    let mut book = Book::new(meta.min_price_increment);
    let mut profile = MultiProfile::new(meta.min_price_increment);
    profile.begin_session(meta.trade_date);

    let mut batch = Vec::new();
    let mut applied = 0usize;
    for i in 0..event_count {
        let seq = (i as u32) + 1;
        let ts = SESSION_OPEN_NS + (i as u64) * 1_000_000;
        let event = if i % 5 == 4 {
            trade(
                if i % 2 == 0 { Side::Bid } else { Side::Ask },
                20_000 + (i as i64 % 7) - 3,
                1 + (i as u32 % 5),
                ts,
                seq,
            )
        } else {
            let side = if i % 2 == 0 { Side::Bid } else { Side::Ask };
            let ticks = if side == Side::Bid {
                20_000 - 1 - (i as i64 % 40)
            } else {
                20_000 + 1 + (i as i64 % 40)
            };
            add(u64::from(seq), side, ticks, 1 + (i as u32 % 10), ts, seq)
        };
        book.apply(&event);
        profile.apply(&event);
        batch.push(event);
        applied += 1;

        let boundary = applied.is_multiple_of(checkpoint_every) || i + 1 == event_count;
        if boundary {
            writer.append_events(&batch).expect("append events");
            batch.clear();
            if applied.is_multiple_of(checkpoint_every) && i + 1 != event_count {
                write_state_checkpoint(&mut writer, &book, &profile);
            }
        }
    }
    writer.close().expect("close log");
}

pub fn write_state_checkpoint(writer: &mut LogWriter, book: &Book, profile: &MultiProfile) {
    let book_bytes = book.serialize_book();
    let flow_bytes = book.serialize_flow();
    let refresh_bytes = book.serialize_refresh();
    let (profile_bytes, cvd_bytes) = profile.serialize();
    writer
        .write_checkpoint([
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
                bytes: &profile_bytes,
            },
            SectionRef {
                id: CVD_SECTION_ID,
                version: CVD_SECTION_VERSION,
                flags: 0,
                bytes: &cvd_bytes,
            },
            SectionRef {
                id: REFRESH_SECTION_ID,
                version: REFRESH_SECTION_VERSION,
                flags: 0,
                bytes: &refresh_bytes,
            },
        ])
        .expect("write checkpoint");
}

pub fn forward_to(
    source: &mut fft_replay::ReplaySource,
    book: &mut Book,
    profile: &mut MultiProfile,
    target_ts: u64,
) {
    while let Some(next) = source.peek_event().expect("peek") {
        if next.ts.0 > target_ts {
            break;
        }
        source
            .apply_next(book, profile)
            .expect("apply")
            .expect("event present");
    }
    book.check_invariants();
}
