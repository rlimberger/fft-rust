//! Event payload codec (`docs/FFTLOG-V2.md` §4): fixed 32-byte records with
//! frame-relative `u32` timestamp deltas and TsReset basis records.
//!
//! TsReset (wire kind 9) is a framing artifact owned by this crate: the encoder emits
//! one whenever the next event's timestamp cannot be expressed as `basis + u32` (jump
//! past `u32::MAX` ns *or* a backwards step), and the decoder consumes it to move the
//! basis. It never escapes as a [`CanonicalEvent`].
//!
//! Ambiguity resolved: the spec says a TsReset "carries a full `u64` basis in
//! `price`+`order_id`". This implementation stores the basis in `order_id` and the same
//! bit pattern in `price` (as `i64`); the decoder cross-checks the two copies and fails
//! loudly on mismatch. All other TsReset fields must be zero.

use fft_core::{CanonicalEvent, EventKind, OrderId, Price, Seq, Side, Ts};

use crate::error::{LogError, Result};

/// Fixed wire size of one event record (§4).
pub const EVENT_RECORD_LEN: usize = 32;
/// Wire kind of the fft-log-internal timestamp-basis reset record.
const KIND_TS_RESET: u8 = 9;

/// An encoded EVENTS payload plus the frame-header fields derived while encoding.
pub(crate) struct EncodedEvents {
    /// Uncompressed payload: `count` × 32-byte records.
    pub payload: Vec<u8>,
    /// Wire record count, **including** TsReset records.
    pub count: u32,
    /// Timestamp of the first event; the initial delta basis.
    pub first_ts: u64,
    /// Maximum event timestamp in the batch.
    pub last_ts: u64,
    /// Sequence of the first event.
    pub first_seq: u64,
    /// Sequence of the last event.
    pub last_seq: u64,
}

// Parameters mirror the frozen §4 record table one-to-one.
#[allow(clippy::too_many_arguments)]
fn put_record(
    out: &mut Vec<u8>,
    kind: u8,
    side: u8,
    flags: u16,
    size: u32,
    ts_delta: u32,
    seq: u32,
    price: i64,
    order_id: u64,
) {
    out.push(kind);
    out.push(side);
    out.extend_from_slice(&flags.to_le_bytes());
    out.extend_from_slice(&size.to_le_bytes());
    out.extend_from_slice(&ts_delta.to_le_bytes());
    out.extend_from_slice(&seq.to_le_bytes());
    out.extend_from_slice(&price.to_le_bytes());
    out.extend_from_slice(&order_id.to_le_bytes());
}

/// Encode a batch into one EVENTS payload. Empty batches yield an empty payload with
/// zeroed bounds. `first_ts` is always the first event's timestamp (the delta basis),
/// `last_ts` the maximum timestamp — identical on the monotonic feeds we log.
pub(crate) fn encode_events(events: &[CanonicalEvent]) -> EncodedEvents {
    let first_ts = events.first().map_or(0, |e| e.ts.0);
    let mut out = Vec::with_capacity(events.len() * EVENT_RECORD_LEN);
    let mut basis = first_ts;
    let mut last_ts = first_ts;
    let mut count: u32 = 0;
    for e in events {
        let delta = match e.ts.0.checked_sub(basis) {
            Some(d) if d <= u64::from(u32::MAX) => d as u32,
            _ => {
                // Jump past u32::MAX ns or a backwards step: reset the basis.
                basis = e.ts.0;
                put_record(
                    &mut out,
                    KIND_TS_RESET,
                    0,
                    0,
                    0,
                    0,
                    0,
                    i64::from_le_bytes(basis.to_le_bytes()),
                    basis,
                );
                count += 1;
                0
            }
        };
        put_record(
            &mut out,
            e.kind as u8,
            e.side as u8,
            e.flags,
            e.size,
            delta,
            e.seq.0,
            e.price.0,
            e.order_id.0,
        );
        count += 1;
        last_ts = last_ts.max(e.ts.0);
    }
    EncodedEvents {
        payload: out,
        count,
        first_ts,
        last_ts,
        first_seq: events.first().map_or(0, |e| u64::from(e.seq.0)),
        last_seq: events.last().map_or(0, |e| u64::from(e.seq.0)),
    }
}

/// Decode an uncompressed EVENTS payload back to absolute-timestamp events.
///
/// `count` and `first_ts` come from the (already validated) frame header;
/// `frame_offset` is only for error context. TsReset records move the basis and are
/// consumed here — they never appear in the output.
pub(crate) fn decode_events(
    payload: &[u8],
    count: u32,
    first_ts: u64,
    frame_offset: u64,
) -> Result<Vec<CanonicalEvent>> {
    let expected_len = count as usize * EVENT_RECORD_LEN;
    if payload.len() != expected_len {
        return Err(LogError::BadPayload {
            offset: frame_offset,
            detail: format!(
                "payload is {} bytes, header count {count} requires {expected_len}",
                payload.len()
            ),
        });
    }
    let mut events = Vec::with_capacity(count as usize);
    let mut basis = first_ts;
    for (index, rec) in payload.chunks_exact(EVENT_RECORD_LEN).enumerate() {
        let index = index as u32;
        let bad = |detail: String| LogError::BadEventRecord {
            frame_offset,
            index,
            detail,
        };
        let kind = rec[0];
        let side = rec[1];
        let flags = u16::from_le_bytes(rec[2..4].try_into().unwrap());
        let size = u32::from_le_bytes(rec[4..8].try_into().unwrap());
        let ts_delta = u32::from_le_bytes(rec[8..12].try_into().unwrap());
        let seq = u32::from_le_bytes(rec[12..16].try_into().unwrap());
        let price = i64::from_le_bytes(rec[16..24].try_into().unwrap());
        let order_id = u64::from_le_bytes(rec[24..32].try_into().unwrap());

        if kind == KIND_TS_RESET {
            if u64::from_le_bytes(price.to_le_bytes()) != order_id {
                return Err(bad(format!(
                    "TsReset basis copies disagree: price bits {price:#x} vs order_id {order_id:#x}"
                )));
            }
            if side != 0 || flags != 0 || size != 0 || ts_delta != 0 || seq != 0 {
                return Err(bad("TsReset record has non-zero non-basis fields".into()));
            }
            basis = order_id;
            continue;
        }
        let kind =
            EventKind::from_u8(kind).ok_or_else(|| bad(format!("unknown event kind {kind}")))?;
        let side = Side::from_u8(side).ok_or_else(|| bad(format!("unknown side {side}")))?;
        let ts = basis
            .checked_add(u64::from(ts_delta))
            .ok_or_else(|| bad(format!("ts basis {basis} + delta {ts_delta} overflows u64")))?;
        events.push(CanonicalEvent {
            kind,
            side,
            flags,
            size,
            ts: Ts(ts),
            seq: Seq(seq),
            price: Price(price),
            order_id: OrderId(order_id),
        });
    }
    Ok(events)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ev(ts: u64, seq: u32) -> CanonicalEvent {
        CanonicalEvent {
            kind: EventKind::Add,
            side: Side::Bid,
            flags: 0x80,
            size: 3,
            ts: Ts(ts),
            seq: Seq(seq),
            price: Price(5_000_250_000_000),
            order_id: OrderId(42),
        }
    }

    #[test]
    fn ts_reset_on_forward_jump_and_backwards_step() {
        let events = vec![
            ev(100, 1),
            ev(100 + u64::from(u32::MAX) + 1, 2),
            ev(50, 3),
            ev(60, 4),
        ];
        let enc = encode_events(&events);
        // Two resets: the forward jump and the backwards step to 50.
        assert_eq!(enc.count, 6);
        assert_eq!(enc.first_ts, 100);
        assert_eq!(enc.last_ts, 100 + u64::from(u32::MAX) + 1);
        let dec = decode_events(&enc.payload, enc.count, enc.first_ts, 0).unwrap();
        assert_eq!(dec, events);
    }

    #[test]
    fn empty_batch() {
        let enc = encode_events(&[]);
        assert_eq!(enc.count, 0);
        assert!(decode_events(&enc.payload, 0, 0, 0).unwrap().is_empty());
    }

    #[test]
    fn corrupt_kind_is_loud() {
        let enc = encode_events(&[ev(1, 1)]);
        let mut payload = enc.payload;
        payload[0] = 200;
        assert!(matches!(
            decode_events(&payload, enc.count, enc.first_ts, 7),
            Err(LogError::BadEventRecord {
                frame_offset: 7,
                index: 0,
                ..
            })
        ));
    }
}
