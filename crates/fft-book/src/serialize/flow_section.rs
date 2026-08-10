//! FLOW section v1: fresh five-second windows plus cB/cA.

use super::RestoreError;
use super::codec::{Reader, w16, w32, w64, wi64, wopt_i64};
use crate::FLOW_SECTION_VERSION;
use crate::book::Book;
use crate::flow::{FLOW_BUCKETS, Flow, TradedAtInsideTicks};
use crate::side::SideBook;
use fft_core::Side;

const MAX_FLOW_LEVELS: u32 = 1_000_000;

pub(super) fn serialize(book: &Book) -> Vec<u8> {
    let mut bytes = Vec::new();
    w16(&mut bytes, FLOW_SECTION_VERSION);
    wopt_i64(&mut bytes, book.tai.bid_price);
    w64(&mut bytes, book.tai.bid_vol);
    wopt_i64(&mut bytes, book.tai.ask_price);
    w64(&mut bytes, book.tai.ask_vol);
    write_side(&mut bytes, &book.bids, book.now);
    write_side(&mut bytes, &book.asks, book.now);
    bytes
}

fn write_side(bytes: &mut Vec<u8>, side: &SideBook, now: u64) {
    let mut count = 0u32;
    side.for_each_alive(now, |_, level| {
        if !level.flow.untouched() && !level.flow.stale(now) {
            count = count
                .checked_add(1)
                .expect("fft-book: flow level count exceeds u32");
        }
    });
    w32(bytes, count);
    side.for_each_alive(now, |price, level| {
        if level.flow.untouched() || level.flow.stale(now) {
            return;
        }
        wi64(bytes, price);
        write_flow(bytes, &level.flow);
    });
}

fn write_flow(bytes: &mut Vec<u8>, flow: &Flow) {
    w64(bytes, flow.last_bucket);
    for values in [&flow.added, &flow.cancelled, &flow.traded] {
        for value in values {
            w32(bytes, *value);
        }
    }
}

pub(super) fn restore_into(bytes: &[u8], book: &mut Book) -> Result<(), RestoreError> {
    let mut reader = Reader::new(bytes, "FLOW");
    let version = reader.u16()?;
    if version != FLOW_SECTION_VERSION {
        return Err(RestoreError::UnsupportedVersion {
            section: "FLOW",
            version,
        });
    }
    let bid_price = reader.opt_i64()?;
    let bid_vol = reader.u64()?;
    let ask_price = reader.opt_i64()?;
    let ask_vol = reader.u64()?;
    if (bid_price.is_none() && bid_vol != 0) || (ask_price.is_none() && ask_vol != 0) {
        return Err(reader.corrupt("volume without cB/cA price"));
    }
    book.tai = TradedAtInsideTicks {
        bid_price,
        bid_vol,
        ask_price,
        ask_vol,
    };
    read_side(&mut reader, &mut book.bids, Side::Bid, book.tick, book.now)?;
    read_side(&mut reader, &mut book.asks, Side::Ask, book.tick, book.now)?;
    reader.finish()
}

fn read_side(
    reader: &mut Reader<'_>,
    side_book: &mut SideBook,
    side: Side,
    tick: i64,
    now: u64,
) -> Result<(), RestoreError> {
    let count = reader.count_with_min(MAX_FLOW_LEVELS, 136)?;
    if count > 0 && !side_book.initialised {
        return Err(reader.corrupt("flow exists on uninitialised side"));
    }
    let mut previous_price = None;
    for _ in 0..count {
        let price = reader.i64()?;
        if price.checked_sub(side_book.base_tick).is_none() {
            return Err(reader.corrupt("flow price range overflow"));
        }
        if price.checked_mul(tick).is_none() {
            return Err(reader.corrupt("flow price exceeds fixed-point range"));
        }
        if let Some(previous) = previous_price {
            let ordered = if side == Side::Bid {
                price < previous
            } else {
                price > previous
            };
            if !ordered {
                return Err(reader.corrupt("flow prices out of order"));
            }
        }
        previous_price = Some(price);
        let flow = read_flow(reader)?;
        if flow.untouched() {
            return Err(reader.corrupt("untouched flow entry"));
        }
        if flow.last_bucket > now / crate::flow::FLOW_BUCKET_NS {
            return Err(reader.corrupt("flow bucket is ahead of book time"));
        }
        if flow.stale(now) {
            return Err(reader.corrupt("stale flow entry"));
        }
        let level = side_book.level_entry(price);
        if !level.flow.untouched() {
            return Err(reader.corrupt("duplicate flow entry"));
        }
        level.flow = flow;
    }
    Ok(())
}

fn read_flow(reader: &mut Reader<'_>) -> Result<Flow, RestoreError> {
    let mut flow = Flow {
        last_bucket: reader.u64()?,
        ..Flow::default()
    };
    for values in [&mut flow.added, &mut flow.cancelled, &mut flow.traded] {
        for value in values.iter_mut().take(FLOW_BUCKETS) {
            *value = reader.u32()?;
        }
    }
    Ok(flow)
}
