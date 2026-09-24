//! OHLCV candles: intervals, resampling, and 1-minute bars built from trade prints.

use std::collections::HashMap;

use chrono::{DateTime, Utc};
use rust_decimal::Decimal;

use crate::domain::{InstrumentId, Trade};

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Candle {
    pub start: DateTime<Utc>,
    pub open: Decimal,
    pub high: Decimal,
    pub low: Decimal,
    pub close: Decimal,
    /// Base-asset volume (shares or coins).
    pub volume: Decimal,
    /// Traded value in the quote currency.
    pub value: Decimal,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Interval {
    M1,
    M5,
    M15,
    H1,
    D1,
    W1,
}

impl Interval {
    pub const ALL: [Interval; 6] = [Interval::M1, Interval::M5, Interval::M15, Interval::H1, Interval::D1, Interval::W1];

    pub fn code(self) -> &'static str {
        match self {
            Interval::M1 => "1m",
            Interval::M5 => "5m",
            Interval::M15 => "15m",
            Interval::H1 => "1h",
            Interval::D1 => "1d",
            Interval::W1 => "1w",
        }
    }

    pub fn parse(s: &str) -> Option<Interval> {
        Interval::ALL.into_iter().find(|i| i.code() == s.trim())
    }

    pub fn secs(self) -> i64 {
        match self {
            Interval::M1 => 60,
            Interval::M5 => 300,
            Interval::M15 => 900,
            Interval::H1 => 3600,
            Interval::D1 => 86400,
            Interval::W1 => 7 * 86400,
        }
    }

    pub fn is_intraday(self) -> bool {
        self.secs() < 86400
    }

    /// Bucket start for `t`. Weeks start Monday 00:00 UTC (the Unix epoch was a Thursday).
    fn bucket(self, t: DateTime<Utc>) -> DateTime<Utc> {
        let offset = if self == Interval::W1 { 3 * 86400 } else { 0 };
        let s = t.timestamp() + offset;
        DateTime::from_timestamp(s - s.rem_euclid(self.secs()) - offset, 0).expect("in range")
    }
}

/// Merge candles (oldest first) into `interval` buckets.
pub fn resample(candles: &[Candle], interval: Interval) -> Vec<Candle> {
    let mut out: Vec<Candle> = Vec::new();
    for c in candles {
        let start = interval.bucket(c.start);
        match out.last_mut() {
            Some(last) if last.start == start => {
                last.high = last.high.max(c.high);
                last.low = last.low.min(c.low);
                last.close = c.close;
                last.volume += c.volume;
                last.value += c.value;
            }
            _ => out.push(Candle { start, ..*c }),
        }
    }
    out
}

/// Rolls trade prints into 1-minute bars per instrument.
#[derive(Debug, Default)]
pub struct BarBuilder {
    open: HashMap<InstrumentId, Candle>,
}

impl BarBuilder {
    /// Add a print; returns the previous bar of this instrument if the print started a new minute.
    pub fn on_trade(&mut self, t: &Trade) -> Option<(InstrumentId, Candle)> {
        let start = Interval::M1.bucket(t.at);
        let fresh = Candle { start, open: t.price, high: t.price, low: t.price, close: t.price, volume: t.qty, value: t.price * t.qty };
        match self.open.get_mut(&t.instrument) {
            Some(bar) if bar.start == start => {
                bar.high = bar.high.max(t.price);
                bar.low = bar.low.min(t.price);
                bar.close = t.price;
                bar.volume += t.qty;
                bar.value += t.price * t.qty;
                None
            }
            Some(bar) if bar.start < start => {
                let done = std::mem::replace(bar, fresh);
                Some((t.instrument.clone(), done))
            }
            Some(_) => None, // a late print for an already-closed minute
            None => {
                self.open.insert(t.instrument.clone(), fresh);
                None
            }
        }
    }

    /// Close and return every open bar whose minute started before `cutoff`.
    pub fn flush_before(&mut self, cutoff: DateTime<Utc>) -> Vec<(InstrumentId, Candle)> {
        let stale: Vec<InstrumentId> = self.open.iter().filter(|(_, c)| c.start < cutoff).map(|(id, _)| id.clone()).collect();
        stale.into_iter().filter_map(|id| self.open.remove(&id).map(|c| (id, c))).collect()
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;
    use rust_decimal_macros::dec;

    fn t(h: u32, m: u32, s: u32) -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 9, 23, h, m, s).unwrap()
    }

    fn bar(h: u32, m: u32, o: Decimal, hi: Decimal, lo: Decimal, c: Decimal) -> Candle {
        Candle { start: t(h, m, 0), open: o, high: hi, low: lo, close: c, volume: dec!(1), value: c }
    }

    #[test]
    fn intervals_parse_and_measure() {
        assert_eq!(Interval::parse("15m"), Some(Interval::M15));
        assert_eq!(Interval::parse("1w").map(Interval::secs), Some(7 * 86400));
        assert_eq!(Interval::parse("2h"), None);
        assert!(Interval::H1.is_intraday() && !Interval::D1.is_intraday());
    }

    #[test]
    fn resample_aligns_buckets_across_gaps() {
        let bars = vec![
            bar(1, 3, dec!(10), dec!(11), dec!(9), dec!(10)),
            bar(1, 4, dec!(10), dec!(15), dec!(10), dec!(14)),
            bar(1, 7, dec!(14), dec!(14), dec!(8), dec!(9)), // gap at 1:05-1:06
            bar(1, 59, dec!(9), dec!(9), dec!(9), dec!(9)),
            bar(2, 0, dec!(9), dec!(12), dec!(9), dec!(12)),
        ];
        let five = resample(&bars, Interval::M5);
        assert_eq!(five.iter().map(|c| c.start).collect::<Vec<_>>(), vec![t(1, 0, 0), t(1, 5, 0), t(1, 55, 0), t(2, 0, 0)]);
        assert_eq!((five[0].open, five[0].high, five[0].low, five[0].close), (dec!(10), dec!(15), dec!(9), dec!(14)));
        assert_eq!(five[0].volume, dec!(2));
        let hour = resample(&bars, Interval::H1);
        assert_eq!(hour.len(), 2);
        assert_eq!((hour[0].open, hour[0].high, hour[0].low, hour[0].close), (dec!(10), dec!(15), dec!(8), dec!(9)));
    }

    #[test]
    fn bar_builder_closes_minutes() {
        let mut b = BarBuilder::default();
        let id: InstrumentId = "KRX:005930".parse().unwrap();
        let tr = |p: Decimal, q: Decimal, at| Trade { instrument: id.clone(), price: p, qty: q, at };
        assert!(b.on_trade(&tr(dec!(100), dec!(2), t(1, 0, 5))).is_none());
        assert!(b.on_trade(&tr(dec!(103), dec!(1), t(1, 0, 40))).is_none());
        assert!(b.on_trade(&tr(dec!(99), dec!(1), t(1, 0, 59))).is_none());
        let (done_id, done) = b.on_trade(&tr(dec!(101), dec!(1), t(1, 1, 2))).unwrap();
        assert_eq!(done_id, id);
        assert_eq!((done.start, done.open, done.high, done.low, done.close), (t(1, 0, 0), dec!(100), dec!(103), dec!(99), dec!(99)));
        assert_eq!((done.volume, done.value), (dec!(4), dec!(200) + dec!(103) + dec!(99)));
        let flushed = b.flush_before(t(1, 2, 0));
        assert_eq!(flushed.len(), 1);
        assert_eq!(flushed[0].1.start, t(1, 1, 0));
        assert!(b.flush_before(t(1, 3, 0)).is_empty());
    }
}
