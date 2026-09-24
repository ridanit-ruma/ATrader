//! Shadow order book: our fills deplete real liquidity (recovering with half-life `tau_res`) and
//! shift the price by a permanent-impact offset (decaying with half-life `tau_perm`).
//! Pure: time comes in as arguments.

use std::collections::HashMap;

use chrono::{DateTime, Utc};
use rust_decimal::Decimal;
use rust_decimal::prelude::{FromPrimitive, ToPrimitive};
use rust_decimal_macros::dec;

use crate::domain::{Level, Side, Venue};
use crate::venue::TickRule;

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SimParams {
    pub eta: f64,
    pub tau_perm_secs: f64,
    pub tau_res_secs: f64,
    pub max_slippage: Decimal,
    pub default_sigma: f64,
    pub default_adv: Decimal,
}

impl SimParams {
    pub fn default_for(venue: Venue) -> Self {
        let crypto = !venue.has_session();
        SimParams {
            eta: 0.5,
            tau_perm_secs: if crypto { 600.0 } else { 1800.0 },
            tau_res_secs: 60.0,
            max_slippage: dec!(0.05),
            default_sigma: 0.03,
            default_adv: match venue {
                Venue::Krx | Venue::Upbit => dec!(1000000000),
                Venue::Us => dec!(10000000),
                Venue::Binance => dec!(1000000),
            },
        }
    }
}

/// 20-day daily-return volatility and average daily traded value, in the quote currency.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct DailyStats {
    pub sigma: f64,
    pub adv_notional: Decimal,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ShadowLevel {
    pub price: Decimal,
    pub qty: Decimal,
    pub real_price: Decimal,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Slice {
    pub price: Decimal,
    pub qty: Decimal,
    pub real_price: Decimal,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Size {
    Qty(Decimal),
    Notional(Decimal),
}

#[derive(Debug, Clone, Default)]
pub struct ShadowState {
    /// Permanent impact as a relative price shift (0.01 = +1%).
    pub offset: f64,
    /// Liquidity we took, keyed by (side of the resting liquidity, real price level).
    depletion: HashMap<(Side, Decimal), Decimal>,
    updated_at: Option<DateTime<Utc>>,
}

fn half_life_factor(dt_secs: f64, tau_secs: f64) -> f64 {
    0.5f64.powf(dt_secs / tau_secs)
}

impl ShadowState {
    pub fn decay(&mut self, now: DateTime<Utc>, p: &SimParams) {
        if let Some(last) = self.updated_at {
            let dt = (now - last).num_milliseconds().max(0) as f64 / 1000.0;
            self.offset *= half_life_factor(dt, p.tau_perm_secs);
            // Far below any tick; keeps float dust from lingering forever.
            if self.offset.abs() < 1e-7 {
                self.offset = 0.0;
            }
            let f = Decimal::from_f64(half_life_factor(dt, p.tau_res_secs)).unwrap_or(Decimal::ZERO);
            self.depletion.retain(|_, q| {
                *q = (*q * f).round_dp(8);
                !q.is_zero()
            });
        }
        self.updated_at = Some(now);
    }

    /// A real price moved by the offset, snapped to the nearest valid tick.
    pub fn shift(&self, price: Decimal, tick: &TickRule) -> Decimal {
        let mult = Decimal::from_f64(1.0 + self.offset).unwrap_or(Decimal::ONE);
        tick.round(price * mult)
    }

    /// The shadow view of one side of the real book. `side` is the side of the resting liquidity:
    /// asks are `Sell`, bids are `Buy`.
    pub fn shadow_side(&self, levels: &[Level], side: Side, tick: &TickRule) -> Vec<ShadowLevel> {
        levels
            .iter()
            .filter_map(|l| {
                let qty = l.qty - self.depletion.get(&(side, l.price)).copied().unwrap_or_default();
                (qty > Decimal::ZERO).then(|| ShadowLevel { price: self.shift(l.price, tick), qty, real_price: l.price })
            })
            .collect()
    }

    /// Record liquidity a `taker` removed from the opposite side.
    pub fn consume(&mut self, taker: Side, slices: &[Slice]) {
        for s in slices {
            *self.depletion.entry((taker.opposite(), s.real_price)).or_default() += s.qty;
        }
    }
}

/// Take liquidity from `levels` (best first) until `size` is met or the next level is worse than
/// `limit`. Quantities are floored to `step`.
pub fn walk(levels: &[ShadowLevel], taker: Side, size: Size, limit: Decimal, step: Decimal) -> Vec<Slice> {
    let mut out = Vec::new();
    let mut left = size;
    for l in levels {
        let worse = match taker {
            Side::Buy => l.price > limit,
            Side::Sell => l.price < limit,
        };
        if worse {
            break;
        }
        let q = match left {
            Size::Qty(q) => l.qty.min(q),
            Size::Notional(n) => l.qty.min((n / l.price / step).floor() * step),
        };
        if q <= Decimal::ZERO {
            break;
        }
        out.push(Slice { price: l.price, qty: q, real_price: l.real_price });
        left = match left {
            Size::Qty(q0) => Size::Qty(q0 - q),
            Size::Notional(n) => Size::Notional(n - q * l.price),
        };
    }
    out
}

/// (total qty, total notional) of `slices`.
pub fn totals(slices: &[Slice]) -> (Decimal, Decimal) {
    slices.iter().fold((Decimal::ZERO, Decimal::ZERO), |(q, n), s| (q + s.qty, n + s.qty * s.price))
}

/// Permanent impact of one taker execution, square-root law:
/// `sign × η × σ × sqrt(notional / ADV)`.
pub fn impact(taker: Side, notional: Decimal, stats: DailyStats, p: &SimParams) -> f64 {
    if stats.adv_notional <= Decimal::ZERO {
        return 0.0;
    }
    let ratio = (notional / stats.adv_notional).to_f64().unwrap_or(0.0);
    taker.sign() * p.eta * stats.sigma * ratio.sqrt()
}

/// A resting limit order's place in the shadow book.
#[derive(Debug, Clone, PartialEq)]
pub struct Resting {
    pub order_id: u64,
    pub side: Side,
    pub price: Decimal,
    pub remaining: Decimal,
    pub queue_ahead: Decimal,
}

/// A real trade printed at `shadow_price` (already shifted). Consumes from `avail`, the trade's
/// unallocated size, and returns how much of `r` fills. At our price, the queue ahead of us goes
/// first. A print through our price fills us directly.
pub fn passive_fill(r: &mut Resting, shadow_price: Decimal, avail: &mut Decimal) -> Decimal {
    let through = match r.side {
        Side::Buy => shadow_price < r.price,
        Side::Sell => shadow_price > r.price,
    };
    if !through && shadow_price != r.price {
        return Decimal::ZERO;
    }
    if !through {
        let q = r.queue_ahead.min(*avail);
        r.queue_ahead -= q;
        *avail -= q;
    }
    let f = r.remaining.min(*avail);
    r.remaining -= f;
    *avail -= f;
    f
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{TimeZone, Utc};
    use rust_decimal_macros::dec;

    fn lv(price: Decimal, qty: Decimal) -> ShadowLevel {
        ShadowLevel { price, qty, real_price: price }
    }

    fn asks() -> Vec<ShadowLevel> {
        vec![lv(dec!(100), dec!(5)), lv(dec!(101), dec!(5)), lv(dec!(102), dec!(10))]
    }

    #[test]
    fn walk_takes_levels_in_order() {
        let s = walk(&asks(), Side::Buy, Size::Qty(dec!(8)), dec!(1000), dec!(1));
        assert_eq!(s.iter().map(|x| (x.price, x.qty)).collect::<Vec<_>>(), vec![(dec!(100), dec!(5)), (dec!(101), dec!(3))]);
    }

    #[test]
    fn walk_stops_at_limit() {
        let s = walk(&asks(), Side::Buy, Size::Qty(dec!(8)), dec!(100.5), dec!(1));
        assert_eq!(totals(&s), (dec!(5), dec!(500)));
    }

    #[test]
    fn walk_by_notional_respects_lot_step() {
        let s = walk(&asks(), Side::Buy, Size::Notional(dec!(1000)), dec!(1000), dec!(1));
        assert_eq!(totals(&s), (dec!(9), dec!(904)));
    }

    #[test]
    fn walk_sells_into_bids() {
        let bids = vec![lv(dec!(99), dec!(5)), lv(dec!(98), dec!(5))];
        let s = walk(&bids, Side::Sell, Size::Qty(dec!(7)), dec!(98.5), dec!(1));
        assert_eq!(totals(&s), (dec!(5), dec!(495)));
    }

    #[test]
    fn depletion_hides_liquidity_then_recovers() {
        let p = SimParams::default_for(Venue::Upbit);
        let t0 = Utc.with_ymd_and_hms(2026, 9, 23, 0, 0, 0).unwrap();
        let mut st = ShadowState::default();
        st.decay(t0, &p);
        st.consume(Side::Buy, &[Slice { price: dec!(100), qty: dec!(3), real_price: dec!(100) }]);
        let real = [Level { price: dec!(100), qty: dec!(5) }];
        let tick = TickRule::Fixed(dec!(1));
        assert_eq!(st.shadow_side(&real, Side::Sell, &tick)[0].qty, dec!(2));
        st.decay(t0 + chrono::Duration::seconds(60), &p); // one resilience half-life
        assert_eq!(st.shadow_side(&real, Side::Sell, &tick)[0].qty, dec!(3.5));
    }

    #[test]
    fn offset_decays_by_half_life() {
        let p = SimParams::default_for(Venue::Upbit); // tau_perm = 600 s
        let t0 = Utc.with_ymd_and_hms(2026, 9, 23, 0, 0, 0).unwrap();
        let mut st = ShadowState::default();
        st.decay(t0, &p);
        st.offset = 0.02;
        st.decay(t0 + chrono::Duration::seconds(600), &p);
        assert!((st.offset - 0.01).abs() < 1e-12);
    }

    #[test]
    fn tiny_offset_does_not_shift_the_book() {
        let mut st = ShadowState::default();
        st.offset = 1e-6;
        let real = [Level { price: dec!(100000000), qty: dec!(1) }];
        let shadow = st.shadow_side(&real, Side::Sell, &TickRule::Fixed(dec!(1000)));
        assert_eq!(shadow[0].price, dec!(100000000));
    }

    #[test]
    fn negligible_offset_snaps_to_zero() {
        let p = SimParams::default_for(Venue::Upbit);
        let t0 = Utc.with_ymd_and_hms(2026, 9, 23, 0, 0, 0).unwrap();
        let mut st = ShadowState::default();
        st.decay(t0, &p);
        st.offset = 1.5e-7;
        st.decay(t0 + chrono::Duration::seconds(600), &p);
        assert_eq!(st.offset, 0.0);
    }

    #[test]
    fn impact_follows_square_root_law() {
        let p = SimParams::default_for(Venue::Krx); // eta 0.5
        let stats = DailyStats { sigma: 0.02, adv_notional: dec!(1000000000) };
        assert!((impact(Side::Buy, dec!(10000000), stats, &p) - 0.001).abs() < 1e-12);
        assert!((impact(Side::Sell, dec!(10000000), stats, &p) + 0.001).abs() < 1e-12);
        assert_eq!(impact(Side::Buy, dec!(1), DailyStats { sigma: 0.02, adv_notional: dec!(0) }, &p), 0.0);
    }

    #[test]
    fn passive_fill_waits_for_queue_then_fills() {
        let mut r = Resting { order_id: 1, side: Side::Buy, price: dec!(100), remaining: dec!(10), queue_ahead: dec!(5) };
        let mut avail = dec!(8);
        assert_eq!(passive_fill(&mut r, dec!(100), &mut avail), dec!(3));
        assert_eq!((r.queue_ahead, r.remaining, avail), (dec!(0), dec!(7), dec!(0)));
        let mut avail = dec!(20);
        assert_eq!(passive_fill(&mut r, dec!(99), &mut avail), dec!(7)); // traded through
        let mut avail = dec!(20);
        assert_eq!(passive_fill(&mut r, dec!(101), &mut avail), dec!(0)); // above a buy: no fill
    }
}
