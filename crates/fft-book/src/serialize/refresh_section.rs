//! REFRESH section v1: native-refresh classification state only.

use super::RestoreError;
use super::codec::{Reader, w8, w16, w32, w64, wi64};
use crate::PriceRefreshAgg;
use crate::REFRESH_SECTION_VERSION;
use crate::book::Book;
use crate::refresh::{LiveRefresh, RefreshTracker, Tombstone};
use fft_core::Side;

const MAX_REFRESH_ENTRIES: u32 = 10_000_000;

pub(super) fn serialize(book: &Book) -> Vec<u8> {
    let mut bytes = Vec::new();
    w16(&mut bytes, REFRESH_SECTION_VERSION);
    w32(&mut bytes, book.refresh.gap_epoch);

    let mut live: Vec<_> = book.refresh.live.iter().collect();
    live.sort_by_key(|(id, _)| **id);
    w32(
        &mut bytes,
        u32::try_from(live.len()).expect("fft-book: refresh entry count exceeds u32"),
    );
    for (id, state) in live {
        w64(&mut bytes, *id);
        w32(&mut bytes, state.reloads);
        w64(&mut bytes, state.hidden);
        w8(&mut bytes, u8::from(state.unavailable));
    }

    let mut tombstones: Vec<_> = book.refresh.tombstones.iter().collect();
    tombstones.sort_by_key(|(id, _)| **id);
    w32(
        &mut bytes,
        u32::try_from(tombstones.len()).expect("fft-book: tombstone count exceeds u32"),
    );
    for (id, tombstone) in tombstones {
        w64(&mut bytes, *id);
        w8(&mut bytes, tombstone.side as u8);
        wi64(&mut bytes, tombstone.price);
        w64(&mut bytes, tombstone.depleted_ts);
        w32(&mut bytes, tombstone.epoch);
        w32(&mut bytes, tombstone.reloads);
        w64(&mut bytes, tombstone.hidden);
    }

    w32(
        &mut bytes,
        u32::try_from(book.refresh.per_price.len())
            .expect("fft-book: refresh aggregate count exceeds u32"),
    );
    for (&(side, price), aggregate) in &book.refresh.per_price {
        w8(&mut bytes, side);
        wi64(&mut bytes, price);
        w32(&mut bytes, aggregate.refresh_count);
        w64(&mut bytes, aggregate.hidden_volume);
    }
    bytes
}

pub(super) fn restore(bytes: &[u8]) -> Result<RefreshTracker, RestoreError> {
    let mut reader = Reader::new(bytes, "REFRESH");
    let version = reader.u16()?;
    if version != REFRESH_SECTION_VERSION {
        return Err(RestoreError::UnsupportedVersion {
            section: "REFRESH",
            version,
        });
    }
    let mut tracker = RefreshTracker {
        gap_epoch: reader.u32()?,
        ..RefreshTracker::default()
    };
    read_live(&mut reader, &mut tracker)?;
    read_tombstones(&mut reader, &mut tracker)?;
    read_aggregates(&mut reader, &mut tracker)?;
    reader.finish()?;
    Ok(tracker)
}

fn read_live(reader: &mut Reader<'_>, tracker: &mut RefreshTracker) -> Result<(), RestoreError> {
    let count = reader.count_with_min(MAX_REFRESH_ENTRIES, 21)?;
    let mut previous = None;
    for _ in 0..count {
        let id = reader.u64()?;
        if id == 0 || previous.is_some_and(|value| id <= value) {
            return Err(reader.corrupt("live refresh ids not strictly ascending"));
        }
        previous = Some(id);
        let state = LiveRefresh {
            reloads: reader.u32()?,
            hidden: reader.u64()?,
            unavailable: reader.boolean()?,
        };
        if (state.reloads == 0 && state.hidden != 0)
            || (state.reloads > 0 && state.hidden == 0)
            || (!state.unavailable && state.reloads == 0)
        {
            return Err(reader.corrupt("inconsistent live refresh history"));
        }
        tracker.live.insert(id, state);
    }
    Ok(())
}

fn read_tombstones(
    reader: &mut Reader<'_>,
    tracker: &mut RefreshTracker,
) -> Result<(), RestoreError> {
    let count = reader.count_with_min(MAX_REFRESH_ENTRIES, 41)?;
    let mut previous = None;
    for _ in 0..count {
        let id = reader.u64()?;
        if id == 0 || previous.is_some_and(|value| id <= value) {
            return Err(reader.corrupt("tombstone ids not strictly ascending"));
        }
        previous = Some(id);
        let side = match Side::from_u8(reader.u8()?) {
            Some(Side::Bid) => Side::Bid,
            Some(Side::Ask) => Side::Ask,
            _ => return Err(reader.corrupt("invalid tombstone side")),
        };
        let tombstone = Tombstone {
            side,
            price: reader.i64()?,
            depleted_ts: reader.u64()?,
            epoch: reader.u32()?,
            reloads: reader.u32()?,
            hidden: reader.u64()?,
        };
        if (tombstone.reloads == 0 && tombstone.hidden != 0)
            || (tombstone.reloads > 0 && tombstone.hidden == 0)
        {
            return Err(reader.corrupt("inconsistent tombstone history"));
        }
        tracker.tombstones.insert(id, tombstone);
    }
    Ok(())
}

fn read_aggregates(
    reader: &mut Reader<'_>,
    tracker: &mut RefreshTracker,
) -> Result<(), RestoreError> {
    let count = reader.count_with_min(MAX_REFRESH_ENTRIES, 21)?;
    let mut previous = None;
    for _ in 0..count {
        let side = match Side::from_u8(reader.u8()?) {
            Some(Side::Bid) => Side::Bid,
            Some(Side::Ask) => Side::Ask,
            _ => return Err(reader.corrupt("invalid aggregate side")),
        };
        let price = reader.i64()?;
        let key = (side as u8, price);
        if previous.is_some_and(|value| key <= value) {
            return Err(reader.corrupt("aggregate keys not strictly ascending"));
        }
        previous = Some(key);
        let aggregate = PriceRefreshAgg {
            refresh_count: reader.u32()?,
            hidden_volume: reader.u64()?,
        };
        if aggregate.refresh_count == 0 {
            return Err(reader.corrupt("zero-count refresh aggregate"));
        }
        if aggregate.hidden_volume == 0 {
            return Err(reader.corrupt("zero-volume refresh aggregate"));
        }
        tracker.per_price.insert(key, aggregate);
    }
    Ok(())
}
