//! Venue rankings for discovering what to look at.

use rust_decimal::Decimal;

use crate::domain::InstrumentId;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Ranking {
    Gainers,
    Losers,
    Volume,
    Value,
}

impl Ranking {
    pub fn parse(s: &str) -> Option<Ranking> {
        match s.trim() {
            "gainers" => Some(Ranking::Gainers),
            "losers" => Some(Ranking::Losers),
            "volume" => Some(Ranking::Volume),
            "value" => Some(Ranking::Value),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct ScreenRow {
    pub id: InstrumentId,
    pub name: Option<String>,
    pub price: Decimal,
    /// Change versus the previous close (24 h for crypto), percent.
    pub change_pct: Decimal,
    pub volume: Decimal,
    /// Traded value in the quote currency.
    pub value: Decimal,
}

/// Sort `rows` by `ranking` (best first) and keep `limit`.
pub fn rank(mut rows: Vec<ScreenRow>, ranking: Ranking, limit: usize) -> Vec<ScreenRow> {
    match ranking {
        Ranking::Gainers => rows.sort_by(|a, b| b.change_pct.cmp(&a.change_pct)),
        Ranking::Losers => rows.sort_by(|a, b| a.change_pct.cmp(&b.change_pct)),
        Ranking::Volume => rows.sort_by(|a, b| b.volume.cmp(&a.volume)),
        Ranking::Value => rows.sort_by(|a, b| b.value.cmp(&a.value)),
    }
    rows.truncate(limit);
    rows
}

#[cfg(test)]
mod tests {
    use super::*;
    use rust_decimal_macros::dec;

    fn row(sym: &str, chg: Decimal, vol: Decimal, val: Decimal) -> ScreenRow {
        ScreenRow { id: format!("UPBIT:{sym}").parse().unwrap(), name: None, price: dec!(1), change_pct: chg, volume: vol, value: val }
    }

    #[test]
    fn ranks_by_the_chosen_metric() {
        let rows = vec![row("A", dec!(5), dec!(1), dec!(30)), row("B", dec!(-7), dec!(9), dec!(10)), row("C", dec!(1), dec!(3), dec!(20))];
        let syms = |r: Vec<ScreenRow>| r.into_iter().map(|x| x.id.symbol).collect::<Vec<_>>();
        assert_eq!(syms(rank(rows.clone(), Ranking::Gainers, 2)), vec!["A", "C"]);
        assert_eq!(syms(rank(rows.clone(), Ranking::Losers, 1)), vec!["B"]);
        assert_eq!(syms(rank(rows.clone(), Ranking::Volume, 3)), vec!["B", "C", "A"]);
        assert_eq!(syms(rank(rows, Ranking::Value, 1)), vec!["A"]);
        assert_eq!(Ranking::parse("value"), Some(Ranking::Value));
        assert_eq!(Ranking::parse("hot"), None);
    }
}
