//! Pure Market Profile aggregation, lettering, agreement, and footer math.

use fft_core::Price;
use fft_engine::{DomRenderState, ProfilePriceRow, ProfileRenderState, ProfileSessionRender};

pub const ETH_PERIOD_COUNT: usize = 46;
pub const RTH_OFFSET: usize = 31;
pub const RTH_PERIOD_COUNT: usize = 13;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TpoKind {
    Eth,
    Rth,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct MpRow {
    pub price: Price,
    pub eth_periods: u64,
    pub rth_periods: u64,
    pub session_volume: u64,
    pub period_volume: u64,
    pub buy_volume: u64,
    pub sell_volume: u64,
}

#[derive(Debug, Default)]
pub struct VisibleProfile {
    pub scaled_tick: Price,
    pub rows: Vec<MpRow>,
}

pub fn display_session(profile: &ProfileRenderState) -> Option<&ProfileSessionRender> {
    profile.sessions.first()
}

pub fn period_letter(index: usize) -> char {
    match index {
        0..=25 => char::from(b'A' + index as u8),
        26..=51 => char::from(b'a' + (index - 26) as u8),
        _ => panic!("MP period index {index} exceeds letter range"),
    }
}

/// Visit occupied physical EP columns. During RTH, the display clock restarts
/// at A and uses `rth_periods`; ETH resumes after RTH at physical periods 44–45.
pub fn for_each_tpo(eth: u64, rth: u64, mut visit: impl FnMut(usize, char, TpoKind)) {
    let valid_eth = (1u64 << ETH_PERIOD_COUNT) - 1;
    assert_eq!(eth & !valid_eth, 0, "MP ETH bitset exceeds 46 periods");
    let valid_rth = (1u64 << RTH_PERIOD_COUNT) - 1;
    assert_eq!(rth & !valid_rth, 0, "MP RTH bitset exceeds 13 periods");
    for physical in 0..ETH_PERIOD_COUNT {
        if (RTH_OFFSET..RTH_OFFSET + RTH_PERIOD_COUNT).contains(&physical) {
            let rth_index = physical - RTH_OFFSET;
            if rth & (1u64 << rth_index) != 0 {
                visit(physical, period_letter(rth_index), TpoKind::Rth);
            }
            continue;
        }
        if eth & (1u64 << physical) != 0 {
            visit(physical, period_letter(physical), TpoKind::Eth);
        }
    }
}

pub fn visible_rows(
    session: &ProfileSessionRender,
    tick: Price,
    scale: u8,
    center: Option<Price>,
    max_rows: usize,
) -> VisibleProfile {
    validate_scale(scale);
    if session.rows.is_empty() || max_rows == 0 {
        return VisibleProfile::default();
    }
    assert!(tick.0 > 0, "MP tick size must be positive");
    let scaled_tick = Price(
        tick.0
            .checked_mul(i64::from(scale))
            .expect("scaled MP tick overflows i64"),
    );
    validate_rows(&session.rows);
    let first = bucket(session.rows[0].price, scaled_tick);
    let last = bucket(session.rows[session.rows.len() - 1].price, scaled_tick);
    let midpoint = i64::try_from((i128::from(first.0) + i128::from(last.0)) / 2)
        .expect("MP range midpoint fits i64");
    let desired = bucket(center.unwrap_or(Price(midpoint)), scaled_tick);
    let count = max_rows;
    let start_price = Price(
        desired
            .0
            .checked_sub(
                i64::try_from(count / 2)
                    .expect("MP start fits i64")
                    .checked_mul(scaled_tick.0)
                    .expect("MP start price offset overflows"),
            )
            .expect("MP start price overflows"),
    );
    let mut rows = Vec::with_capacity(count);
    for index in 0..count {
        rows.push(MpRow {
            price: Price(
                start_price
                    .0
                    .checked_add(
                        i64::try_from(index)
                            .expect("MP row index fits i64")
                            .checked_mul(scaled_tick.0)
                            .expect("MP row offset overflows"),
                    )
                    .expect("MP row price overflows"),
            ),
            ..MpRow::default()
        });
    }
    let end_price = rows.last().expect("visible rows are nonempty").price;
    for source in &session.rows {
        let price = bucket(source.price, scaled_tick);
        if price.0 < start_price.0 || price.0 > end_price.0 {
            continue;
        }
        let index = usize::try_from((price.0 - start_price.0) / scaled_tick.0)
            .expect("visible MP index is nonnegative");
        merge(&mut rows[index], source);
    }
    VisibleProfile { scaled_tick, rows }
}

pub fn pan_center(
    session: &ProfileSessionRender,
    tick: Price,
    scale: u8,
    center: Option<Price>,
    delta: i64,
) -> Option<Price> {
    validate_scale(scale);
    let (first, last) = (session.rows.first()?, session.rows.last()?);
    let scaled_tick = tick
        .0
        .checked_mul(i64::from(scale))
        .expect("scaled MP tick overflows i64");
    assert!(scaled_tick > 0, "MP tick size must be positive");
    let midpoint = i64::try_from((i128::from(first.price.0) + i128::from(last.price.0)) / 2)
        .expect("MP range midpoint fits i64");
    let current = center.unwrap_or(Price(midpoint));
    let moved = i128::from(current.0) + i128::from(delta) * i128::from(scaled_tick);
    Some(Price(
        i64::try_from(moved).expect("MP pan center overflows i64"),
    ))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct VolumeMismatch {
    pub price: Price,
    pub profile_volume: u64,
    pub dom_volume: u64,
}

/// Compare raw instrument-tick volume for prices present in both pane sources.
pub fn check_pane_agreement(
    profile: &ProfileRenderState,
    dom: &DomRenderState,
) -> Result<usize, VolumeMismatch> {
    let Some(session) = display_session(profile) else {
        return Ok(0);
    };
    validate_rows(&session.rows);
    let mut compared = 0;
    for dom_row in &dom.rows {
        if let Ok(index) = session
            .rows
            .binary_search_by_key(&dom_row.price, |row| row.price)
        {
            let profile_volume = session.rows[index].session_volume;
            if profile_volume != dom_row.session_volume {
                return Err(VolumeMismatch {
                    price: dom_row.price,
                    profile_volume,
                    dom_volume: dom_row.session_volume,
                });
            }
            compared += 1;
        }
    }
    Ok(compared)
}

pub fn session_open_footer(trade_date: u32) -> String {
    assert!(trade_date > 0, "trade date must follow Unix epoch day zero");
    let (_, month, day) = crate::gate_report::civil_from_days(i64::from(trade_date) - 1);
    format!("{month:02}-{day:02} 18:00")
}

fn validate_scale(scale: u8) {
    assert!(
        matches!(scale, 1 | 2 | 4),
        "MP tick scale must be 1, 2, or 4"
    );
}

fn validate_rows(rows: &[ProfilePriceRow]) {
    for pair in rows.windows(2) {
        assert!(
            pair[0].price < pair[1].price,
            "MP rows must be strictly ascending"
        );
    }
}

fn bucket(price: Price, tick: Price) -> Price {
    Price(
        price
            .0
            .div_euclid(tick.0)
            .checked_mul(tick.0)
            .expect("MP bucket price overflows i64"),
    )
}

/// Session-open hairline price floored to `scaled_tick`, if the session has an open.
pub fn open_marker_bucket(session: &ProfileSessionRender, scaled_tick: Price) -> Option<Price> {
    assert!(scaled_tick.0 > 0, "MP open marker tick must be positive");
    session.open.map(|price| bucket(price, scaled_tick))
}

fn merge(target: &mut MpRow, source: &ProfilePriceRow) {
    target.eth_periods |= source.eth_periods;
    target.rth_periods |= source.rth_periods;
    target.session_volume = target
        .session_volume
        .checked_add(source.session_volume)
        .expect("MP session-volume aggregation overflow");
    target.period_volume = target
        .period_volume
        .checked_add(source.period_volume)
        .expect("MP period-volume aggregation overflow");
    target.buy_volume = target
        .buy_volume
        .checked_add(source.buy_volume)
        .expect("MP buy-volume aggregation overflow");
    target.sell_volume = target
        .sell_volume
        .checked_add(source.sell_volume)
        .expect("MP sell-volume aggregation overflow");
}

#[cfg(test)]
mod tests {
    use fft_engine::DomPriceRow;

    use super::*;

    fn row(price: i64, volume: u64) -> ProfilePriceRow {
        ProfilePriceRow {
            price: Price(price),
            session_volume: volume,
            ..Default::default()
        }
    }

    #[test]
    fn letters_restart_at_rth_a_and_resume_eth_after_close() {
        let eth = (1 << 0) | (1 << 30) | (1 << 44);
        let rth = (1 << 0) | (1 << 12);
        let mut got = Vec::new();
        for_each_tpo(eth, rth, |column, letter, kind| {
            got.push((column, letter, kind));
        });
        assert_eq!(
            got,
            [
                (0, 'A', TpoKind::Eth),
                (30, 'e', TpoKind::Eth),
                (31, 'A', TpoKind::Rth),
                (43, 'M', TpoKind::Rth),
                (44, 's', TpoKind::Eth),
            ]
        );
    }

    #[test]
    fn visible_aggregation_ors_tpos_and_sums_volume() {
        let mut a = row(100, 3);
        a.eth_periods = 1;
        a.period_volume = 2;
        let mut b = row(101, 5);
        b.eth_periods = 2;
        b.period_volume = 4;
        let session = ProfileSessionRender {
            rows: vec![a, b],
            ..Default::default()
        };
        let view = visible_rows(&session, Price(1), 2, Some(Price(100)), 2);
        assert_eq!(view.rows.len(), 2);
        let bucket = view
            .rows
            .iter()
            .find(|row| row.price == Price(100))
            .unwrap();
        assert_eq!(bucket.session_volume, 8);
        assert_eq!(bucket.period_volume, 6);
        assert_eq!(bucket.eth_periods, 3);
    }

    #[test]
    fn linked_center_stays_centered_beyond_profile_range() {
        let session = ProfileSessionRender {
            rows: vec![row(100, 1), row(101, 1)],
            ..Default::default()
        };
        let view = visible_rows(&session, Price(1), 1, Some(Price(110)), 5);
        assert_eq!(view.rows[2].price, Price(110));
        assert!(view.rows.iter().all(|row| row.session_volume == 0));
    }

    #[test]
    fn agreement_reports_first_divergent_price() {
        let profile = ProfileRenderState {
            sessions: vec![ProfileSessionRender {
                rows: vec![row(100, 7), row(101, 9)],
                ..Default::default()
            }],
        };
        let dom = DomRenderState {
            rows: vec![
                DomPriceRow {
                    price: Price(99),
                    session_volume: 99,
                    ..Default::default()
                },
                DomPriceRow {
                    price: Price(100),
                    session_volume: 7,
                    ..Default::default()
                },
                DomPriceRow {
                    price: Price(101),
                    session_volume: 8,
                    ..Default::default()
                },
            ],
            ..Default::default()
        };
        assert_eq!(
            check_pane_agreement(&profile, &dom),
            Err(VolumeMismatch {
                price: Price(101),
                profile_volume: 9,
                dom_volume: 8,
            })
        );
    }

    #[test]
    fn agreement_counts_only_overlapping_prices() {
        let profile = ProfileRenderState {
            sessions: vec![ProfileSessionRender {
                rows: vec![row(100, 7)],
                ..Default::default()
            }],
        };
        let dom = DomRenderState {
            rows: vec![DomPriceRow {
                price: Price(100),
                session_volume: 7,
                ..Default::default()
            }],
            ..Default::default()
        };
        assert_eq!(check_pane_agreement(&profile, &dom), Ok(1));
    }

    #[test]
    fn footer_uses_prior_day_at_1800_new_york() {
        // 2026-07-29 CT trade date.
        assert_eq!(session_open_footer(20_663), "07-28 18:00");
        // 1970-01-02 trade date rolls the footer into the prior year boundary safely.
        assert_eq!(session_open_footer(1), "01-01 18:00");
    }

    #[test]
    fn open_marker_bucket_present_or_absent() {
        let with = ProfileSessionRender {
            open: Some(Price(103)),
            ..Default::default()
        };
        assert_eq!(open_marker_bucket(&with, Price(2)), Some(Price(102)));

        let without = ProfileSessionRender {
            open: None,
            ..Default::default()
        };
        assert_eq!(open_marker_bucket(&without, Price(2)), None);
    }
}
