use rust_decimal::Decimal;
use rust_decimal_macros::dec;

use crate::domain::{InstrumentId, Side, Venue};

#[derive(Debug, Clone, PartialEq)]
pub enum TickRule {
    /// KRX unified price-band table (KOSPI and KOSDAQ, since 2023-01-25).
    Krx,
    /// US equities: $0.01 at or above $1, $0.0001 below.
    Us,
    /// One tick size for every price (crypto, from exchange metadata).
    Fixed(Decimal),
}

/// (upper bound exclusive, tick) pairs; 1,000 above the last bound.
const KRX_TICKS: [(i64, i64); 6] =
    [(2_000, 1), (5_000, 5), (20_000, 10), (50_000, 50), (200_000, 100), (500_000, 500)];

impl TickRule {
    pub fn size_at(&self, price: Decimal) -> Decimal {
        match self {
            TickRule::Krx => KRX_TICKS
                .iter()
                .find(|(upper, _)| price < Decimal::from(*upper))
                .map_or(dec!(1000), |(_, t)| Decimal::from(*t)),
            TickRule::Us => {
                if price >= Decimal::ONE {
                    dec!(0.01)
                } else {
                    dec!(0.0001)
                }
            }
            TickRule::Fixed(t) => *t,
        }
    }

    pub fn floor(&self, price: Decimal) -> Decimal {
        let t = self.size_at(price);
        (price / t).floor() * t
    }

    pub fn ceil(&self, price: Decimal) -> Decimal {
        let t = self.size_at(price);
        (price / t).ceil() * t
    }

    pub fn round(&self, price: Decimal) -> Decimal {
        let t = self.size_at(price);
        (price / t).round() * t
    }

    pub fn is_valid(&self, price: Decimal) -> bool {
        price > Decimal::ZERO && (price % self.size_at(price)).is_zero()
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct LotRule {
    pub step: Decimal,
    pub min_qty: Decimal,
    pub min_notional: Decimal,
}

impl LotRule {
    pub fn is_valid(&self, qty: Decimal, price: Decimal) -> bool {
        qty > Decimal::ZERO
            && (qty % self.step).is_zero()
            && qty >= self.min_qty
            && qty * price >= self.min_notional
    }
}

/// Integer shares, no minimum notional (KRX and US in v1).
pub fn whole_shares() -> LotRule {
    LotRule { step: Decimal::ONE, min_qty: Decimal::ONE, min_notional: Decimal::ZERO }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Instrument {
    pub id: InstrumentId,
    pub name: String,
    pub tick: TickRule,
    pub lot: LotRule,
    pub tradable: bool,
}

/// KRX daily price limit: ±30% of the previous close, snapped inward to valid ticks.
/// Other venues have none.
pub fn price_band(venue: Venue, tick: &TickRule, prev_close: Option<Decimal>) -> Option<(Decimal, Decimal)> {
    if venue != Venue::Krx {
        return None;
    }
    let p = prev_close?;
    Some((tick.ceil(p * dec!(0.7)), tick.floor(p * dec!(1.3))))
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct FeeSchedule {
    pub commission_bps: Decimal,
    pub sell_tax_bps: Decimal,
    pub sell_reg_fee_bps: Decimal,
}

impl FeeSchedule {
    /// Defaults per spec §3. The KRX securities transaction tax (20 bps) follows the law in force
    /// for 2026; check it when the law changes.
    // ponytail: hard-coded defaults; fees.toml with effective dates lands in Phase 8.
    pub fn default_for(venue: Venue) -> Self {
        let (commission_bps, sell_tax_bps, sell_reg_fee_bps) = match venue {
            Venue::Krx => (dec!(1.5), dec!(20), dec!(0)),
            Venue::Us => (dec!(0), dec!(0), dec!(0.278)),
            Venue::Upbit => (dec!(5), dec!(0), dec!(0)),
            Venue::Binance => (dec!(10), dec!(0), dec!(0)),
        };
        FeeSchedule { commission_bps, sell_tax_bps, sell_reg_fee_bps }
    }

    /// (fee, tax) for one execution of `notional`, rounded to `dp` decimal places.
    pub fn cost(&self, side: Side, notional: Decimal, dp: u32) -> (Decimal, Decimal) {
        let bps = |b: Decimal| notional * b / dec!(10000);
        let mut fee = bps(self.commission_bps);
        let mut tax = Decimal::ZERO;
        if side == Side::Sell {
            fee += bps(self.sell_reg_fee_bps);
            tax = bps(self.sell_tax_bps);
        }
        (fee.round_dp(dp), tax.round_dp(dp))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rust_decimal_macros::dec;

    #[test]
    fn krx_ticks_follow_price_bands() {
        let t = TickRule::Krx;
        assert_eq!(t.size_at(dec!(1999)), dec!(1));
        assert_eq!(t.size_at(dec!(2000)), dec!(5));
        assert_eq!(t.size_at(dec!(49950)), dec!(50));
        assert_eq!(t.size_at(dec!(50000)), dec!(100));
        assert_eq!(t.size_at(dec!(600000)), dec!(1000));
        assert!(!t.is_valid(dec!(50050)));
        assert!(t.is_valid(dec!(50100)));
        assert_eq!((t.floor(dec!(50050)), t.ceil(dec!(50050))), (dec!(50000), dec!(50100)));
    }

    #[test]
    fn us_ticks() {
        let t = TickRule::Us;
        assert_eq!(t.size_at(dec!(12.345)), dec!(0.01));
        assert_eq!(t.size_at(dec!(0.5)), dec!(0.0001));
        assert!(t.is_valid(dec!(12.34)));
        assert!(!t.is_valid(dec!(12.345)));
        assert!(!t.is_valid(dec!(0)));
    }

    #[test]
    fn lot_rules() {
        let shares = whole_shares();
        assert!(shares.is_valid(dec!(1), dec!(100)));
        assert!(!shares.is_valid(dec!(0), dec!(100)));
        assert!(!shares.is_valid(dec!(-1), dec!(100)));
        assert!(!shares.is_valid(dec!(1.5), dec!(100)));
        let btc = LotRule { step: dec!(0.00000001), min_qty: dec!(0.00000001), min_notional: dec!(5000) };
        assert!(btc.is_valid(dec!(0.0001), dec!(100000000)));
        assert!(!btc.is_valid(dec!(0.00001), dec!(100000000)));
        assert!(!btc.is_valid(dec!(0.000000001), dec!(100000000)));
    }

    #[test]
    fn krx_price_band_is_thirty_percent() {
        assert_eq!(price_band(Venue::Krx, &TickRule::Krx, Some(dec!(70000))), Some((dec!(49000), dec!(91000))));
        assert_eq!(price_band(Venue::Krx, &TickRule::Krx, None), None);
        assert_eq!(price_band(Venue::Us, &TickRule::Us, Some(dec!(100))), None);
    }

    #[test]
    fn fees_and_taxes() {
        let krx = FeeSchedule::default_for(Venue::Krx);
        assert_eq!(krx.cost(Side::Sell, dec!(1000000), 0), (dec!(150), dec!(2000)));
        assert_eq!(krx.cost(Side::Buy, dec!(1000000), 0), (dec!(150), dec!(0)));
        let us = FeeSchedule::default_for(Venue::Us);
        assert_eq!(us.cost(Side::Sell, dec!(10000), 2), (dec!(0.28), dec!(0)));
        let upbit = FeeSchedule::default_for(Venue::Upbit);
        assert_eq!(upbit.cost(Side::Buy, dec!(1000000), 0), (dec!(500), dec!(0)));
    }
}
