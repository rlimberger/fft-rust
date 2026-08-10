//! Frame header codec (`docs/FFTLOG-V2.md` §3): fixed 64-byte uncompressed headers so a
//! reader can walk the frame chain without decompressing anything. Both length ceilings
//! are enforced here, at decode time, **before** any payload allocation.

use xxhash_rust::xxh3::xxh3_64;

use crate::error::{LogError, Result};

/// Frame kind 1: zstd-compressed 32-byte event records (§4).
pub const KIND_EVENTS: u8 = 1;
/// Frame kind 2: zstd-compressed checkpoint sections (§5).
pub const KIND_CHECKPOINT: u8 = 2;

/// Fixed frame header size in bytes.
pub const FRAME_HEADER_LEN: usize = 64;
/// Frozen ceiling for `compressed_len` (§3).
pub const MAX_COMPRESSED_LEN: u32 = 16 * 1024 * 1024;
/// Frozen ceiling for `uncompressed_len` (§3).
pub const MAX_UNCOMPRESSED_LEN: u32 = 64 * 1024 * 1024;

/// Decoded 64-byte frame header. `header_xxh3` is validated at decode and recomputed at
/// encode, so it is not stored.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FrameHeader {
    /// [`KIND_EVENTS`] or [`KIND_CHECKPOINT`].
    pub kind: u8,
    /// Wire records in an EVENTS frame (§4, including TsReset records) or sections in a
    /// CHECKPOINT frame.
    pub count: u32,
    /// zstd payload length; ≤ [`MAX_COMPRESSED_LEN`].
    pub compressed_len: u32,
    /// Decompressed payload length; ≤ [`MAX_UNCOMPRESSED_LEN`].
    pub uncompressed_len: u32,
    /// Event-time lower bound; also the ts-delta basis of an EVENTS frame (§4).
    pub first_ts: u64,
    /// Event-time upper bound.
    pub last_ts: u64,
    /// Source-sequence bound.
    pub first_seq: u64,
    /// Source-sequence bound.
    pub last_seq: u64,
    /// xxh3-64 over the compressed payload.
    pub payload_xxh3: u64,
}

impl FrameHeader {
    /// Encode to the 64-byte wire form, computing `header_xxh3` over the first 56 bytes.
    pub fn encode(&self) -> [u8; FRAME_HEADER_LEN] {
        let mut out = [0u8; FRAME_HEADER_LEN];
        out[0] = self.kind;
        // out[1..4] reserved, zero.
        out[4..8].copy_from_slice(&self.count.to_le_bytes());
        out[8..12].copy_from_slice(&self.compressed_len.to_le_bytes());
        out[12..16].copy_from_slice(&self.uncompressed_len.to_le_bytes());
        out[16..24].copy_from_slice(&self.first_ts.to_le_bytes());
        out[24..32].copy_from_slice(&self.last_ts.to_le_bytes());
        out[32..40].copy_from_slice(&self.first_seq.to_le_bytes());
        out[40..48].copy_from_slice(&self.last_seq.to_le_bytes());
        out[48..56].copy_from_slice(&self.payload_xxh3.to_le_bytes());
        let checksum = xxh3_64(&out[..56]);
        out[56..64].copy_from_slice(&checksum.to_le_bytes());
        out
    }

    /// Decode and validate a frame header found at file offset `offset` (used only for
    /// error reporting). Checks, in order: `header_xxh3`, reserved bytes, kind, and both
    /// length ceilings — all before the caller touches (or allocates for) the payload.
    pub fn decode(bytes: &[u8; FRAME_HEADER_LEN], offset: u64) -> Result<FrameHeader> {
        let stored = u64::from_le_bytes(bytes[56..64].try_into().unwrap());
        if xxh3_64(&bytes[..56]) != stored {
            return Err(LogError::FrameHeaderChecksum { offset });
        }
        if bytes[1..4] != [0, 0, 0] {
            return Err(LogError::NonZeroReserved { offset });
        }
        let kind = bytes[0];
        if kind != KIND_EVENTS && kind != KIND_CHECKPOINT {
            return Err(LogError::BadFrameKind { offset, kind });
        }
        let compressed_len = u32::from_le_bytes(bytes[8..12].try_into().unwrap());
        if compressed_len > MAX_COMPRESSED_LEN {
            return Err(LogError::LimitExceeded {
                offset,
                field: "compressed_len",
                value: u64::from(compressed_len),
                max: u64::from(MAX_COMPRESSED_LEN),
            });
        }
        let uncompressed_len = u32::from_le_bytes(bytes[12..16].try_into().unwrap());
        if uncompressed_len > MAX_UNCOMPRESSED_LEN {
            return Err(LogError::LimitExceeded {
                offset,
                field: "uncompressed_len",
                value: u64::from(uncompressed_len),
                max: u64::from(MAX_UNCOMPRESSED_LEN),
            });
        }
        Ok(FrameHeader {
            kind,
            count: u32::from_le_bytes(bytes[4..8].try_into().unwrap()),
            compressed_len,
            uncompressed_len,
            first_ts: u64::from_le_bytes(bytes[16..24].try_into().unwrap()),
            last_ts: u64::from_le_bytes(bytes[24..32].try_into().unwrap()),
            first_seq: u64::from_le_bytes(bytes[32..40].try_into().unwrap()),
            last_seq: u64::from_le_bytes(bytes[40..48].try_into().unwrap()),
            payload_xxh3: u64::from_le_bytes(bytes[48..56].try_into().unwrap()),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> FrameHeader {
        FrameHeader {
            kind: KIND_EVENTS,
            count: 3,
            compressed_len: 100,
            uncompressed_len: 96,
            first_ts: 1_000,
            last_ts: 2_000,
            first_seq: 10,
            last_seq: 12,
            payload_xxh3: 0xdead_beef,
        }
    }

    #[test]
    fn round_trip() {
        let h = sample();
        let bytes = h.encode();
        assert_eq!(FrameHeader::decode(&bytes, 64).unwrap(), h);
    }

    #[test]
    fn checksum_flip_detected() {
        let mut bytes = sample().encode();
        bytes[20] ^= 0x01;
        assert!(matches!(
            FrameHeader::decode(&bytes, 64),
            Err(LogError::FrameHeaderChecksum { offset: 64 })
        ));
    }

    #[test]
    fn limits_enforced_with_valid_checksum() {
        let mut h = sample();
        h.uncompressed_len = MAX_UNCOMPRESSED_LEN + 1;
        let bytes = h.encode(); // checksum valid — limit check must still fire
        assert!(matches!(
            FrameHeader::decode(&bytes, 0),
            Err(LogError::LimitExceeded {
                field: "uncompressed_len",
                ..
            })
        ));
        let mut h = sample();
        h.compressed_len = MAX_COMPRESSED_LEN + 1;
        assert!(matches!(
            FrameHeader::decode(&h.encode(), 0),
            Err(LogError::LimitExceeded {
                field: "compressed_len",
                ..
            })
        ));
    }
}
