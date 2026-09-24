//! Technical indicators over closing prices. Every series is aligned to its input; values that
//! need more history than exists are `None`.

use rust_decimal::prelude::ToPrimitive;

use crate::candles::Candle;

type Series = Vec<Option<f64>>;

pub fn sma(x: &[f64], n: usize) -> Series {
    (0..x.len()).map(|i| (n > 0 && i + 1 >= n).then(|| x[i + 1 - n..=i].iter().sum::<f64>() / n as f64)).collect()
}

/// Exponential moving average seeded with the SMA of the first `n` values.
pub fn ema(x: &[f64], n: usize) -> Series {
    let mut out = vec![None; x.len()];
    if n == 0 || x.len() < n {
        return out;
    }
    let alpha = 2.0 / (n as f64 + 1.0);
    let mut prev = x[..n].iter().sum::<f64>() / n as f64;
    out[n - 1] = Some(prev);
    for i in n..x.len() {
        prev = alpha * x[i] + (1.0 - alpha) * prev;
        out[i] = Some(prev);
    }
    out
}

/// Wilder's RSI.
pub fn rsi(x: &[f64], n: usize) -> Series {
    let mut out = vec![None; x.len()];
    if n == 0 || x.len() <= n {
        return out;
    }
    let change = |i: usize| x[i] - x[i - 1];
    let (mut gain, mut loss) = (1..=n).fold((0.0, 0.0), |(g, l), i| (g + change(i).max(0.0), l + (-change(i)).max(0.0)));
    gain /= n as f64;
    loss /= n as f64;
    let value = |g: f64, l: f64| if l == 0.0 { if g == 0.0 { 50.0 } else { 100.0 } } else { 100.0 - 100.0 / (1.0 + g / l) };
    out[n] = Some(value(gain, loss));
    for i in n + 1..x.len() {
        gain = (gain * (n as f64 - 1.0) + change(i).max(0.0)) / n as f64;
        loss = (loss * (n as f64 - 1.0) + (-change(i)).max(0.0)) / n as f64;
        out[i] = Some(value(gain, loss));
    }
    out
}

pub fn macd(x: &[f64], fast: usize, slow: usize, signal: usize) -> [Series; 3] {
    let (f, s) = (ema(x, fast), ema(x, slow));
    let line: Series = f.iter().zip(&s).map(|(a, b)| Some((*a)? - (*b)?)).collect();
    let first = line.iter().position(Option::is_some);
    let mut sig = vec![None; x.len()];
    if let Some(start) = first {
        let tail: Vec<f64> = line[start..].iter().map(|v| v.unwrap_or_default()).collect();
        for (i, v) in ema(&tail, signal).into_iter().enumerate() {
            sig[start + i] = v;
        }
    }
    let hist = line.iter().zip(&sig).map(|(a, b)| Some((*a)? - (*b)?)).collect();
    [line, sig, hist]
}

pub fn bollinger(x: &[f64], n: usize, k: f64) -> [Series; 3] {
    let mid = sma(x, n);
    let dev: Series = (0..x.len())
        .map(|i| {
            let m = mid[i]?;
            Some((x[i + 1 - n..=i].iter().map(|v| (v - m).powi(2)).sum::<f64>() / n as f64).sqrt())
        })
        .collect();
    let band = |sign: f64| mid.iter().zip(&dev).map(|(m, d)| Some((*m)? + sign * k * (*d)?)).collect();
    [band(1.0), mid.clone(), band(-1.0)]
}

/// Wilder's average true range.
pub fn atr(high: &[f64], low: &[f64], close: &[f64], n: usize) -> Series {
    let len = high.len().min(low.len()).min(close.len());
    let tr: Vec<f64> = (0..len)
        .map(|i| {
            let range = high[i] - low[i];
            if i == 0 { range } else { range.max((high[i] - close[i - 1]).abs()).max((low[i] - close[i - 1]).abs()) }
        })
        .collect();
    let mut out = vec![None; len];
    if n == 0 || len < n {
        return out;
    }
    let mut prev = tr[..n].iter().sum::<f64>() / n as f64;
    out[n - 1] = Some(prev);
    for i in n..len {
        prev = (prev * (n as f64 - 1.0) + tr[i]) / n as f64;
        out[i] = Some(prev);
    }
    out
}

/// Sample stdev of the last `n` log returns, percent per bar.
pub fn volatility(x: &[f64], n: usize) -> Series {
    let rets: Vec<f64> = (1..x.len()).map(|i| (x[i] / x[i - 1]).ln()).collect();
    (0..x.len())
        .map(|i| {
            if n < 2 || i < n {
                return None;
            }
            let w = &rets[i - n..i];
            let mean = w.iter().sum::<f64>() / n as f64;
            let var = w.iter().map(|r| (r - mean).powi(2)).sum::<f64>() / (n as f64 - 1.0);
            var.is_finite().then(|| var.sqrt() * 100.0)
        })
        .collect()
}

#[derive(Debug, Clone, PartialEq)]
pub enum IndicatorSpec {
    Sma(usize),
    Ema(usize),
    Rsi(usize),
    Macd(usize, usize, usize),
    Bollinger(usize, f64),
    Atr(usize),
    Volatility(usize),
}

const MAX_PERIOD: usize = 500;

impl IndicatorSpec {
    /// `name[:param[:param]]`, e.g. `sma:20`, `rsi`, `macd:12:26:9`, `bb:20:2`.
    pub fn parse(s: &str) -> Result<IndicatorSpec, String> {
        let mut parts = s.trim().split(':');
        let name = parts.next().unwrap_or_default().to_lowercase();
        let params: Vec<&str> = parts.collect();
        let int = |i: usize, default: Option<usize>| -> Result<usize, String> {
            let v = match params.get(i) {
                Some(p) => p.parse::<usize>().map_err(|_| format!("{s}: {p} is not a whole number"))?,
                None => default.ok_or_else(|| format!("{s}: needs a period, e.g. {name}:20"))?,
            };
            if v == 0 || v > MAX_PERIOD {
                return Err(format!("{s}: period must be 1..={MAX_PERIOD}"));
            }
            Ok(v)
        };
        match name.as_str() {
            "sma" => Ok(IndicatorSpec::Sma(int(0, None)?)),
            "ema" => Ok(IndicatorSpec::Ema(int(0, None)?)),
            "rsi" => Ok(IndicatorSpec::Rsi(int(0, Some(14))?)),
            "macd" => Ok(IndicatorSpec::Macd(int(0, Some(12))?, int(1, Some(26))?, int(2, Some(9))?)),
            "bb" | "bollinger" => {
                let k = match params.get(1) {
                    Some(p) => p.parse::<f64>().map_err(|_| format!("{s}: {p} is not a number"))?,
                    None => 2.0,
                };
                if !(k > 0.0 && k <= 10.0) {
                    return Err(format!("{s}: width must be in (0, 10]"));
                }
                Ok(IndicatorSpec::Bollinger(int(0, Some(20))?, k))
            }
            "atr" => Ok(IndicatorSpec::Atr(int(0, Some(14))?)),
            "vol" | "volatility" => Ok(IndicatorSpec::Volatility(int(0, Some(20))?)),
            other => Err(format!("unknown indicator {other:?}; use sma, ema, rsi, macd, bb, atr or vol")),
        }
    }
}

/// Named output lines for `spec` over `candles` (oldest first).
pub fn compute(spec: &IndicatorSpec, candles: &[Candle]) -> Vec<(String, Series)> {
    let f = |pick: fn(&Candle) -> rust_decimal::Decimal| candles.iter().map(|c| pick(c).to_f64().unwrap_or(f64::NAN)).collect::<Vec<f64>>();
    let close = f(|c| c.close);
    match *spec {
        IndicatorSpec::Sma(n) => vec![(format!("sma_{n}"), sma(&close, n))],
        IndicatorSpec::Ema(n) => vec![(format!("ema_{n}"), ema(&close, n))],
        IndicatorSpec::Rsi(n) => vec![(format!("rsi_{n}"), rsi(&close, n))],
        IndicatorSpec::Macd(a, b, c) => {
            let [m, s, h] = macd(&close, a, b, c);
            vec![("macd".into(), m), ("macd_signal".into(), s), ("macd_hist".into(), h)]
        }
        IndicatorSpec::Bollinger(n, k) => {
            let [u, m, l] = bollinger(&close, n, k);
            vec![("bb_upper".into(), u), ("bb_middle".into(), m), ("bb_lower".into(), l)]
        }
        IndicatorSpec::Atr(n) => vec![(format!("atr_{n}"), atr(&f(|c| c.high), &f(|c| c.low), &close, n))],
        IndicatorSpec::Volatility(n) => vec![(format!("vol_{n}"), volatility(&close, n))],
    }
}
#[cfg(test)]
mod tests {
    use super::*;

    fn close(v: &[Option<f64>], i: usize, want: f64) {
        let got = v[i].unwrap_or_else(|| panic!("index {i} is None"));
        assert!((got - want).abs() < 1e-9, "index {i}: {got} vs {want}");
    }

    #[test]
    fn moving_averages() {
        let x = [1.0, 2.0, 3.0, 4.0, 5.0];
        let s = sma(&x, 3);
        assert_eq!((s[0], s[1]), (None, None));
        close(&s, 2, 2.0);
        close(&s, 4, 4.0);
        let e = ema(&x, 3);
        assert_eq!(e[1], None);
        close(&e, 2, 2.0);
        close(&e, 3, 3.0);
        close(&e, 4, 4.0);
    }

    #[test]
    fn rsi_extremes_and_bounds() {
        let up: Vec<f64> = (1..=20).map(f64::from).collect();
        close(&rsi(&up, 14), 19, 100.0);
        let down: Vec<f64> = (1..=20).rev().map(f64::from).collect();
        close(&rsi(&down, 14), 19, 0.0);
        let zig: Vec<f64> = (0..40).map(|i| if i % 2 == 0 { 10.0 } else { 11.0 }).collect();
        let r = rsi(&zig, 14)[39].unwrap();
        assert!((0.0..=100.0).contains(&r));
        assert_eq!(rsi(&up, 14)[13], None);
    }

    #[test]
    fn flat_series_give_flat_indicators() {
        let x = [7.0; 60];
        let [m, s, h] = macd(&x, 12, 26, 9);
        close(&m, 59, 0.0);
        close(&s, 59, 0.0);
        close(&h, 59, 0.0);
        let [u, mid, l] = bollinger(&x, 20, 2.0);
        close(&u, 59, 7.0);
        close(&mid, 59, 7.0);
        close(&l, 59, 7.0);
        close(&volatility(&x, 20), 59, 0.0);
        let hi = [9.0; 30];
        let lo = [7.0; 30];
        close(&atr(&hi, &lo, &[8.0; 30], 14), 29, 2.0);
    }

    #[test]
    fn short_history_is_all_none() {
        let x = [1.0, 2.0];
        assert!(sma(&x, 20).iter().all(Option::is_none));
        assert!(rsi(&x, 14).iter().all(Option::is_none));
        assert!(macd(&x, 12, 26, 9)[2].iter().all(Option::is_none));
        assert!(volatility(&[], 20).is_empty());
    }

    #[test]
    fn specs_parse_with_defaults_and_limits() {
        assert_eq!(IndicatorSpec::parse("sma:20"), Ok(IndicatorSpec::Sma(20)));
        assert_eq!(IndicatorSpec::parse("rsi"), Ok(IndicatorSpec::Rsi(14)));
        assert_eq!(IndicatorSpec::parse("macd"), Ok(IndicatorSpec::Macd(12, 26, 9)));
        assert_eq!(IndicatorSpec::parse("bb:20:2.5"), Ok(IndicatorSpec::Bollinger(20, 2.5)));
        assert!(IndicatorSpec::parse("sma").is_err());
        assert!(IndicatorSpec::parse("sma:0").is_err());
        assert!(IndicatorSpec::parse("sma:5000").is_err());
        assert!(IndicatorSpec::parse("magic:3").is_err());
    }
}
