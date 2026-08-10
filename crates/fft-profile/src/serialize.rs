//! PROFILE (id 3), CVD (id 4), and SESSION (id 6) checkpoint section payloads
//! (`docs/FFTLOG-V2.md` §5, `docs/ENGINE.md` §4). This crate owns the byte layouts
//! and versions; `fft-log` wraps them in section headers and checksums.
//!
//! Each payload is self-identifying: first two bytes = section version (mirrors
//! fft-book). PROFILE keeps arrays/geometry; SESSION carries period cursors,
//! gap markers, loud counters, and explicit boundary timestamps.
//!
//! Determinism: fixed field order, sessions ascending by trade date, arrays
//! price-ascending trimmed to the traded range, no map iteration anywhere.
//! `serialize → restore → serialize` is byte-identical.

use crate::cvd::{Cvd, CvdCandle, TouchCounter};
use crate::profile::MultiProfile;
use crate::session::{MAX_SPAN_TICKS, SessionProfile};
use crate::tpo::SessionClock;
use fft_core::{Price, Ts};

pub const PROFILE_SECTION_ID: u16 = 3;
pub const PROFILE_SECTION_VERSION: u16 = 1;
pub const CVD_SECTION_ID: u16 = 4;
pub const CVD_SECTION_VERSION: u16 = 1;
pub const SESSION_SECTION_ID: u16 = 6;
pub const SESSION_SECTION_VERSION: u16 = 1;

/// Sanity ceiling on sessions per checkpoint (a fixture week is 5).
const MAX_SESSIONS: u32 = 4096;
/// Candles are indexed by ETH period; a session has ≤ 46, bitsets cap at 64.
const MAX_CANDLES: u32 = 64;

/// `(PROFILE, CVD, SESSION)` section payloads for one checkpoint.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct ProfileSections {
    pub profile: Vec<u8>,
    pub cvd: Vec<u8>,
    pub session: Vec<u8>,
}

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
    /// PROFILE + CVD + SESSION section payloads (ENGINE.md §4).
    pub fn serialize(&self) -> ProfileSections {
        ProfileSections {
            profile: profile_section(self),
            cvd: cvd_section(self),
            session: session_section(self),
        }
    }

    /// Reconstruct complete state from the three section payloads. Each
    /// payload's first two bytes are its section version. Never replays events.
    pub fn restore(
        profile: &[u8],
        cvd: &[u8],
        session: &[u8],
    ) -> Result<MultiProfile, RestoreError> {
        let mut out = restore_profile_section(profile)?;
        restore_session_into(session, &mut out)?;
        restore_cvd_into(cvd, &mut out)?;
        Ok(out)
    }
}

fn profile_section(p: &MultiProfile) -> Vec<u8> {
    let mut b = Vec::new();
    put_u16(&mut b, PROFILE_SECTION_VERSION);
    put_i64(&mut b, p.tick.0);
    put_u32(
        &mut b,
        u32::try_from(p.sessions.len()).expect("session count fits u32"),
    );
    for s in &p.sessions {
        put_u32(&mut b, s.trade_date());
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
    put_u16(&mut b, CVD_SECTION_VERSION);
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

fn session_section(p: &MultiProfile) -> Vec<u8> {
    let mut b = Vec::new();
    put_u16(&mut b, SESSION_SECTION_VERSION);
    put_u32(
        &mut b,
        u32::try_from(p.sessions.len()).expect("session count fits u32"),
    );
    for s in &p.sessions {
        let c = s.clock();
        put_u32(&mut b, s.trade_date());
        put_u32(&mut b, s.current_eth_period);
        b.push(u8::from(s.period_gap));
        put_u64(&mut b, s.post_close_events);
        put_u64(&mut b, s.backward_ts_events);
        // Explicit boundaries — never rebuild from trade_date alone at restore.
        put_u64(&mut b, c.session_open().0);
        put_u64(&mut b, c.rth_open().0);
        put_u64(&mut b, c.rth_close().0);
        put_u64(&mut b, c.session_end().0);
    }
    b
}

fn restore_profile_section(bytes: &[u8]) -> Result<MultiProfile, RestoreError> {
    let mut c = Cursor::new(bytes, "PROFILE");
    let version = c.u16()?;
    if version != PROFILE_SECTION_VERSION {
        return Err(RestoreError::UnsupportedVersion {
            section: "PROFILE",
            version,
        });
    }
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
        // Clock placeholder: SESSION section overwrites with explicit boundaries.
        let mut s = SessionProfile::new(SessionClock::for_trade_date(trade_date), Price(tick));
        let base_tick = c.i64()?;
        let len = c.u32()?;
        if i64::from(len) > MAX_SPAN_TICKS {
            return Err(c.corrupt("price span exceeds ceiling"));
        }
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

fn restore_session_into(bytes: &[u8], p: &mut MultiProfile) -> Result<(), RestoreError> {
    let mut c = Cursor::new(bytes, "SESSION");
    let version = c.u16()?;
    if version != SESSION_SECTION_VERSION {
        return Err(RestoreError::UnsupportedVersion {
            section: "SESSION",
            version,
        });
    }
    let count = c.u32()?;
    if count as usize != p.sessions.len() {
        return Err(c.corrupt("session count disagrees with PROFILE section"));
    }
    for s in &mut p.sessions {
        let trade_date = c.u32()?;
        if trade_date != s.trade_date() {
            return Err(c.corrupt("trade date disagrees with PROFILE section"));
        }
        s.current_eth_period = c.u32()?;
        s.period_gap = c.bool()?;
        s.post_close_events = c.u64()?;
        s.backward_ts_events = c.u64()?;
        let session_open = Ts(c.u64()?);
        let rth_open = Ts(c.u64()?);
        let rth_close = Ts(c.u64()?);
        let session_end = Ts(c.u64()?);
        // Rebuild clock from trade_date for eth_end derivation, then verify
        // explicit boundaries match — clock is the authority for lettering math.
        let clock = SessionClock::for_trade_date(trade_date);
        if clock.session_open() != session_open
            || clock.rth_open() != rth_open
            || clock.rth_close() != rth_close
            || clock.session_end() != session_end
        {
            return Err(c.corrupt("session boundary timestamps disagree with trade_date"));
        }
        s.clock = clock;
    }
    c.finish()
}

fn restore_cvd_into(bytes: &[u8], p: &mut MultiProfile) -> Result<(), RestoreError> {
    let mut c = Cursor::new(bytes, "CVD");
    let version = c.u16()?;
    if version != CVD_SECTION_VERSION {
        return Err(RestoreError::UnsupportedVersion {
            section: "CVD",
            version,
        });
    }
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

fn put_u16(b: &mut Vec<u8>, v: u16) {
    b.extend_from_slice(&v.to_le_bytes());
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

    fn u16(&mut self) -> Result<u16, RestoreError> {
        Ok(u16::from_le_bytes(
            self.take(2)?.try_into().expect("2 bytes"),
        ))
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
