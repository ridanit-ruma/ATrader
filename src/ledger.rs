use std::collections::HashMap;

use rust_decimal::Decimal;

use crate::domain::{Currency, InstrumentId, Side};

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Position {
    pub qty: Decimal,
    /// Moving average cost per unit, excluding fees.
    pub avg_cost: Decimal,
    /// Quantity promised to open sell orders.
    pub reserved: Decimal,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Portfolio {
    pub cash: HashMap<Currency, Decimal>,
    pub reserved_cash: HashMap<Currency, Decimal>,
    pub positions: HashMap<InstrumentId, Position>,
}

impl Portfolio {
    pub fn new(initial: &[(Currency, Decimal)]) -> Self {
        Portfolio { cash: initial.iter().copied().collect(), ..Default::default() }
    }

    pub fn cash(&self, c: Currency) -> Decimal {
        self.cash.get(&c).copied().unwrap_or_default()
    }

    pub fn available_cash(&self, c: Currency) -> Decimal {
        self.cash(c) - self.reserved_cash.get(&c).copied().unwrap_or_default()
    }

    pub fn available_qty(&self, id: &InstrumentId) -> Decimal {
        self.positions.get(id).map_or(Decimal::ZERO, |p| p.qty - p.reserved)
    }

    pub fn reserve_cash(&mut self, c: Currency, amount: Decimal) {
        *self.reserved_cash.entry(c).or_default() += amount;
    }

    pub fn release_cash(&mut self, c: Currency, amount: Decimal) {
        let r = self.reserved_cash.entry(c).or_default();
        *r = (*r - amount).max(Decimal::ZERO);
    }

    pub fn reserve_qty(&mut self, id: &InstrumentId, qty: Decimal) {
        self.positions.entry(id.clone()).or_default().reserved += qty;
    }

    pub fn release_qty(&mut self, id: &InstrumentId, qty: Decimal) {
        if let Some(p) = self.positions.get_mut(id) {
            p.reserved = (p.reserved - qty).max(Decimal::ZERO);
        }
    }

    /// Book one execution. Returns realised PnL (net of this sale's fee and tax) for sells.
    pub fn apply_fill(
        &mut self,
        id: &InstrumentId,
        side: Side,
        qty: Decimal,
        notional: Decimal,
        fee: Decimal,
        tax: Decimal,
    ) -> Option<Decimal> {
        let cash = self.cash.entry(id.venue.currency()).or_default();
        match side {
            Side::Buy => {
                *cash -= notional + fee + tax;
                let p = self.positions.entry(id.clone()).or_default();
                p.avg_cost = ((p.qty * p.avg_cost + notional) / (p.qty + qty)).round_dp(8);
                p.qty += qty;
                None
            }
            Side::Sell => {
                *cash += notional - fee - tax;
                let p = self.positions.get_mut(id).expect("sell of a position the broker checked");
                let pnl = notional - p.avg_cost * qty - fee - tax;
                p.qty -= qty;
                if p.qty.is_zero() && p.reserved.is_zero() {
                    self.positions.remove(id);
                }
                Some(pnl)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rust_decimal_macros::dec;

    fn id() -> InstrumentId {
        "KRX:005930".parse().unwrap()
    }

    #[test]
    fn moving_average_cost_and_realised_pnl() {
        let mut p = Portfolio::new(&[(Currency::Krw, dec!(10000))]);
        assert_eq!(p.apply_fill(&id(), Side::Buy, dec!(10), dec!(1000), dec!(1), dec!(0)), None);
        p.apply_fill(&id(), Side::Buy, dec!(10), dec!(1100), dec!(1), dec!(0));
        assert_eq!(p.positions[&id()].avg_cost, dec!(105));
        let pnl = p.apply_fill(&id(), Side::Sell, dec!(5), dec!(600), dec!(1), dec!(2));
        assert_eq!(pnl, Some(dec!(72))); // 600 - 105*5 - 1 - 2
        assert_eq!(p.positions[&id()].qty, dec!(15));
        assert_eq!(p.cash(Currency::Krw), dec!(10000) - dec!(1001) - dec!(1101) + dec!(597));
    }

    #[test]
    fn flat_position_is_removed() {
        let mut p = Portfolio::new(&[(Currency::Krw, dec!(10000))]);
        p.apply_fill(&id(), Side::Buy, dec!(2), dec!(200), dec!(0), dec!(0));
        p.apply_fill(&id(), Side::Sell, dec!(2), dec!(300), dec!(0), dec!(0));
        assert!(p.positions.is_empty());
        assert_eq!(p.available_qty(&id()), dec!(0));
    }

    #[test]
    fn reservations_reduce_what_is_available() {
        let mut p = Portfolio::new(&[(Currency::Krw, dec!(1000))]);
        p.reserve_cash(Currency::Krw, dec!(300));
        assert_eq!(p.available_cash(Currency::Krw), dec!(700));
        p.release_cash(Currency::Krw, dec!(500)); // over-release clamps at zero
        assert_eq!(p.available_cash(Currency::Krw), dec!(1000));
        p.apply_fill(&id(), Side::Buy, dec!(10), dec!(100), dec!(0), dec!(0));
        p.reserve_qty(&id(), dec!(4));
        assert_eq!(p.available_qty(&id()), dec!(6));
        p.release_qty(&id(), dec!(4));
        assert_eq!(p.available_qty(&id()), dec!(10));
    }
}
