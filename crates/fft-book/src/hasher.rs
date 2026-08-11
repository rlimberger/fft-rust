//! Fast hasher for dense `u64` keys (order ids).
//!
//! std's default SipHash is DoS-resistant but wasted on CME order ids: they are
//! dense integers from a trusted feed, looked up on every Add/Cancel/Modify/Fill.
//! This is the FxHash64 mix used by Firefox / `rustc-hash` — one rotate, xor, and
//! multiply by the odd constant `0x517cc1b727220a95`. Pure identity is unsafe here:
//! hashbrown's SIMD control tags use high hash bits, so unmixed sequential ids
//! collapse into the same groups.
//!
//! Pre-sizing large capacities (`1<<16`) was measured and *regressed* apply time
//! (~19 s vs ~2.5 s): oversize empty tables thrash on the hot lookup path. Maps
//! grow from empty; the hasher is the win.

use std::collections::HashMap;
use std::hash::{BuildHasherDefault, Hasher};

/// FxHash64 multiply constant (Firefox / rustc-hash).
const K: u64 = 0x517c_c1b7_2722_0a95;

#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct FxU64Hasher {
    state: u64,
}

impl Hasher for FxU64Hasher {
    #[inline]
    fn write(&mut self, bytes: &[u8]) {
        // `u64: Hash` calls `write_u64`; this path is only for completeness.
        let mut chunks = bytes.chunks_exact(8);
        for chunk in chunks.by_ref() {
            self.write_u64(u64::from_ne_bytes(chunk.try_into().unwrap()));
        }
        let rem = chunks.remainder();
        if !rem.is_empty() {
            let mut buf = [0u8; 8];
            buf[..rem.len()].copy_from_slice(rem);
            self.write_u64(u64::from_ne_bytes(buf));
        }
    }

    #[inline]
    fn write_u8(&mut self, i: u8) {
        self.write_u64(u64::from(i));
    }

    #[inline]
    fn write_u16(&mut self, i: u16) {
        self.write_u64(u64::from(i));
    }

    #[inline]
    fn write_u32(&mut self, i: u32) {
        self.write_u64(u64::from(i));
    }

    #[inline]
    fn write_u64(&mut self, i: u64) {
        // FxHash64: rotate → xor → multiply. Order matters: multiply-before-xor
        // with a zero seed degenerates to identity and tanks hashbrown.
        self.state = self.state.rotate_left(5) ^ i;
        self.state = self.state.wrapping_mul(K);
    }

    #[inline]
    fn write_usize(&mut self, i: usize) {
        self.write_u64(i as u64);
    }

    #[inline]
    fn finish(&self) -> u64 {
        self.state
    }
}

/// Deterministic (non-random) build hasher — fine for trusted integer keys.
pub(crate) type FxU64BuildHasher = BuildHasherDefault<FxU64Hasher>;

/// `HashMap` keyed by `u64` with [`FxU64Hasher`].
pub(crate) type U64Map<V> = HashMap<u64, V, FxU64BuildHasher>;

/// Order-id index / refresh maps (same hasher).
pub(crate) type OrderIdMap<V> = U64Map<V>;

#[inline]
pub(crate) fn order_id_map_new<V>() -> OrderIdMap<V> {
    OrderIdMap::with_hasher(FxU64BuildHasher::default())
}
