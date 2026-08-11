//! Place-path native-refresh census + full-day apply.

use fft_book::{Book, REFRESH_SECTION_VERSION, RefreshState};
use fft_core::{EventKind, OrderId, Side};
use fft_log::LogReader;
use fft_profile::MultiProfile;
use fft_replay::ReplaySource;
use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::time::{Duration, Instant};

const APPLY_BUDGET: Duration = Duration::from_secs(3600);

#[derive(Default)]
struct Track {
    reloads: u32,
    hidden: u64,
    max_reloads: u32,
}

#[derive(Default)]
pub struct Census {
    pub total: u64,
    pub hidden: u64,
    pub max_reloads: u32,
    per_order: HashMap<u64, Track>,
    pub unavail: HashSet<u64>,
    pub sig_checked: u64,
    pub sig_ok: u64,
    pub inv_a: u64,
    pub gaps: u32,
}

impl Census {
    fn end_life(&mut self, order_id: u64) {
        if let Some(e) = self.per_order.get_mut(&order_id) {
            e.reloads = 0;
            e.hidden = 0;
        }
    }

    fn note(&mut self, book: &Book, order_id: u64) {
        match book.refresh_state(OrderId(order_id)) {
            RefreshState::Known {
                native,
                reloads,
                hidden_volume,
            } => {
                if (hidden_volume > 0) != (reloads > 0) {
                    self.inv_a += 1;
                }
                if reloads == 0 {
                    // Resting non-native life: drop prior-life counters so the
                    // next native cycle is counted even if reloads/hidden match.
                    self.end_life(order_id);
                    return;
                }
                self.sig_checked += 1;
                if native {
                    self.sig_ok += 1;
                }
                let e = self.per_order.entry(order_id).or_default();
                if reloads > e.reloads {
                    self.total += u64::from(reloads - e.reloads);
                    if hidden_volume >= e.hidden {
                        self.hidden += hidden_volume - e.hidden;
                    } else {
                        self.inv_a += 1;
                        self.hidden += hidden_volume;
                    }
                } else if reloads < e.reloads || hidden_volume != e.hidden {
                    self.total += u64::from(reloads);
                    self.hidden += hidden_volume;
                }
                e.reloads = reloads;
                e.hidden = hidden_volume;
                e.max_reloads = e.max_reloads.max(reloads);
                self.max_reloads = self.max_reloads.max(e.max_reloads);
            }
            RefreshState::Unavailable if book.gaps_seen() > 0 => {
                self.unavail.insert(order_id);
            }
            RefreshState::NotResting => self.end_life(order_id),
            _ => {}
        }
    }

    /// (distinct_ids, hist buckets 1 / 2–4 / 5–9 / 10+)
    pub fn hist(&self) -> (u64, [u64; 4]) {
        let mut h = [0u64; 4];
        let mut n = 0u64;
        for t in self.per_order.values() {
            if t.max_reloads == 0 {
                continue;
            }
            n += 1;
            match t.max_reloads {
                1 => h[0] += 1,
                2..=4 => h[1] += 1,
                5..=9 => h[2] += 1,
                _ => h[3] += 1,
            }
        }
        (n, h)
    }
}

pub struct Run {
    pub events: u64,
    pub eof: bool,
    pub applied_seq: u64,
    pub applied_ts: u64,
    pub census: Census,
    pub sec_count: u64,
    pub sec_hidden: u64,
    pub book_gaps: u32,
    pub live_eod: usize,
    pub trade_date: u32,
    pub symbol: String,
    pub eod_native: u64,
    pub eod_unavail: u64,
}

pub fn run(path: &Path) -> Result<Run, String> {
    let mut src = ReplaySource::open(path).map_err(|e| format!("open: {e}"))?;
    for w in &src.open_report().warnings {
        eprintln!("claim4-census: open warning: {w}");
    }
    let meta = src.meta().clone();
    let mut book = Book::new(meta.min_price_increment);
    let mut profile = MultiProfile::new(meta.min_price_increment);
    profile.begin_session(meta.trade_date);

    let mut census = Census::default();
    let mut events = 0u64;
    let start = Instant::now();
    loop {
        if events > 0 && events.is_multiple_of(4_000_000) {
            eprintln!(
                "claim4-census: … {events} events, {:.1}s",
                start.elapsed().as_secs_f64()
            );
        }
        if start.elapsed() >= APPLY_BUDGET {
            return Err(format!(
                "apply budget {}s after {events} events",
                APPLY_BUDGET.as_secs()
            ));
        }
        let Some(ev) = src
            .apply_next(&mut book, &mut profile)
            .map_err(|e| format!("apply_next: {e}"))?
        else {
            break;
        };
        events += 1;
        match ev.kind {
            EventKind::Gap => census.gaps = book.gaps_seen(),
            EventKind::Clear if !ev.is_snapshot() => {
                for t in census.per_order.values_mut() {
                    t.reloads = 0;
                    t.hidden = 0;
                }
            }
            EventKind::Add | EventKind::Modify | EventKind::Cancel
                if !ev.is_snapshot() && ev.order_id.0 != 0 =>
            {
                census.note(&book, ev.order_id.0);
            }
            _ => {}
        }
    }

    book.check_invariants();
    census.gaps = book.gaps_seen();
    for t in census.per_order.values() {
        if (t.hidden > 0) != (t.reloads > 0) {
            census.inv_a += 1;
        }
    }

    let (sec_count, sec_hidden, live_sum, live_hid) = parse_refresh(&book.serialize_refresh())?;

    let mut eod_native = 0u64;
    let mut eod_unavail = 0u64;
    for side in [Side::Bid, Side::Ask] {
        book.for_each_level(side, |price, _| {
            let agg = book.refresh_at(side, price);
            if u64::from(agg.refresh_count) > sec_count || agg.hidden_volume > sec_hidden {
                census.inv_a += 1;
            }
            book.for_each_order_at(side, price, |id, _| match book.refresh_state(id) {
                RefreshState::Known {
                    native,
                    reloads,
                    hidden_volume,
                } => {
                    if (hidden_volume > 0) != (reloads > 0) {
                        census.inv_a += 1;
                    }
                    if native {
                        eod_native += 1;
                        match census.per_order.get(&id.0) {
                            Some(t) if reloads <= t.max_reloads => {}
                            _ if reloads > 0 => census.inv_a += 1,
                            _ => {}
                        }
                    }
                }
                RefreshState::Unavailable => {
                    eod_unavail += 1;
                    if book.gaps_seen() > 0 {
                        census.unavail.insert(id.0);
                    }
                }
                RefreshState::NotResting => {}
            });
        });
    }

    if sec_count != census.total || sec_hidden != census.hidden {
        return Err(format!(
            "session aggregate vs place-path: section={sec_count}/{sec_hidden} \
             census={}/{} live={live_sum}/{live_hid}",
            census.total, census.hidden
        ));
    }

    Ok(Run {
        events,
        eof: true,
        applied_seq: src.applied_seq(),
        applied_ts: src.applied_ts(),
        census,
        sec_count,
        sec_hidden,
        book_gaps: book.gaps_seen(),
        live_eod: book.live_order_count(),
        trade_date: meta.trade_date,
        symbol: meta.symbol,
        eod_native,
        eod_unavail,
    })
}

pub fn count_events(path: &Path) -> Result<u64, String> {
    let (reader, report) = LogReader::open(path).map_err(|e| format!("open: {e}"))?;
    for w in &report.warnings {
        eprintln!("claim4-census: count open warning: {w}");
    }
    let mut n = 0u64;
    for ev in reader.events(0..reader.frame_count()) {
        ev.map_err(|e| format!("events: {e}"))?;
        n += 1;
    }
    Ok(n)
}

fn parse_refresh(bytes: &[u8]) -> Result<(u64, u64, u64, u64), String> {
    let mut r = Sec::new(bytes);
    if r.u16()? != REFRESH_SECTION_VERSION {
        return Err("REFRESH version mismatch".into());
    }
    let _ = (r.u32()?, r.u64()?);
    let mut live_r = 0u64;
    let mut live_h = 0u64;
    for _ in 0..r.u32()? {
        let _ = r.u64()?;
        live_r += u64::from(r.u32()?);
        live_h += r.u64()?;
        let _ = r.u8()?;
    }
    for _ in 0..r.u32()? {
        let _ = (
            r.u64()?,
            r.u8()?,
            r.i64()?,
            r.u64()?,
            r.u32()?,
            r.u32()?,
            r.u64()?,
        );
    }
    for _ in 0..r.u32()? {
        let _ = (r.u64()?, r.u64()?, r.u8()?, r.u64()?, r.u32()?);
    }
    let mut cnt = 0u64;
    let mut hid = 0u64;
    for _ in 0..r.u32()? {
        let _ = (r.u8()?, r.i64()?);
        cnt += u64::from(r.u32()?);
        hid += r.u64()?;
    }
    if r.pos != bytes.len() {
        return Err(format!("{} trailing REFRESH bytes", bytes.len() - r.pos));
    }
    Ok((cnt, hid, live_r, live_h))
}

struct Sec<'a> {
    buf: &'a [u8],
    pos: usize,
}
impl<'a> Sec<'a> {
    fn new(buf: &'a [u8]) -> Self {
        Self { buf, pos: 0 }
    }
    fn take(&mut self, n: usize) -> Result<&'a [u8], String> {
        if self.pos + n > self.buf.len() {
            return Err(format!("truncated at {}", self.pos));
        }
        let s = &self.buf[self.pos..self.pos + n];
        self.pos += n;
        Ok(s)
    }
    fn u8(&mut self) -> Result<u8, String> {
        Ok(self.take(1)?[0])
    }
    fn u16(&mut self) -> Result<u16, String> {
        Ok(u16::from_le_bytes(self.take(2)?.try_into().unwrap()))
    }
    fn u32(&mut self) -> Result<u32, String> {
        Ok(u32::from_le_bytes(self.take(4)?.try_into().unwrap()))
    }
    fn u64(&mut self) -> Result<u64, String> {
        Ok(u64::from_le_bytes(self.take(8)?.try_into().unwrap()))
    }
    fn i64(&mut self) -> Result<i64, String> {
        Ok(self.u64()? as i64)
    }
}
