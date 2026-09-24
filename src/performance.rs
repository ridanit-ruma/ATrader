//! Account performance from equity snapshots and fills.

use chrono::{DateTime, Utc};
use rust_decimal::Decimal;
use rust_decimal::prelude::ToPrimitive;

use crate::app::krw_per;
use crate::broker::Fill;
use crate::domain::Side;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SnapshotKind {
    Minute,
    Daily,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Snapshot {
    pub account: String,
    pub generation: i32,
    pub at: DateTime<Utc>,
    pub kind: SnapshotKind,
    pub equity_krw: Decimal,
    pub cash_krw: Decimal,
    pub positions_krw: Decimal,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Performance {
    pub start_equity_krw: Decimal,
    pub end_equity_krw: Decimal,
    pub return_pct: Decimal,
    pub max_drawdown_pct: Decimal,
    /// Annualized (√365) stdev of daily returns, percent; needs 2+ daily returns.
    pub volatility_pct: Option<f64>,
    /// Annualized mean/stdev of daily returns, risk-free 0.
    pub sharpe: Option<f64>,
    pub trades: usize,
    pub sells: usize,
    pub win_rate_pct: Option<Decimal>,
    pub realized_pnl_krw: Decimal,
    pub fees_krw: Decimal,
    /// Traded notional (KRW) over average equity.
    pub turnover: Option<Decimal>,
}

/// `snapshots` oldest first (any kinds), `fills` within the same period.
pub fn performance(snapshots: &[Snapshot], fills: &[Fill], usd_krw: Decimal) -> Performance {
    let equity: Vec<Decimal> = snapshots.iter().map(|s| s.equity_krw).collect();
    let start = equity.first().copied().unwrap_or_default();
    let end = equity.last().copied().unwrap_or_default();
    let return_pct = if start > Decimal::ZERO { ((end - start) / start * Decimal::ONE_HUNDRED).round_dp(2) } else { Decimal::ZERO };

    let mut peak = Decimal::ZERO;
    let mut max_dd = Decimal::ZERO;
    for e in &equity {
        peak = peak.max(*e);
        if peak > Decimal::ZERO {
            max_dd = max_dd.max((peak - e) / peak * Decimal::ONE_HUNDRED);
        }
    }

    let daily: Vec<f64> = snapshots.iter().filter(|s| s.kind == SnapshotKind::Daily).filter_map(|s| s.equity_krw.to_f64()).collect();
    let rets: Vec<f64> = daily.windows(2).filter(|w| w[0] > 0.0).map(|w| w[1] / w[0] - 1.0).collect();
    let (volatility_pct, sharpe) = if rets.len() >= 2 {
        let n = rets.len() as f64;
        let mean = rets.iter().sum::<f64>() / n;
        let sd = (rets.iter().map(|r| (r - mean).powi(2)).sum::<f64>() / (n - 1.0)).sqrt();
        let annual = 365f64.sqrt();
        (Some(sd * annual * 100.0), (sd > 0.0).then(|| mean / sd * annual))
    } else {
        (None, None)
    };

    let krw = |f: &Fill, v: Decimal| v * krw_per(f.instrument.venue.currency(), usd_krw);
    let sells: Vec<&Fill> = fills.iter().filter(|f| f.side == Side::Sell).collect();
    let wins = sells.iter().filter(|f| f.realized_pnl.is_some_and(|p| p > Decimal::ZERO)).count();
    let traded: Decimal = fills.iter().map(|f| krw(f, f.notional)).sum();
    let avg_equity = if equity.is_empty() { Decimal::ZERO } else { equity.iter().sum::<Decimal>() / Decimal::from(equity.len()) };

    Performance {
        start_equity_krw: start,
        end_equity_krw: end,
        return_pct,
        max_drawdown_pct: max_dd.round_dp(2),
        volatility_pct,
        sharpe,
        trades: fills.len(),
        sells: sells.len(),
        win_rate_pct: (!sells.is_empty()).then(|| (Decimal::from(wins) / Decimal::from(sells.len()) * Decimal::ONE_HUNDRED).round_dp(2)),
        realized_pnl_krw: sells.iter().map(|f| krw(f, f.realized_pnl.unwrap_or_default())).sum::<Decimal>().round_dp(0),
        fees_krw: fills.iter().map(|f| krw(f, f.fee + f.tax)).sum::<Decimal>().round_dp(0),
        turnover: (avg_equity > Decimal::ZERO && !fills.is_empty()).then(|| (traded / avg_equity).round_dp(2)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::broker::Liquidity;
    use chrono::TimeZone;
    use rust_decimal_macros::dec;

    fn snap(day: u32, kind: SnapshotKind, equity: Decimal) -> Snapshot {
        Snapshot {
            account: "a".into(),
            generation: 1,
            at: Utc.with_ymd_and_hms(2026, 9, day, 0, 0, 0).unwrap(),
            kind,
            equity_krw: equity,
            cash_krw: equity,
            positions_krw: Decimal::ZERO,
        }
    }

    fn sell(pnl: Decimal) -> Fill {
        Fill {
            order_id: 1,
            account: "a".into(),
            instrument: "UPBIT:KRW-BTC".parse().unwrap(),
            side: crate::domain::Side::Sell,
            qty: dec!(1),
            notional: dec!(1000),
            price: dec!(1000),
            fee: dec!(1),
            tax: dec!(0),
            realized_pnl: Some(pnl),
            liquidity: Liquidity::Taker,
            at: Utc.with_ymd_and_hms(2026, 9, 2, 0, 0, 0).unwrap(),
        }
    }

    #[test]
    fn returns_drawdown_and_trade_stats() {
        let s = vec![
            snap(1, SnapshotKind::Daily, dec!(100)),
            snap(2, SnapshotKind::Daily, dec!(120)),
            snap(3, SnapshotKind::Daily, dec!(90)),
            snap(4, SnapshotKind::Daily, dec!(110)),
        ];
        let p = performance(&s, &[sell(dec!(50)), sell(dec!(-10))], dec!(1400));
        assert_eq!(p.return_pct, dec!(10));
        assert_eq!(p.max_drawdown_pct, dec!(25)); // 120 -> 90
        assert_eq!((p.trades, p.sells), (2, 2));
        assert_eq!(p.win_rate_pct, Some(dec!(50)));
        assert_eq!(p.realized_pnl_krw, dec!(40));
        assert_eq!(p.fees_krw, dec!(2));
        assert!(p.volatility_pct.unwrap() > 0.0);
        assert!(p.sharpe.is_some());
        assert_eq!(p.turnover, Some((dec!(2000) / dec!(105)).round_dp(2)));
    }

    #[test]
    fn degenerate_inputs_do_not_divide_by_zero() {
        let empty = performance(&[], &[], dec!(1400));
        assert_eq!((empty.return_pct, empty.max_drawdown_pct, empty.win_rate_pct, empty.turnover), (dec!(0), dec!(0), None, None));
        let one = performance(&[snap(1, SnapshotKind::Minute, dec!(100))], &[], dec!(1400));
        assert_eq!((one.return_pct, one.volatility_pct, one.sharpe), (dec!(0), None, None));
        let zero = performance(&[snap(1, SnapshotKind::Daily, dec!(0)), snap(2, SnapshotKind::Daily, dec!(0))], &[], dec!(1400));
        assert_eq!(zero.return_pct, dec!(0));
    }
}
