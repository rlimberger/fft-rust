//! DBN MBO → `fft_core::CanonicalEvent` (`docs/FFTLOG-V2.md` §4). Every mapping fact is
//! verified against the pinned `dbn-0.65.0` source, cited per item below. Records this
//! module cannot map are loud errors carrying the raw record — never skipped.
//!
//! Mapping (citations are paths inside the `dbn-0.65.0` crate source):
//!
//! | DBN | Canonical | Source |
//! |---|---|---|
//! | action `'A'` Add | `EventKind::Add` | `src/enums.rs:331-333` |
//! | action `'C'` Cancel (full or partial) | `EventKind::Cancel` | `src/enums.rs:328-330` |
//! | action `'M'` Modify (price and/or size) | `EventKind::Modify` | `src/enums.rs:319-321` |
//! | action `'T'` Trade (aggressor; book unaffected) | `EventKind::Trade` | `src/enums.rs:322-324` |
//! | action `'F'` Fill (resting order) | `EventKind::Fill` | `src/enums.rs:325-327` |
//! | action `'R'` cleaR book | `EventKind::Clear` | `src/enums.rs:334-336` |
//! | action `'N'` None ("no effect on the book") | **loud error** — no canonical carrier | `src/enums.rs:337-340` |
//! | side `'B'`/`'A'`/`'N'` | `Side::Bid`/`Ask`/`None` | `src/enums.rs:273-284` |
//! | `price: i64`, 1e-9 fixed point | `Price` verbatim (same scale) | `src/record.rs:82-87`, `FIXED_PRICE_SCALE` `src/lib.rs:150` |
//! | `size: u32` | `size` verbatim | `src/record.rs:88-90` |
//! | `order_id: u64` (venue-assigned) | `OrderId` verbatim | `src/record.rs:80-81` |
//! | `sequence: u32` (venue-assigned) | `Seq` verbatim | `src/record.rs:119-120` |
//! | `hd.ts_event: u64` UTC ns | `Ts` verbatim (canonical basis) | `src/record.rs:59-64` |
//! | `flags: FlagSet` (`u8` bit field) | `flags` zero-extended to `u16` | `src/flags.rs:9-23`, `src/record.rs:91-93` |
//!
//! `EventKind::Status` (wire 7) carries the DBN `status`-schema code; batch job
//! GLBX-20260803-4WJS899FNL is `mbo`-only, so this decoder never emits it.
//!
//! `UNDEF_PRICE = i64::MAX` and `UNDEF_ORDER_SIZE = u32::MAX` (`src/lib.rs:152,154`) are
//! legal only on `'R'` clear records, where the book fields are meaningless; they are
//! normalized to zero there and are loud errors anywhere else.

use dbn::{MboMsg, UNDEF_ORDER_SIZE, UNDEF_PRICE};
use fft_core::{CanonicalEvent, EventKind, OrderId, Price, Seq, Side, Ts};

use super::error::IngestError;

/// Map one DBN MBO record to a canonical event. Pure; no sequence accounting.
pub fn map_mbo(rec: &MboMsg) -> Result<CanonicalEvent, IngestError> {
    let raw = || format!("{rec:?}");
    let kind = match u8::try_from(rec.action).unwrap_or(0) {
        b'A' => EventKind::Add,
        b'C' => EventKind::Cancel,
        b'M' => EventKind::Modify,
        b'T' => EventKind::Trade,
        b'F' => EventKind::Fill,
        b'R' => EventKind::Clear,
        action => {
            return Err(IngestError::UnmappableAction {
                action,
                record: raw(),
            });
        }
    };
    let side = match u8::try_from(rec.side).unwrap_or(0) {
        b'B' => Side::Bid,
        b'A' => Side::Ask,
        b'N' => Side::None,
        side => {
            return Err(IngestError::UnmappableSide {
                side,
                record: raw(),
            });
        }
    };
    let clear = kind == EventKind::Clear;
    let price = match rec.price {
        UNDEF_PRICE if clear => 0,
        UNDEF_PRICE => {
            return Err(IngestError::UndefinedField {
                field: "price",
                record: raw(),
            });
        }
        price => price,
    };
    let size = match rec.size {
        UNDEF_ORDER_SIZE if clear => 0,
        UNDEF_ORDER_SIZE => {
            return Err(IngestError::UndefinedField {
                field: "size",
                record: raw(),
            });
        }
        size => size,
    };
    Ok(CanonicalEvent {
        kind,
        side,
        flags: u16::from(rec.flags.raw()),
        size,
        ts: Ts(rec.hd.ts_event),
        seq: Seq(rec.sequence),
        price: Price(price),
        order_id: OrderId(rec.order_id),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use dbn::record::RecordHeader;
    use dbn::rtype;

    fn mbo(action: u8, side: u8, seq: u32) -> MboMsg {
        MboMsg {
            hd: RecordHeader::new::<MboMsg>(rtype::MBO, 1, 42, 1_785_276_000_000_000_000),
            order_id: 647_399_625_133,
            price: 6_420_250_000_000,
            size: 5,
            flags: dbn::FlagSet::from(130),
            channel_id: 0,
            action: action as std::ffi::c_char,
            side: side as std::ffi::c_char,
            ts_recv: 1_785_276_000_000_000_100,
            ts_in_delta: 100,
            sequence: seq,
        }
    }

    #[test]
    fn maps_all_book_actions() {
        for (action, kind) in [
            (b'A', EventKind::Add),
            (b'C', EventKind::Cancel),
            (b'M', EventKind::Modify),
            (b'T', EventKind::Trade),
            (b'F', EventKind::Fill),
            (b'R', EventKind::Clear),
        ] {
            let ev = map_mbo(&mbo(action, b'B', 7)).unwrap();
            assert_eq!(ev.kind, kind, "action {}", char::from(action));
            assert_eq!(ev.side, Side::Bid);
            assert_eq!(ev.flags, 130);
            assert_eq!(ev.size, 5);
            assert_eq!(ev.ts, Ts(1_785_276_000_000_000_000)); // ts_event, never ts_recv
            assert_eq!(ev.seq, Seq(7));
            assert_eq!(ev.price, Price(6_420_250_000_000));
            assert_eq!(ev.order_id, OrderId(647_399_625_133));
        }
    }

    #[test]
    fn action_none_is_a_loud_error() {
        let err = map_mbo(&mbo(b'N', b'N', 7)).unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("unmappable DBN action 'N'"), "{msg}");
        assert!(msg.contains("647399625133"), "raw record missing: {msg}");
    }

    #[test]
    fn undef_price_only_legal_on_clear() {
        let mut rec = mbo(b'R', b'N', 7);
        rec.price = UNDEF_PRICE;
        rec.size = UNDEF_ORDER_SIZE;
        let ev = map_mbo(&rec).unwrap();
        assert_eq!(
            (ev.kind, ev.price, ev.size),
            (EventKind::Clear, Price(0), 0)
        );

        let mut rec = mbo(b'A', b'B', 7);
        rec.price = UNDEF_PRICE;
        let msg = map_mbo(&rec).unwrap_err().to_string();
        assert!(msg.contains("undefined price"), "{msg}");
    }
}
