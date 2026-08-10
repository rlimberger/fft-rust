//! PROFILE (id 3) and CVD (id 4) checkpoint section payloads
//! (`docs/FFTLOG-V2.md` §5). This crate owns both byte layouts and versions;
//! `fft-log` wraps them in section headers and checksums.
//!
//! Determinism: fixed field order, sessions ascending by trade date, arrays
//! price-ascending trimmed to the traded range, no map iteration anywhere.
//! `serialize → restore → serialize` is byte-identical. Restore reconstructs
//! complete state directly — never via synthetic events.
//!
//! PROFILE section v1 (all integers little-endian):
//! ```text
//! i64 tick                    // price units per tick (min_price_increment)
//! u32 session_count           // ascending CT trade date
//! per session:
//!   u32 trade_date            // CT days since Unix epoch
//!   u32 current_eth_period    // developing period, ETH-anchored
//!   u8  period_gap            // 1 = developing-period PV is gap-marked
//!   i64 base_tick             // session low in ticks (0 when len == 0)
//!   u32 len                   // traded span in ticks, high−low+1 (0 = no trades)
//!   if len > 0:
//!     i64 open_tick, i64 poc_tick, u64 poc_volume
//!     u8 has_ib, i64 ib_low_tick, i64 ib_high_tick   // zeros when has_ib == 0
//!     u64 total_volume
//!     u64×len eth_periods, u64×len rth_periods, u64×len volume,
//!     u64×len period_volume, u64×len buy_volume, u64×len sell_volume
//! ```
//!
//! CVD section v1:
//! ```text
//! u32 session_count           // must mirror the PROFILE section
//! per session:
//!   u32 trade_date
//!   u64 buy_volume, u64 sell_volume, i64 high, i64 low
//!   u32 candle_count          // candle i = ETH-anchored period i
//!   (i64 open, i64 high, i64 low, i64 close) × candle_count
//!   per touch counter, cB then cA:
//!     u8 has_price, i64 price (raw 1e-9; 0 when absent), u64 volume, u8 gap
//! ```

use crate::cvd::{Cvd, CvdCandle, TouchCounter};
use crate::profile::MultiProfile;
use crate::session::{MAX_SPAN_TICKS, SessionProfile};
use crate::tpo::SessionClock;
use fft_core::Price;

pub const PROFILE_SECTION_ID: u16 = 3;
pub const PROFILE_SECTION_VERSION: u16 = 1;
pub const CVD_SECTION_ID: u16 = 4;
pub const CVD_SECTION_VERSION: u16 = 1;

/// Sanity ceiling on sessions per checkpoint (a fixture week is 5).
const MAX_SESSIONS: u32 = 4096;
/// Candles are indexed by ETH period; a session has ≤ 46, bitsets cap at 64.
const MAX_CANDLES: u32 = 64;

/// Loud restore failure: version mismatch or malformed section bytes.
#[derive(Debug, PartialEq, Eq)]
pub enum RestoreError {
    UnsupportedVersion {
        section: &'static str,
        version: u16,
    },
    Truncated {
        section: &'static str,
    },
    Corrupt {
        section: &'static str,
        what: &'static str,
    },
}

impl std::fmt::Display for RestoreError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RestoreError::UnsupportedVersion { section, version } => {
                write!(f, "{section} section version {version} unsupported")
            }
            RestoreError::Truncated { section } => write!(f, "{section} section truncated"),
            RestoreError::Corrupt { section, what } => {
                write!(f, "{section} section corrupt: {what}")
            }
        }
    }
}

impl std::error::Error for RestoreError {}

impl MultiProfile {
    /// `(PROFILE, CVD)` section payloads for the current checkpoint.
    pub fn serialize(&self) -> (Vec<u8>, Vec<u8>) {
        (profile_section(self), cvd_section(self))
    }

    /// Reconstruct complete state from section payloads (versions from the
    /// §5 section headers). Never replays events.
    pub fn restore(
        profile_version: u16,
        profile: &[u8],
        cvd_version: u16,
        cvd: &[u8],
    ) -> Result<MultiProfile, RestoreError> {
        if profile_version != PROFILE_SECTION_VERSION {
            return Err(RestoreError::UnsupportedVersion {
                section: "PROFILE",
                version: profile_version,
            });
        }
        if cvd_version != CVD_SECTION_VERSION {
            return Err(RestoreError::UnsupportedVersion {
                section: "CVD",
                version: cvd_version,
            });
        }
        let mut out = restore_profile_section(profile)?;
        restore_cvd_into(cvd, &mut out)?;
        Ok(out)
    }
}

fn profile_section(p: &MultiProfile) -> Vec<u8> {
    let mut b = Vec::new();
    put_i64(&mut b, p.tick.0);
    put_u32(
        &mut b,
        u32::try_from(p.sessions.len()).expect("session count fits u32"),
    );
    for s in &p.sessions {
        put_u32(&mut b, s.trade_date());
        put_u32(&mut b, s.current_eth_period);
        b.push(u8::from(s.period_gap));
        match (s.low_tick, s.high_tick) {
            (Some(low), Some(high)) => {
                let lo = usize::try_from(low - s.base_tick).expect("low within arrays");
                let hi = usize::try_from(high - s.base_tick).expect("high within arrays");
                put_i64(&mut b, low);
                put_u32(&mut b, u32::try_from(hi - lo + 1).expect("span fits u32"));
                put_i64(&mut b, s.open_tick.expect("open set once traded"));
                put_i64(&mut b, s.poc_tick.expect("poc set once traded"));
                put_u64(&mut b, s.poc_volume);
                match (s.ib_low_tick, s.ib_high_tick) {
                    (Some(il), Some(ih)) => {
                        b.push(1);
                        put_i64(&mut b, il);
                        put_i64(&mut b, ih);
                    }
                    _ => {
                        b.push(0);
                        put_i64(&mut b, 0);
                        put_i64(&mut b, 0);
                    }
                }
                put_u64(&mut b, s.total_volume);
                for arr in [
                    &s.eth_periods,
                    &s.rth_periods,
                    &s.volume,
                    &s.period_volume,
                    &s.buy_volume,
                    &s.sell_volume,
                ] {
                    for &v in &arr[lo..=hi] {
                        put_u64(&mut b, v);
                    }
                }
            }
            _ => {
                put_i64(&mut b, 0);
                put_u32(&mut b, 0);
            }
        }
    }
    b
}

fn cvd_section(p: &MultiProfile) -> Vec<u8> {
    let mut b = Vec::new();
    put_u32(
        &mut b,
        u32::try_from(p.sessions.len()).expect("session count fits u32"),
    );
    for s in &p.sessions {
        let c = s.cvd();
        put_u32(&mut b, s.trade_date());
        put_u64(&mut b, c.buy_volume);
        put_u64(&mut b, c.sell_volume);
        put_i64(&mut b, c.high);
        put_i64(&mut b, c.low);
        put_u32(
            &mut b,
            u32::try_from(c.candles.len()).expect("candle count fits u32"),
        );
        for cd in &c.candles {
            put_i64(&mut b, cd.open);
            put_i64(&mut b, cd.high);
            put_i64(&mut b, cd.low);
            put_i64(&mut b, cd.close);
        }
        for t in [&c.cur_bid, &c.cur_ask] {
            b.push(u8::from(t.price.is_some()));
            put_i64(&mut b, t.price.map_or(0, |p| p.0));
            put_u64(&mut b, t.volume);
            b.push(u8::from(t.gap));
        }
    }
    b
}

fn restore_profile_section(bytes: &[u8]) -> Result<MultiProfile, RestoreError> {
    let mut c = Cursor::new(bytes, "PROFILE");
    let tick = c.i64()?;
    if tick <= 0 {
        return Err(c.corrupt("non-positive tick"));
    }
    let count = c.u32()?;
    if count > MAX_SESSIONS {
        return Err(c.corrupt("session count exceeds ceiling"));
    }
    let mut sessions = Vec::new();
    let mut prev_date = None;
    for _ in 0..count {
        let trade_date = c.u32()?;
        if prev_date.is_some_and(|d| trade_date <= d) {
            return Err(c.corrupt("trade dates not strictly ascending"));
        }
        prev_date = Some(trade_date);
        let current_eth_period = c.u32()?;
        let period_gap = c.bool()?;
        let base_tick = c.i64()?;
        let len = c.u32()?;
        if i64::from(len) > MAX_SPAN_TICKS {
            return Err(c.corrupt("price span exceeds ceiling"));
        }
        let mut s = SessionProfile::new(SessionClock::for_trade_date(trade_date), Price(tick));
        s.current_eth_period = current_eth_period;
        s.period_gap = period_gap;
        if len > 0 {
            let n = len as usize;
            let high_tick = base_tick + i64::from(len) - 1;
            s.base_tick = base_tick;
            s.low_tick = Some(base_tick);
            s.high_tick = Some(high_tick);
            let open = c.i64()?;
            let poc = c.i64()?;
            if !(base_tick..=high_tick).contains(&open) || !(base_tick..=high_tick).contains(&poc) {
                return Err(c.corrupt("open/poc outside traded range"));
            }
            s.open_tick = Some(open);
            s.poc_tick = Some(poc);
            s.poc_volume = c.u64()?;
            let has_ib = c.bool()?;
            let (il, ih) = (c.i64()?, c.i64()?);
            if has_ib {
                s.ib_low_tick = Some(il);
                s.ib_high_tick = Some(ih);
            } else if il != 0 || ih != 0 {
                return Err(c.corrupt("absent IB carries nonzero bounds"));
            }
            s.total_volume = c.u64()?;
            for arr in [
                &mut s.eth_periods,
                &mut s.rth_periods,
                &mut s.volume,
                &mut s.period_volume,
                &mut s.buy_volume,
                &mut s.sell_volume,
            ] {
                arr.reserve_exact(n);
                for _ in 0..n {
                    arr.push(c.u64()?);
                }
            }
        }
        sessions.push(s);
    }
    c.finish()?;
    Ok(MultiProfile {
        tick: Price(tick),
        sessions,
    })
}

fn restore_cvd_into(bytes: &[u8], p: &mut MultiProfile) -> Result<(), RestoreError> {
    let mut c = Cursor::new(bytes, "CVD");
    let count = c.u32()?;
    if count as usize != p.sessions.len() {
        return Err(c.corrupt("session count disagrees with PROFILE section"));
    }
    for s in &mut p.sessions {
        let trade_date = c.u32()?;
        if trade_date != s.trade_date() {
            return Err(c.corrupt("trade date disagrees with PROFILE section"));
        }
        let buy_volume = c.u64()?;
        let sell_volume = c.u64()?;
        let high = c.i64()?;
        let low = c.i64()?;
        let candle_count = c.u32()?;
        if candle_count > MAX_CANDLES {
            return Err(c.corrupt("candle count exceeds ceiling"));
        }
        let mut candles = Vec::with_capacity(candle_count as usize);
        for _ in 0..candle_count {
            candles.push(CvdCandle {
                open: c.i64()?,
                high: c.i64()?,
                low: c.i64()?,
                close: c.i64()?,
            });
        }
        let mut touch = [TouchCounter::default(), TouchCounter::default()];
        for t in &mut touch {
            let has_price = c.bool()?;
            let price = c.i64()?;
            if !has_price && price != 0 {
                return Err(c.corrupt("absent touch price carries nonzero value"));
            }
            t.price = has_price.then_some(Price(price));
            t.volume = c.u64()?;
            t.gap = c.bool()?;
        }
        let [cur_bid, cur_ask] = touch;
        s.cvd = Cvd {
            buy_volume,
            sell_volume,
            high,
            low,
            candles,
            cur_bid,
            cur_ask,
        };
    }
    c.finish()
}

fn put_u32(b: &mut Vec<u8>, v: u32) {
    b.extend_from_slice(&v.to_le_bytes());
}

fn put_u64(b: &mut Vec<u8>, v: u64) {
    b.extend_from_slice(&v.to_le_bytes());
}

fn put_i64(b: &mut Vec<u8>, v: i64) {
    b.extend_from_slice(&v.to_le_bytes());
}

struct Cursor<'a> {
    buf: &'a [u8],
    pos: usize,
    section: &'static str,
}

impl<'a> Cursor<'a> {
    fn new(buf: &'a [u8], section: &'static str) -> Cursor<'a> {
        Cursor {
            buf,
            pos: 0,
            section,
        }
    }

    fn take(&mut self, n: usize) -> Result<&'a [u8], RestoreError> {
        let end = self.pos.checked_add(n).filter(|&e| e <= self.buf.len());
        let Some(end) = end else {
            return Err(RestoreError::Truncated {
                section: self.section,
            });
        };
        let out = &self.buf[self.pos..end];
        self.pos = end;
        Ok(out)
    }

    fn bool(&mut self) -> Result<bool, RestoreError> {
        match self.take(1)?[0] {
            0 => Ok(false),
            1 => Ok(true),
            _ => Err(self.corrupt("flag byte not 0/1")),
        }
    }

    fn u32(&mut self) -> Result<u32, RestoreError> {
        Ok(u32::from_le_bytes(
            self.take(4)?.try_into().expect("4 bytes"),
        ))
    }

    fn u64(&mut self) -> Result<u64, RestoreError> {
        Ok(u64::from_le_bytes(
            self.take(8)?.try_into().expect("8 bytes"),
        ))
    }

    fn i64(&mut self) -> Result<i64, RestoreError> {
        Ok(i64::from_le_bytes(
            self.take(8)?.try_into().expect("8 bytes"),
        ))
    }

    fn corrupt(&self, what: &'static str) -> RestoreError {
        RestoreError::Corrupt {
            section: self.section,
            what,
        }
    }

    fn finish(self) -> Result<(), RestoreError> {
        if self.pos == self.buf.len() {
            Ok(())
        } else {
            Err(self.corrupt("trailing bytes"))
        }
    }
}
