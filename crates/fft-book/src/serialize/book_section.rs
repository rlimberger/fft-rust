//! BOOK section v3: book metadata and resting FIFO topology only.

use super::RestoreError;
use super::codec::{Reader, w8, w16, w32, w64, wi64, wopt_i64, wopt_u64};
use crate::BOOK_SECTION_VERSION;
use crate::book::Book;
use crate::hasher::{OrderIdMap, order_id_map_new};
use crate::level::{NIL, Order, OrderOrigin};
use crate::side::{SideBook, WINDOW_TICKS, link_tail};
use fft_core::Side;
use slab::Slab;

const MAX_LEVELS: u32 = 1_000_000;
const MAX_ORDERS: u32 = 10_000_000;

pub(super) fn serialize(book: &Book) -> Vec<u8> {
    let mut bytes = Vec::new();
    w16(&mut bytes, BOOK_SECTION_VERSION);
    wi64(&mut bytes, book.tick);
    w64(&mut bytes, book.now);
    wopt_u64(&mut bytes, book.last_seq);
    w8(&mut bytes, u8::from(book.gap_pending));
    match book.last_gap {
        Some((expected, observed)) => {
            w8(&mut bytes, 1);
            w64(&mut bytes, expected);
            w64(&mut bytes, observed);
        }
        None => {
            w8(&mut bytes, 0);
            w64(&mut bytes, 0);
            w64(&mut bytes, 0);
        }
    }
    w64(&mut bytes, book.unknown_refs);
    match book.last_trade {
        Some((price, size, aggressor)) => {
            w8(&mut bytes, 1);
            wi64(&mut bytes, price);
            w32(&mut bytes, size);
            w8(&mut bytes, aggressor as u8);
        }
        None => {
            w8(&mut bytes, 0);
            wi64(&mut bytes, 0);
            w32(&mut bytes, 0);
            w8(&mut bytes, 0);
        }
    }
    write_side(&mut bytes, &book.bids, &book.orders);
    write_side(&mut bytes, &book.asks, &book.orders);
    bytes
}

fn write_side(bytes: &mut Vec<u8>, side: &SideBook, orders: &Slab<Order>) {
    w8(bytes, u8::from(side.initialised));
    wi64(bytes, side.base_tick);
    wopt_i64(bytes, side.best);
    w32(
        bytes,
        u32::try_from(side.populated).expect("fft-book: level count exceeds u32"),
    );
    side.try_for_each_populated(|price, level| {
        wi64(bytes, price);
        w32(bytes, level.order_count);
        let mut slot = level.head;
        while slot != NIL {
            let order = &orders[slot as usize];
            w64(bytes, order.id);
            w32(bytes, order.size);
            w64(bytes, order.ts);
            w32(bytes, order.epoch);
            w8(
                bytes,
                match order.origin {
                    OrderOrigin::Live => 0,
                    OrderOrigin::Snapshot => 1,
                },
            );
            slot = order.next;
        }
        true
    });
}

pub(super) fn restore(bytes: &[u8]) -> Result<Book, RestoreError> {
    let mut reader = Reader::new(bytes, "BOOK");
    let version = reader.u16()?;
    if version != BOOK_SECTION_VERSION {
        return Err(RestoreError::UnsupportedVersion {
            section: "BOOK",
            version,
        });
    }
    let tick = reader.i64()?;
    if tick <= 0 {
        return Err(reader.corrupt("non-positive tick"));
    }
    let now = reader.u64()?;
    let last_seq = reader.opt_u64()?;
    if last_seq == Some(0) {
        return Err(reader.corrupt("zero last sequence"));
    }
    let gap_pending = reader.boolean()?;
    let last_gap = read_last_gap(&mut reader)?;
    let unknown_refs = reader.u64()?;
    let last_trade = read_last_trade(&mut reader)?;

    let mut orders = Slab::new();
    let mut index = order_id_map_new();
    let bids = read_side(&mut reader, Side::Bid, tick, &mut orders, &mut index)?;
    let asks = read_side(&mut reader, Side::Ask, tick, &mut orders, &mut index)?;
    reader.finish()?;

    Ok(Book {
        tick,
        bids,
        asks,
        orders,
        index,
        refresh: Default::default(),
        tai: Default::default(),
        last_trade,
        now,
        since_gc: 0,
        last_seq,
        gap_pending,
        last_gap,
        unknown_refs,
        // Runtime block-framing / gap-desync counters, not seek-relevant —
        // restart at 0. Per-order gap taint reconstructs from order.epoch vs
        // REFRESH.gap_epoch (and gap_pending when the book is mid-gap).
        snapshot_clears: 0,
        gap_desync_cancels: 0,
        gap_desync_modifies: 0,
    })
}

fn read_last_gap(reader: &mut Reader<'_>) -> Result<Option<(u64, u64)>, RestoreError> {
    let present = reader.boolean()?;
    let expected = reader.u64()?;
    let observed = reader.u64()?;
    if !present && (expected != 0 || observed != 0) {
        return Err(reader.corrupt("nonzero absent gap"));
    }
    if present && (expected == 0 || observed == 0) {
        return Err(reader.corrupt("zero gap sequence"));
    }
    Ok(present.then_some((expected, observed)))
}

fn read_last_trade(reader: &mut Reader<'_>) -> Result<Option<(i64, u32, Side)>, RestoreError> {
    let present = reader.boolean()?;
    let price = reader.i64()?;
    let size = reader.u32()?;
    let aggressor =
        Side::from_u8(reader.u8()?).ok_or_else(|| reader.corrupt("invalid trade aggressor"))?;
    if !present && (price != 0 || size != 0 || aggressor != Side::None) {
        return Err(reader.corrupt("nonzero absent trade"));
    }
    Ok(present.then_some((price, size, aggressor)))
}

fn read_side(
    reader: &mut Reader<'_>,
    side: Side,
    tick: i64,
    orders: &mut Slab<Order>,
    index: &mut OrderIdMap<u32>,
) -> Result<SideBook, RestoreError> {
    let initialised = reader.boolean()?;
    let base_tick = reader.i64()?;
    let encoded_best = reader.opt_i64()?;
    let level_count = reader.count_with_min(MAX_LEVELS, 37)?;
    if !initialised && (base_tick != 0 || encoded_best.is_some() || level_count != 0) {
        return Err(reader.corrupt("uninitialised side carries state"));
    }

    let mut out = SideBook::new(side == Side::Bid);
    out.initialised = initialised;
    out.base_tick = base_tick;
    if initialised && base_tick.checked_add(WINDOW_TICKS as i64).is_none() {
        return Err(reader.corrupt("side window price overflow"));
    }
    let mut previous_price = None;
    for _ in 0..level_count {
        let price = reader.i64()?;
        if price.checked_sub(base_tick).is_none() {
            return Err(reader.corrupt("level price range overflow"));
        }
        if price.checked_mul(tick).is_none() {
            return Err(reader.corrupt("level price exceeds fixed-point range"));
        }
        if let Some(previous) = previous_price {
            let ordered = if side == Side::Bid {
                price < previous
            } else {
                price > previous
            };
            if !ordered {
                return Err(reader.corrupt("level prices out of order"));
            }
        }
        previous_price = Some(price);
        let order_count = reader.count_with_min(MAX_ORDERS, 25)?;
        if order_count == 0 {
            return Err(reader.corrupt("empty BOOK level"));
        }
        let mut live_seen = false;
        for _ in 0..order_count {
            read_order(reader, &mut out, side, price, orders, index, &mut live_seen)?;
        }
    }
    out.best = out.scan_best();
    if out.best != encoded_best {
        return Err(reader.corrupt("best price does not match levels"));
    }
    Ok(out)
}

fn read_order(
    reader: &mut Reader<'_>,
    side_book: &mut SideBook,
    side: Side,
    price: i64,
    orders: &mut Slab<Order>,
    index: &mut OrderIdMap<u32>,
    live_seen: &mut bool,
) -> Result<(), RestoreError> {
    let id = reader.u64()?;
    let size = reader.u32()?;
    let ts = reader.u64()?;
    let epoch = reader.u32()?;
    let origin = match reader.u8()? {
        0 => OrderOrigin::Live,
        1 => OrderOrigin::Snapshot,
        _ => return Err(reader.corrupt("invalid order origin")),
    };
    if id == 0 {
        return Err(reader.corrupt("zero order id"));
    }
    if size == 0 {
        return Err(reader.corrupt("zero-size resting order"));
    }
    match origin {
        OrderOrigin::Snapshot if *live_seen => {
            return Err(reader.corrupt("snapshot order follows live FIFO order"));
        }
        OrderOrigin::Live => *live_seen = true,
        OrderOrigin::Snapshot => {}
    }
    let order = Order {
        id,
        price,
        side,
        size,
        ts,
        epoch,
        origin,
        prev: NIL,
        next: NIL,
    };
    let slot = u32::try_from(orders.insert(order))
        .map_err(|_| reader.corrupt("order slot exceeds u32"))?;
    if index.insert(id, slot).is_some() {
        return Err(reader.corrupt("duplicate order id"));
    }
    link_tail(side_book, orders, slot);
    if origin == OrderOrigin::Snapshot {
        side_book
            .level_mut(price)
            .ok_or_else(|| reader.corrupt("missing restored level"))?
            .snapshot_tail = slot;
    }
    Ok(())
}
