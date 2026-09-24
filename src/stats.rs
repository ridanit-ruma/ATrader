use rust_decimal::Decimal;
use rust_decimal::prelude::ToPrimitive;

use crate::sim::DailyStats;

/// Volatility (sample σ of daily log returns) and average daily traded value, from history
/// ordered oldest first.
pub fn daily_stats(closes: &[Decimal], values: &[Decimal]) -> Option<DailyStats> {
    if closes.len() < 3 || values.is_empty() {
        return None;
    }
    let rets: Vec<f64> = closes
        .windows(2)
        .filter_map(|w| Some((w[1].to_f64()? / w[0].to_f64()?).ln()))
        .filter(|r| r.is_finite())
        .collect();
    if rets.len() < 2 {
        return None;
    }
    let n = rets.len() as f64;
    let mean = rets.iter().sum::<f64>() / n;
    let var = rets.iter().map(|r| (r - mean).powi(2)).sum::<f64>() / (n - 1.0);
    let adv = values.iter().sum::<Decimal>() / Decimal::from(values.len());
    Some(DailyStats { sigma: var.sqrt(), adv_notional: adv.round_dp(2) })
}
#[cfg(test)]
mod tests {
    use super::*;
    use rust_decimal_macros::dec;

    #[test]
    fn sigma_is_sample_stdev_of_log_returns() {
        let s = daily_stats(&[dec!(100), dec!(110), dec!(99)], &[dec!(10), dec!(20), dec!(30)]).unwrap();
        assert!((s.sigma - 0.141895).abs() < 1e-5, "sigma {}", s.sigma);
        assert_eq!(s.adv_notional, dec!(20));
    }

    #[test]
    fn flat_prices_have_zero_sigma() {
        let s = daily_stats(&[dec!(5), dec!(5), dec!(5), dec!(5)], &[dec!(1)]).unwrap();
        assert_eq!(s.sigma, 0.0);
    }

    #[test]
    fn too_little_history_is_none() {
        assert!(daily_stats(&[dec!(1), dec!(2)], &[dec!(1)]).is_none());
        assert!(daily_stats(&[dec!(1), dec!(2), dec!(3)], &[]).is_none());
        assert!(daily_stats(&[dec!(0), dec!(0), dec!(0)], &[dec!(1)]).is_none());
    }
}
