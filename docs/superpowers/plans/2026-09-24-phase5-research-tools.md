# ATrader Phase 5 (Research Tools: Candles, Indicators, Screener, Performance) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Give the agent the structured research data the spec promises and ATrader can supply itself:
- OHLCV candles from 1-minute to weekly on every venue;
- server-side technical indicators;
- venue rankings (a screener);
- account performance statistics built on stored equity snapshots.

**Architecture:**
- **Candles.** `MarketFeed` gains `candles` and `screen`, with default "unsupported" implementations. Crypto venues serve every interval natively. KIS serves daily and weekly. For KRX and US minute intervals, a `BarBuilder` rolls our own trade prints into 1-minute bars, stores them, and resamples them.
- **Indicators** are pure `f64` functions over candles.
- **Snapshots.** A snapshot task values every account each minute and writes a daily close at 00:00 KST.
- **Performance** is a pure function over snapshots and fills.
- **Tools.** Four new `trader` tools expose all of this.

Fundamentals (DART and EDGAR filings and financials) are Phase 5b.

**Tech Stack:** nothing new.

**Spec:** `docs/superpowers/specs/2026-09-24-atrader-design.md`. This plan covers §15 step 5 minus fundamentals: the §7 rows `screen`, `get_candles`, `get_indicators` and `get_performance`, the §4 "Bars", and the §6 equity snapshots.

## Global Constraints

- Earlier phases' constraints still apply.
- Intervals are `1m 5m 15m 1h 1d 1w`. Candles are returned oldest first, and the last one may still be forming. `limit` is between 1 and 200, default 100.
- Candle sources:

  | Venue | Intervals | Source |
  | --- | --- | --- |
  | Upbit | all | `/v1/candles/minutes/{1,5,15,60}`, `/days`, `/weeks` |
  | Binance | all | `/klines` |
  | KRX (KIS) | `1d`, `1w` | `FHKST03010100` (`D`/`W`) |
  | US (KIS) | `1d`, `1w` | `HHDFS76240000` (`GUBN` `0`/`1`) |
  | KRX, US | minute intervals | resampled from the stored 1-minute `bars` table |

- Indicator specs are `name[:param[:param]]`: `sma:N`, `ema:N`, `rsi[:N=14]`, `macd[:12:26:9]`, `bb[:20:2]`, `atr[:14]`, `vol[:20]`. Here `vol` is the stdev of log returns per bar, in percent. At most 8 indicators per call. `points` (default 1, max 100) is how many of the latest values to return.
- Rankings are `gainers losers volume value`, with `limit` between 1 and 50 (default 20).

  | Venue | Source |
  | --- | --- |
  | Upbit | `/v1/ticker/all?quote_currencies=KRW` |
  | Binance | `/api/v3/ticker/24hr` (USDT pairs) |
  | KRX (KIS) | `FHPST01700000` fluctuation (sort `0000` gainers / `0001` losers), `FHPST01710000` volume rank (`FID_BLNG_CLS_CODE` `0` volume, `3` value) |
  | US (KIS) | `HHDFS76290000` updown-rate (`GUBN` `1`/`0`), `HHDFS76310010` trade-vol, `HHDFS76320010` trade-pbmn, merged over `NAS` and `NYS` |

  The KRX losers sort code and the US ranking parameters are unverified until someone runs them with KIS keys. They are recorded as rulings.
- Snapshots are taken every 60 s for every restored account. Prices are shadow mids from books already in memory, with no REST calls; a position without a book is valued at average cost. A `daily` snapshot is written at the first tick of each KST date and stamped 00:00 KST.
- Performance periods are `1d 1w 1m 3m all`. The formulas:
  - Return is over the period's snapshots.
  - Max drawdown is peak to trough over those snapshots.
  - Volatility and Sharpe come from daily returns and are annualized with √365 (crypto trades every day), with a risk-free rate of 0.
  - Win rate is profitable sells over all sells.
  - Turnover is traded notional in KRW over average equity.
- `bars` stores only KRX and US 1-minute bars. Crypto venues have native candles.

## Review Focus

1. **Short histories must not break indicators.** An indicator whose warmup is longer than the history must return an empty or `null` series, never panic or report 0.
2. **Snapshots must survive a missing price.** A position whose book was never received must keep its average-cost value instead of dropping out of equity.
3. **Resampling must align to bucket boundaries.** 1-minute bars with gaps and bars spanning an hour must land in the right buckets, with open, high, low and close correct.
4. **Performance must handle degenerate data.** No snapshots, a single snapshot, zero equity and a period with no fills must each produce zeros or `null`s, never NaN or a division by zero.
5. **Unsupported combinations must fail clearly.** A candle interval or ranking that a venue lacks must return a clear `INVALID_REQUEST` naming what is supported, not `UPSTREAM_ERROR`.

## File Structure

| File | Responsibility |
| --- | --- |
| `src/candles.rs` | `Candle`, `Interval`, `resample`, `BarBuilder` |
| `src/indicators.rs` | `sma`, `ema`, `rsi`, `macd`, `bollinger`, `atr`, `volatility`, `IndicatorSpec::parse`, `compute` |
| `src/screen.rs` | `Ranking`, `ScreenRow`, `rank` (sort helper) |
| `src/performance.rs` | `Snapshot`, `Performance`, `performance(...)` |
| `src/feed/mod.rs` | Modified: `candles` and `screen` trait methods with defaults |
| `src/feed/upbit.rs`, `binance.rs`, `kis/{mod,rest}.rs` | Modified: implementations and parsers |
| `migrations/0002_bars_snapshots.sql` | `bars` and `equity_snapshots` tables |
| `src/store.rs` | Modified: `save_bars`, `bars`, `save_snapshot`, `snapshots` |
| `src/app.rs` | Modified: `value_account` shared valuation; `snapshot_loop`; `bar_loop` |
| `src/market.rs` | Modified: `feed(venue)` accessor |
| `src/tools/{mod,dto}.rs` | Modified: `get_candles`, `get_indicators`, `screen`, `get_performance` |
| `src/cli.rs` | Modified: spawn the bar and snapshot loops |

---

### Task 1: Candles, intervals, resampling and the bar builder

**Files:**
- Create: `src/candles.rs`
- Modify: `src/lib.rs`

**Interfaces:**
- Produces:
  - `Candle { start: DateTime<Utc>, open, high, low, close, volume, value: Decimal }`.
  - `Interval { M1, M5, M15, H1, D1, W1 }` with `parse(&str) -> Option<Interval>`, `code() -> &'static str`, `secs() -> i64` and `is_intraday()`.
  - `resample(&[Candle], Interval) -> Vec<Candle>`. `W1` buckets align to Monday 00:00 UTC.
  - `BarBuilder` with `on_trade(&Trade) -> Option<(InstrumentId, Candle)>` (the bar that just closed) and `flush_before(cutoff) -> Vec<(InstrumentId, Candle)>`.

- [ ] **Step 1: Write the failing tests** (create `src/candles.rs` with the tests; add `pub mod candles;` to `lib.rs`)

```rust
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
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test --lib candles::`
Expected: compile errors.

- [ ] **Step 3: Implement** (prepend to `src/candles.rs`)

```rust
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
```

`flush_before(t(1, 2, 0))` in the test must return the 1:01 bar, whose start 1:01 is before 1:02. It must not return a bar that starts at 1:02. With the one open bar started at 1:01, that holds.

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test --lib candles::`
Expected: 3 passed.

- [ ] **Step 5: Commit**

```bash
git add src/lib.rs src/candles.rs
git commit -m "Add candles, resampling and 1-minute bar builder"
git log -1 --format=%B
```

---

### Task 2: Venue candles and rankings

**Files:**
- Create: `src/screen.rs`
- Modify: `src/feed/mod.rs`, `src/feed/upbit.rs`, `src/feed/binance.rs`, `src/feed/kis/mod.rs`, `src/feed/kis/rest.rs`, `src/lib.rs`

**Interfaces:**
- Produces:
  - `Ranking { Gainers, Losers, Volume, Value }` with `parse` and `code`.
  - `ScreenRow { id: InstrumentId, name: Option<String>, price: Decimal, change_pct: Decimal, volume: Decimal, value: Decimal }`.
  - `rank(rows, ranking, limit) -> Vec<ScreenRow>`.
  - `MarketFeed::candles(&self, id, interval, limit) -> anyhow::Result<Vec<Candle>>` and `MarketFeed::screen(&self, ranking, limit) -> anyhow::Result<Vec<ScreenRow>>`. Their default bodies bail with a message starting `unsupported:`.
  - Parsers:
    - `upbit::{parse_candles_ohlc, parse_tickers}`
    - `binance::{parse_klines_ohlc, parse_tickers}`
    - `kis::rest::{krx_candles, us_candles, krx_rank_rows, us_rank_rows}`

- [ ] **Step 1: Write the failing tests**

Create `src/screen.rs`:

```rust
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
```

Append to the `upbit.rs` tests:

```rust
    #[test]
    fn candles_and_tickers() {
        let body = r#"[{"market":"KRW-BTC","candle_date_time_utc":"2026-09-24T11:45:00","opening_price":2,"high_price":3,"low_price":1,"trade_price":2.5,"candle_acc_trade_price":100,"candle_acc_trade_volume":40},
                      {"market":"KRW-BTC","candle_date_time_utc":"2026-09-24T11:40:00","opening_price":1,"high_price":2,"low_price":1,"trade_price":2,"candle_acc_trade_price":50,"candle_acc_trade_volume":30}]"#;
        let c = parse_candles_ohlc(body.as_bytes()).unwrap();
        assert_eq!(c.len(), 2);
        assert_eq!(c[0].start, Utc.with_ymd_and_hms(2026, 9, 24, 11, 40, 0).unwrap()); // oldest first
        assert_eq!((c[1].close, c[1].value, c[1].volume), (dec!(2.5), dec!(100), dec!(40)));
        let tickers = r#"[{"market":"KRW-XRP","trade_price":2020.0,"signed_change_rate":-0.0198932557,"acc_trade_price_24h":375315639087.83765,"acc_trade_volume_24h":181545431.40572014}]"#;
        let rows = parse_tickers(tickers.as_bytes()).unwrap();
        assert_eq!(rows[0].id.to_string(), "UPBIT:KRW-XRP");
        assert_eq!(rows[0].change_pct, dec!(-1.99));
        assert_eq!(rows[0].value, dec!(375315639087.83765));
    }
```

Append to the `binance.rs` tests:

```rust
    #[test]
    fn klines_and_tickers() {
        let body = r#"[[1790208000000,"84397.6","84622.01","82874.93","83508.08","8469.88",1790294399999,"710303659.28",1,"1","1","0"]]"#;
        let c = parse_klines_ohlc(body.as_bytes()).unwrap();
        assert_eq!((c[0].start.timestamp_millis(), c[0].close, c[0].value), (1790208000000, dec!(83508.08), dec!(710303659.28)));
        let t = r#"[{"symbol":"BTCUSDT","priceChangePercent":"-2.430","lastPrice":"83416.53","volume":"22733.4","quoteVolume":"1915525633.01"},
                   {"symbol":"ETHBTC","priceChangePercent":"1","lastPrice":"0.03","volume":"1","quoteVolume":"1"}]"#;
        let rows = parse_tickers(t.as_bytes()).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!((rows[0].id.to_string(), rows[0].change_pct), ("BINANCE:BTCUSDT".to_string(), dec!(-2.43)));
    }
```

Append to the `kis/rest.rs` tests:

```rust
    #[test]
    fn kis_candles_and_rankings() {
        let body = json!({"output2": [
            {"stck_bsop_date": "20260923", "stck_oprc": "70000", "stck_hgpr": "71000", "stck_lwpr": "69000", "stck_clpr": "70500", "acml_vol": "10", "acml_tr_pbmn": "705000"},
            {"stck_bsop_date": "20260922", "stck_oprc": "69000", "stck_hgpr": "70000", "stck_lwpr": "68000", "stck_clpr": "70000", "acml_vol": "5", "acml_tr_pbmn": "350000"}
        ]});
        let c = krx_candles(&body).unwrap();
        assert_eq!(c[0].start, Utc.with_ymd_and_hms(2026, 9, 21, 15, 0, 0).unwrap()); // 2026-09-22 00:00 KST
        assert_eq!(c[1].close, dec!(70500));
        let us = json!({"output2": [{"xymd": "20260923", "open": "1", "high": "2", "low": "1", "clos": "1.5", "tvol": "10", "tamt": "15"}]});
        assert_eq!(us_candles(&us).unwrap()[0].start, Utc.with_ymd_and_hms(2026, 9, 23, 4, 0, 0).unwrap()); // 00:00 New York (EDT)
        let krx = json!({"output": [{"stck_shrn_iscd": "005930", "hts_kor_isnm": "삼성전자", "stck_prpr": "70000", "prdy_ctrt": "1.5", "acml_vol": "100"}]});
        let rows = krx_rank_rows(&krx);
        assert_eq!((rows[0].id.to_string(), rows[0].name.as_deref(), rows[0].change_pct), ("KRX:005930".to_string(), Some("삼성전자"), dec!(1.5)));
        let krx_vol = json!({"output": [{"mksc_shrn_iscd": "000660", "hts_kor_isnm": "SK하이닉스", "stck_prpr": "1", "prdy_ctrt": "0", "acml_vol": "5", "acml_tr_pbmn": "5"}]});
        assert_eq!(krx_rank_rows(&krx_vol)[0].id.symbol, "000660");
        let usr = json!({"output2": [{"symb": "AAPL", "name": "애플", "last": "187.1", "rate": "-1.2", "tvol": "100", "tamt": "18710"}]});
        assert_eq!(us_rank_rows(&usr)[0].id.to_string(), "US:AAPL");
    }
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test --lib`
Expected: compile errors.

- [ ] **Step 3: Implement**

Create `src/screen.rs` (prepend to the tests) and add `pub mod screen;` to `lib.rs`:

```rust
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
```

In `src/feed/mod.rs`, add `use crate::candles::{Candle, Interval}; use crate::screen::{Ranking, ScreenRow};`, then add these default methods to `MarketFeed`:

```rust
    /// OHLCV candles, oldest first; the last may still be forming.
    async fn candles(&self, _id: &InstrumentId, interval: Interval, _limit: usize) -> anyhow::Result<Vec<Candle>> {
        anyhow::bail!("unsupported: {} has no {} candles", self.venue().tag(), interval.code())
    }

    /// Venue-wide ranking.
    async fn screen(&self, _ranking: Ranking, _limit: usize) -> anyhow::Result<Vec<ScreenRow>> {
        anyhow::bail!("unsupported: {} has no ranking data", self.venue().tag())
    }
```

In `upbit.rs`, add the parsers and trait methods. `parse_candles_ohlc` uses a new `RawOhlc { candle_date_time_utc: String, opening_price, high_price, low_price, trade_price, candle_acc_trade_volume, candle_acc_trade_price }`:

```rust
#[derive(Deserialize)]
struct RawOhlc {
    candle_date_time_utc: String,
    opening_price: Decimal,
    high_price: Decimal,
    low_price: Decimal,
    trade_price: Decimal,
    candle_acc_trade_volume: Decimal,
    candle_acc_trade_price: Decimal,
}

/// Upbit candles arrive newest first.
pub fn parse_candles_ohlc(bytes: &[u8]) -> anyhow::Result<Vec<Candle>> {
    let raw: Vec<RawOhlc> = serde_json::from_slice(bytes)?;
    let mut out = raw
        .into_iter()
        .map(|r| {
            let start = chrono::NaiveDateTime::parse_from_str(&r.candle_date_time_utc, "%Y-%m-%dT%H:%M:%S")?.and_utc();
            Ok(Candle { start, open: r.opening_price, high: r.high_price, low: r.low_price, close: r.trade_price, volume: r.candle_acc_trade_volume, value: r.candle_acc_trade_price })
        })
        .collect::<anyhow::Result<Vec<_>>>()?;
    out.reverse();
    Ok(out)
}

#[derive(Deserialize)]
struct RawTicker {
    market: String,
    trade_price: Decimal,
    signed_change_rate: Decimal,
    acc_trade_price_24h: Decimal,
    acc_trade_volume_24h: Decimal,
}

pub fn parse_tickers(bytes: &[u8]) -> anyhow::Result<Vec<ScreenRow>> {
    let raw: Vec<RawTicker> = serde_json::from_slice(bytes)?;
    Ok(raw
        .into_iter()
        .map(|t| ScreenRow {
            id: id(&t.market),
            name: None,
            price: t.trade_price,
            change_pct: (t.signed_change_rate * Decimal::ONE_HUNDRED).round_dp(2),
            volume: t.acc_trade_volume_24h,
            value: t.acc_trade_price_24h,
        })
        .collect())
}
```

These are the `MarketFeed` methods for `UpbitFeed`:

```rust
    async fn candles(&self, id: &InstrumentId, interval: Interval, limit: usize) -> anyhow::Result<Vec<Candle>> {
        let path = match interval {
            Interval::M1 => "/candles/minutes/1",
            Interval::M5 => "/candles/minutes/5",
            Interval::M15 => "/candles/minutes/15",
            Interval::H1 => "/candles/minutes/60",
            Interval::D1 => "/candles/days",
            Interval::W1 => "/candles/weeks",
        };
        parse_candles_ohlc(&self.get(&format!("{path}?market={}&count={}", id.symbol, limit.min(200))).await?)
    }

    async fn screen(&self, ranking: Ranking, limit: usize) -> anyhow::Result<Vec<ScreenRow>> {
        Ok(rank(parse_tickers(&self.get("/ticker/all?quote_currencies=KRW").await?)?, ranking, limit))
    }
```

In `binance.rs`:

```rust
/// Klines (oldest first): open time, open, high, low, close, volume, close time, quote volume.
pub fn parse_klines_ohlc(bytes: &[u8]) -> anyhow::Result<Vec<Candle>> {
    let rows: Vec<Vec<Value>> = serde_json::from_slice(bytes)?;
    rows.iter()
        .map(|r| {
            let d = |i: usize| -> anyhow::Result<Decimal> {
                r.get(i).and_then(Value::as_str).ok_or_else(|| anyhow!("kline field {i} missing"))?.parse().map_err(Into::into)
            };
            let ms = r.first().and_then(Value::as_i64).ok_or_else(|| anyhow!("kline without open time"))?;
            Ok(Candle {
                start: DateTime::from_timestamp_millis(ms).ok_or_else(|| anyhow!("bad open time {ms}"))?,
                open: d(1)?,
                high: d(2)?,
                low: d(3)?,
                close: d(4)?,
                volume: d(5)?,
                value: d(7)?,
            })
        })
        .collect()
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawTicker {
    symbol: String,
    price_change_percent: Decimal,
    last_price: Decimal,
    volume: Decimal,
    quote_volume: Decimal,
}

/// 24 h tickers, USDT pairs only.
pub fn parse_tickers(bytes: &[u8]) -> anyhow::Result<Vec<ScreenRow>> {
    let raw: Vec<RawTicker> = serde_json::from_slice(bytes)?;
    Ok(raw
        .into_iter()
        .filter(|t| t.symbol.ends_with("USDT"))
        .map(|t| ScreenRow {
            id: id(&t.symbol),
            name: None,
            price: t.last_price,
            change_pct: t.price_change_percent.round_dp(2),
            volume: t.volume,
            value: t.quote_volume,
        })
        .collect())
}
```

These are the `BinanceFeed` methods:

```rust
    async fn candles(&self, id: &InstrumentId, interval: Interval, limit: usize) -> anyhow::Result<Vec<Candle>> {
        parse_klines_ohlc(&self.get(&format!("/klines?symbol={}&interval={}&limit={}", id.symbol, interval.code(), limit.min(200))).await?)
    }

    async fn screen(&self, ranking: Ranking, limit: usize) -> anyhow::Result<Vec<ScreenRow>> {
        Ok(rank(parse_tickers(&self.get("/ticker/24hr").await?)?, ranking, limit))
    }
```

Imports: add `use crate::candles::{Candle, Interval}; use crate::screen::{Ranking, ScreenRow, rank};` to both files. Binance's `parse_klines_ohlc` also needs `chrono::DateTime`, which it already has.

In `kis/rest.rs`:

```rust
fn day_start(tz: chrono_tz::Tz, ymd: &str) -> Option<DateTime<Utc>> {
    use chrono::TimeZone;
    let d = chrono::NaiveDate::parse_from_str(ymd, "%Y%m%d").ok()?;
    tz.from_local_datetime(&d.and_hms_opt(0, 0, 0)?).single().map(|t| t.with_timezone(&Utc))
}

fn candles_from(rows: &Value, tz: chrono_tz::Tz, keys: [&str; 7]) -> anyhow::Result<Vec<Candle>> {
    let [date, o, h, l, c, v, val] = keys;
    let rows = rows.as_array().ok_or_else(|| anyhow!("no chart rows"))?;
    Ok(rows
        .iter()
        .rev()
        .filter_map(|r| {
            Some(Candle {
                start: day_start(tz, r[date].as_str()?)?,
                open: dec(&r[o])?,
                high: dec(&r[h])?,
                low: dec(&r[l])?,
                close: dec(&r[c])?,
                volume: dec(&r[v])?,
                value: dec(&r[val])?,
            })
        })
        .collect())
}

/// `FHKST03010100` rows (newest first) as candles starting 00:00 KST.
pub fn krx_candles(body: &Value) -> anyhow::Result<Vec<Candle>> {
    candles_from(&body["output2"], chrono_tz::Asia::Seoul, ["stck_bsop_date", "stck_oprc", "stck_hgpr", "stck_lwpr", "stck_clpr", "acml_vol", "acml_tr_pbmn"])
}

/// `HHDFS76240000` rows (newest first) as candles starting 00:00 New York time.
pub fn us_candles(body: &Value) -> anyhow::Result<Vec<Candle>> {
    candles_from(&body["output2"], chrono_tz::America::New_York, ["xymd", "open", "high", "low", "clos", "tvol", "tamt"])
}

/// Rows of `FHPST01700000` (fluctuation) or `FHPST01710000` (volume rank).
pub fn krx_rank_rows(body: &Value) -> Vec<ScreenRow> {
    body["output"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|r| {
            let code = r["stck_shrn_iscd"].as_str().or_else(|| r["mksc_shrn_iscd"].as_str())?.trim();
            Some(ScreenRow {
                id: InstrumentId { venue: Venue::Krx, symbol: code.to_string() },
                name: r["hts_kor_isnm"].as_str().map(|s| s.trim().to_string()),
                price: dec(&r["stck_prpr"])?,
                change_pct: dec(&r["prdy_ctrt"]).unwrap_or_default(),
                volume: dec(&r["acml_vol"]).unwrap_or_default(),
                value: dec(&r["acml_tr_pbmn"]).unwrap_or_default(),
            })
        })
        .collect()
}

/// Rows of the US ranking APIs (`output2`).
pub fn us_rank_rows(body: &Value) -> Vec<ScreenRow> {
    body["output2"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|r| {
            Some(ScreenRow {
                id: InstrumentId { venue: Venue::Us, symbol: r["symb"].as_str()?.trim().to_string() },
                name: r["name"].as_str().map(|s| s.trim().to_string()),
                price: dec(&r["last"])?,
                change_pct: dec(&r["rate"]).unwrap_or_default(),
                volume: dec(&r["tvol"]).unwrap_or_default(),
                value: dec(&r["tamt"]).unwrap_or_default(),
            })
        })
        .collect()
}
```

It needs `use crate::candles::Candle; use crate::screen::ScreenRow;`. The existing `krx_daily_stats`/`us_daily_stats` stay as they are.

In `kis/mod.rs`, add these methods for `KisKrxFeed`:

```rust
    async fn candles(&self, id: &InstrumentId, interval: Interval, limit: usize) -> anyhow::Result<Vec<Candle>> {
        let period = match interval {
            Interval::D1 => "D",
            Interval::W1 => "W",
            other => anyhow::bail!("unsupported: KRX {} candles come from stored bars", other.code()),
        };
        let end = self.today();
        let days = if interval == Interval::W1 { 7 * limit as i64 } else { (limit as i64 * 7) / 5 + 10 };
        let start = end - chrono::Duration::days(days);
        let (s, e) = (start.format("%Y%m%d").to_string(), end.format("%Y%m%d").to_string());
        let body = self
            .client
            .get(
                "/uapi/domestic-stock/v1/quotations/inquire-daily-itemchartprice",
                "FHKST03010100",
                &[("FID_COND_MRKT_DIV_CODE", "J"), ("FID_INPUT_ISCD", &id.symbol), ("FID_INPUT_DATE_1", &s), ("FID_INPUT_DATE_2", &e), ("FID_PERIOD_DIV_CODE", period), ("FID_ORG_ADJ_PRC", "0")],
            )
            .await?;
        let mut c = rest::krx_candles(&body)?;
        let skip = c.len().saturating_sub(limit);
        Ok(c.split_off(skip))
    }

    // ponytail: KRX losers sort code "0001" and the US ranking parameters are unverified without KIS keys.
    async fn screen(&self, ranking: Ranking, limit: usize) -> anyhow::Result<Vec<ScreenRow>> {
        let body = match ranking {
            Ranking::Gainers | Ranking::Losers => {
                let sort = if ranking == Ranking::Gainers { "0000" } else { "0001" };
                self.client
                    .get(
                        "/uapi/domestic-stock/v1/ranking/fluctuation",
                        "FHPST01700000",
                        &[
                            ("fid_cond_mrkt_div_code", "J"),
                            ("fid_cond_scr_div_code", "20170"),
                            ("fid_input_iscd", "0000"),
                            ("fid_rank_sort_cls_code", sort),
                            ("fid_input_cnt_1", "0"),
                            ("fid_prc_cls_code", "0"),
                            ("fid_input_price_1", ""),
                            ("fid_input_price_2", ""),
                            ("fid_vol_cnt", ""),
                            ("fid_trgt_cls_code", "0"),
                            ("fid_trgt_exls_cls_code", "0"),
                            ("fid_div_cls_code", "0"),
                            ("fid_rsfl_rate1", ""),
                            ("fid_rsfl_rate2", ""),
                        ],
                    )
                    .await?
            }
            Ranking::Volume | Ranking::Value => {
                let by = if ranking == Ranking::Volume { "0" } else { "3" };
                self.client
                    .get(
                        "/uapi/domestic-stock/v1/quotations/volume-rank",
                        "FHPST01710000",
                        &[
                            ("FID_COND_MRKT_DIV_CODE", "J"),
                            ("FID_COND_SCR_DIV_CODE", "20171"),
                            ("FID_INPUT_ISCD", "0000"),
                            ("FID_DIV_CLS_CODE", "0"),
                            ("FID_BLNG_CLS_CODE", by),
                            ("FID_TRGT_CLS_CODE", "111111111"),
                            ("FID_TRGT_EXLS_CLS_CODE", "0000000000"),
                            ("FID_INPUT_PRICE_1", ""),
                            ("FID_INPUT_PRICE_2", ""),
                            ("FID_VOL_CNT", ""),
                            ("FID_INPUT_DATE_1", ""),
                        ],
                    )
                    .await?
            }
        };
        Ok(crate::screen::rank(rest::krx_rank_rows(&body), ranking, limit))
    }
```

These are the methods for `KisUsFeed`:

```rust
    async fn candles(&self, id: &InstrumentId, interval: Interval, limit: usize) -> anyhow::Result<Vec<Candle>> {
        let gubn = match interval {
            Interval::D1 => "0",
            Interval::W1 => "1",
            other => anyhow::bail!("unsupported: US {} candles come from stored bars", other.code()),
        };
        let excd = self.excd(id)?;
        let body = self
            .client
            .get("/uapi/overseas-price/v1/quotations/dailyprice", "HHDFS76240000", &[("AUTH", ""), ("EXCD", &excd), ("SYMB", &id.symbol), ("GUBN", gubn), ("BYMD", ""), ("MODP", "1")])
            .await?;
        let mut c = rest::us_candles(&body)?;
        let skip = c.len().saturating_sub(limit);
        Ok(c.split_off(skip))
    }

    async fn screen(&self, ranking: Ranking, limit: usize) -> anyhow::Result<Vec<ScreenRow>> {
        let (path, tr_id, extra): (&str, &str, &[(&str, &str)]) = match ranking {
            Ranking::Gainers => ("/uapi/overseas-stock/v1/ranking/updown-rate", "HHDFS76290000", &[("GUBN", "1")]),
            Ranking::Losers => ("/uapi/overseas-stock/v1/ranking/updown-rate", "HHDFS76290000", &[("GUBN", "0")]),
            Ranking::Volume => ("/uapi/overseas-stock/v1/ranking/trade-vol", "HHDFS76310010", &[("PRC1", ""), ("PRC2", "")]),
            Ranking::Value => ("/uapi/overseas-stock/v1/ranking/trade-pbmn", "HHDFS76320010", &[("PRC1", ""), ("PRC2", "")]),
        };
        let mut rows = Vec::new();
        for excd in ["NAS", "NYS"] {
            let mut q: Vec<(&str, &str)> = vec![("EXCD", excd), ("NDAY", "0"), ("VOL_RANG", "0"), ("AUTH", ""), ("KEYB", "")];
            q.extend_from_slice(extra);
            rows.extend(rest::us_rank_rows(&self.client.get(path, tr_id, &q).await?));
        }
        Ok(crate::screen::rank(rows, ranking, limit))
    }
```

Add `use crate::candles::{Candle, Interval}; use crate::screen::{Ranking, ScreenRow};` to `kis/mod.rs`.

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test --lib`
Expected: all pass (4 new parser tests + the screen test).

- [ ] **Step 5: Live check**

Add this to `tests/live.rs`:

```rust
#[tokio::test]
#[ignore]
async fn crypto_candles_and_screens_live() {
    use atrader::candles::Interval;
    use atrader::screen::Ranking;
    let upbit = UpbitFeed::new(Arc::new(SystemClock));
    let c = upbit.candles(&"UPBIT:KRW-BTC".parse().unwrap(), Interval::M5, 10).await.unwrap();
    assert_eq!(c.len(), 10);
    assert!(c[0].start < c[9].start);
    assert_eq!(upbit.screen(Ranking::Value, 5).await.unwrap().len(), 5);
    let binance = BinanceFeed::new(Arc::new(SystemClock));
    assert_eq!(binance.candles(&"BINANCE:BTCUSDT".parse().unwrap(), Interval::D1, 3).await.unwrap().len(), 3);
    assert_eq!(binance.screen(Ranking::Gainers, 5).await.unwrap().len(), 5);
}
```

Run: `cargo test --test live -- --ignored`
Expected: all pass.

- [ ] **Step 6: Commit**

```bash
git add src tests/live.rs
git commit -m "Add venue candles and rankings"
git log -1 --format=%B
```

---

### Task 3: Indicators

**Files:**
- Create: `src/indicators.rs`
- Modify: `src/lib.rs`

**Interfaces:**
- Produces:
  - Series functions that return `Vec<Option<f64>>` aligned to their input:
    - `sma(&[f64], n)`, `ema(&[f64], n)`, `rsi(&[f64], n)`
    - `macd(&[f64], fast, slow, signal) -> [Vec<Option<f64>>; 3]` (macd, signal, hist)
    - `bollinger(&[f64], n, k) -> [Vec<Option<f64>>; 3]` (upper, middle, lower)
    - `atr(high, low, close, n)`
    - `volatility(&[f64], n)`, which is the stdev of log returns in percent
  - `IndicatorSpec::parse(&str) -> Result<IndicatorSpec, String>` and `compute(&IndicatorSpec, &[Candle]) -> Vec<(String, Vec<Option<f64>>)>`. Each result is a named line such as `("sma_20", …)` or `("macd", …), ("macd_signal", …), ("macd_hist", …)`.

- [ ] **Step 1: Write the failing tests**

```rust
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
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test --lib indicators::`
Expected: compile errors.

- [ ] **Step 3: Implement** (prepend; add `pub mod indicators;` to `lib.rs`)

```rust
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
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test --lib indicators::`
Expected: 5 passed.

- [ ] **Step 5: Commit**

```bash
git add src/lib.rs src/indicators.rs
git commit -m "Add technical indicators"
git log -1 --format=%B
```

---

### Task 4: Stored bars, equity snapshots and performance

**Files:**
- Create: `migrations/0002_bars_snapshots.sql`, `src/performance.rs`
- Modify: `src/store.rs`, `src/app.rs`, `src/tools/mod.rs` (use the shared valuation), `src/cli.rs`, `src/lib.rs`, `tests/store.rs`

**Interfaces:**
- Produces:
  - `Store` methods:
    - `save_bars(&[(InstrumentId, Candle)])`
    - `bars(&InstrumentId, since: DateTime<Utc>) -> Vec<Candle>` (oldest first)
    - `save_snapshot(&Snapshot)`
    - `snapshots(account, generation, since: Option<DateTime<Utc>>) -> Vec<Snapshot>` (oldest first)
  - `Snapshot { account, generation, at, kind: SnapshotKind { Minute, Daily }, equity_krw, cash_krw, positions_krw }`.
  - `performance(snapshots: &[Snapshot], fills: &[Fill], usd_krw) -> Performance`.
  - `Performance { start_equity_krw, end_equity_krw, return_pct, max_drawdown_pct, volatility_pct: Option<f64>, sharpe: Option<f64>, trades, sells, win_rate_pct: Option<Decimal>, realized_pnl_krw, fees_krw, turnover: Option<Decimal> }`.
  - `app::value_account(&SimBroker, &Portfolio, usd_krw) -> Valuation { cash_krw, positions_krw, equity_krw, lines: Vec<PositionLine> }`.
  - `PositionLine { id, qty, avg_cost, price: Option<Decimal>, value, value_krw }`.
  - `app::snapshot_loop(App, generations)` and `app::bar_loop(bus rx, store)`, both async.

- [ ] **Step 1: Write the failing tests**

Create `src/performance.rs`:

```rust
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
```

Append to `tests/store.rs`:

```rust
#[sqlx::test]
async fn bars_and_snapshots_round_trip(pool: PgPool) {
    use atrader::candles::Candle;
    use atrader::performance::{Snapshot, SnapshotKind};
    use chrono::TimeZone;
    let store = Store::new(pool);
    let id: InstrumentId = "KRX:005930".parse().unwrap();
    let at = |m| Utc.with_ymd_and_hms(2026, 9, 23, 1, m, 0).unwrap();
    let c = |m, p| Candle { start: at(m), open: p, high: p, low: p, close: p, volume: dec!(1), value: p };
    store.save_bars(&[(id.clone(), c(1, dec!(100))), (id.clone(), c(0, dec!(99)))]).await.unwrap();
    store.save_bars(&[(id.clone(), c(1, dec!(101)))]).await.unwrap(); // upsert
    let bars = store.bars(&id, at(0)).await.unwrap();
    assert_eq!(bars.iter().map(|b| b.close).collect::<Vec<_>>(), vec![dec!(99), dec!(101)]);

    store.create_account("a", "Test", None, &[(Currency::Krw, dec!(1))], Utc::now()).await.unwrap();
    let s = Snapshot { account: "a".into(), generation: 1, at: at(5), kind: SnapshotKind::Daily, equity_krw: dec!(10), cash_krw: dec!(4), positions_krw: dec!(6) };
    store.save_snapshot(&s).await.unwrap();
    assert_eq!(store.snapshots("a", 1, None).await.unwrap(), vec![s.clone()]);
    assert!(store.snapshots("a", 1, Some(at(6))).await.unwrap().is_empty());
}
```

Add a valuation test to the `app.rs` tests module:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::{Book, Level, ManualClock};
    use crate::venue::{Calendar, Instrument, LotRule, TickRule};
    use chrono::{TimeZone, Utc};
    use rust_decimal_macros::dec;

    #[test]
    fn positions_without_a_book_keep_their_cost_value() {
        let clock = ManualClock::new(Utc.with_ymd_and_hms(2026, 9, 23, 1, 0, 0).unwrap());
        let broker = SimBroker::new(std::sync::Arc::new(clock.clone()), Calendar::default());
        let btc: crate::domain::InstrumentId = "UPBIT:KRW-BTC".parse().unwrap();
        let eth: crate::domain::InstrumentId = "BINANCE:ETHUSDT".parse().unwrap();
        broker.add_instrument(Instrument {
            id: btc.clone(),
            name: "BTC".into(),
            tick: TickRule::Fixed(dec!(1000)),
            lot: LotRule { step: dec!(0.00000001), min_qty: dec!(0.00000001), min_notional: dec!(0) },
            tradable: true,
        });
        broker.on_book(Book {
            instrument: btc.clone(),
            bids: vec![Level { price: dec!(99000), qty: dec!(1) }],
            asks: vec![Level { price: dec!(101000), qty: dec!(1) }],
            prev_close: None,
            received_at: clock.now(),
        });
        let mut pf = crate::ledger::Portfolio::new(&[(Currency::Krw, dec!(1000)), (Currency::Usdt, dec!(10))]);
        pf.apply_fill(&btc, crate::domain::Side::Buy, dec!(2), dec!(180000), dec!(0), dec!(0));
        pf.apply_fill(&eth, crate::domain::Side::Buy, dec!(1), dec!(5), dec!(0), dec!(0));
        let v = value_account(&broker, &pf, dec!(1400));
        // Cash: 1000 - 180000 KRW + 5 USDT * 1400. BTC at mid 100000 * 2; ETH has no book: cost 5 USDT.
        assert_eq!(v.cash_krw, dec!(1000) - dec!(180000) + dec!(7000));
        assert_eq!(v.positions_krw, dec!(200000) + dec!(7000));
        assert_eq!(v.equity_krw, v.cash_krw + v.positions_krw);
        assert_eq!(v.lines.len(), 2);
    }
}
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test` (with `DATABASE_URL`)
Expected: compile errors.

- [ ] **Step 3: Implement**

Create `migrations/0002_bars_snapshots.sql`:

```sql
-- 1-minute bars built from KRX and US trade prints (crypto venues serve their own candles).
CREATE TABLE bars (
    instrument TEXT NOT NULL,
    start      TIMESTAMPTZ NOT NULL,
    open       NUMERIC NOT NULL,
    high       NUMERIC NOT NULL,
    low        NUMERIC NOT NULL,
    close      NUMERIC NOT NULL,
    volume     NUMERIC NOT NULL,
    value      NUMERIC NOT NULL,
    PRIMARY KEY (instrument, start)
);

CREATE TABLE equity_snapshots (
    account_id    TEXT NOT NULL REFERENCES accounts(id),
    generation    INT  NOT NULL,
    at            TIMESTAMPTZ NOT NULL,
    kind          TEXT NOT NULL CHECK (kind IN ('minute', 'daily')),
    equity_krw    NUMERIC NOT NULL,
    cash_krw      NUMERIC NOT NULL,
    positions_krw NUMERIC NOT NULL,
    PRIMARY KEY (account_id, generation, kind, at)
);
```

Prepend this to `src/performance.rs` and add `pub mod performance;` to `lib.rs`:

```rust
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
```

In `src/store.rs`, add the imports `use crate::candles::Candle; use crate::performance::{Snapshot, SnapshotKind};` and these methods:

```rust
    pub async fn save_bars(&self, bars: &[(InstrumentId, Candle)]) -> sqlx::Result<()> {
        let mut tx = self.pool.begin().await?;
        for (id, c) in bars {
            sqlx::query(
                "INSERT INTO bars (instrument, start, open, high, low, close, volume, value) VALUES ($1,$2,$3,$4,$5,$6,$7,$8)
                 ON CONFLICT (instrument, start) DO UPDATE SET open = EXCLUDED.open, high = EXCLUDED.high, low = EXCLUDED.low,
                     close = EXCLUDED.close, volume = EXCLUDED.volume, value = EXCLUDED.value",
            )
            .bind(id.to_string())
            .bind(c.start)
            .bind(c.open)
            .bind(c.high)
            .bind(c.low)
            .bind(c.close)
            .bind(c.volume)
            .bind(c.value)
            .execute(&mut *tx)
            .await?;
        }
        tx.commit().await
    }

    /// 1-minute bars from `since`, oldest first.
    pub async fn bars(&self, id: &InstrumentId, since: DateTime<Utc>) -> sqlx::Result<Vec<Candle>> {
        let rows = sqlx::query("SELECT * FROM bars WHERE instrument = $1 AND start >= $2 ORDER BY start")
            .bind(id.to_string())
            .bind(since)
            .fetch_all(&self.pool)
            .await?;
        Ok(rows
            .iter()
            .map(|r| Candle {
                start: r.get("start"),
                open: r.get("open"),
                high: r.get("high"),
                low: r.get("low"),
                close: r.get("close"),
                volume: r.get("volume"),
                value: r.get("value"),
            })
            .collect())
    }

    pub async fn save_snapshot(&self, s: &Snapshot) -> sqlx::Result<()> {
        sqlx::query(
            "INSERT INTO equity_snapshots (account_id, generation, at, kind, equity_krw, cash_krw, positions_krw)
             VALUES ($1,$2,$3,$4,$5,$6,$7) ON CONFLICT DO NOTHING",
        )
        .bind(&s.account)
        .bind(s.generation)
        .bind(s.at)
        .bind(if s.kind == SnapshotKind::Daily { "daily" } else { "minute" })
        .bind(s.equity_krw)
        .bind(s.cash_krw)
        .bind(s.positions_krw)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// Oldest first; `since` inclusive.
    pub async fn snapshots(&self, account: &str, generation: i32, since: Option<DateTime<Utc>>) -> sqlx::Result<Vec<Snapshot>> {
        let rows = sqlx::query(
            "SELECT * FROM equity_snapshots WHERE account_id = $1 AND generation = $2 AND ($3::timestamptz IS NULL OR at >= $3)
             ORDER BY at, kind",
        )
        .bind(account)
        .bind(generation)
        .bind(since)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows
            .iter()
            .map(|r| Snapshot {
                account: r.get("account_id"),
                generation: r.get("generation"),
                at: r.get("at"),
                kind: if r.get::<String, _>("kind") == "daily" { SnapshotKind::Daily } else { SnapshotKind::Minute },
                equity_krw: r.get("equity_krw"),
                cash_krw: r.get("cash_krw"),
                positions_krw: r.get("positions_krw"),
            })
            .collect())
    }
```

In `src/app.rs`, add the shared valuation and the two loops:

```rust
#[derive(Debug, Clone, PartialEq)]
pub struct PositionLine {
    pub id: crate::domain::InstrumentId,
    pub qty: Decimal,
    pub avg_cost: Decimal,
    /// Shadow mid; `None` without a book (valued at cost then).
    pub price: Option<Decimal>,
    /// In the instrument's currency.
    pub value: Decimal,
    pub value_krw: Decimal,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Valuation {
    pub cash_krw: Decimal,
    pub positions_krw: Decimal,
    pub equity_krw: Decimal,
    pub lines: Vec<PositionLine>,
}

/// Value a portfolio at the shadow mids already in memory.
pub fn value_account(broker: &SimBroker, pf: &crate::ledger::Portfolio, usd_krw: Decimal) -> Valuation {
    let cash_krw: Decimal = pf.cash.iter().map(|(c, v)| *v * krw_per(*c, usd_krw)).sum();
    let lines: Vec<PositionLine> = pf
        .positions
        .iter()
        .filter(|(_, p)| !p.qty.is_zero())
        .map(|(id, p)| {
            let price = broker.book_view(id, 1).and_then(|v| Some((v.shadow_bids.first()?.price + v.shadow_asks.first()?.price) / Decimal::TWO));
            let cur = id.venue.currency();
            let value = (p.qty * price.unwrap_or(p.avg_cost)).round_dp(cur.decimals());
            PositionLine { id: id.clone(), qty: p.qty, avg_cost: p.avg_cost, price, value, value_krw: value * krw_per(cur, usd_krw) }
        })
        .collect();
    let positions_krw: Decimal = lines.iter().map(|l| l.value_krw).sum();
    Valuation { cash_krw, positions_krw, equity_krw: cash_krw + positions_krw, lines }
}

/// Every minute, record each account's equity; at the first tick of a KST day, also record a
/// daily close stamped 00:00 KST.
pub async fn snapshot_loop(app: Arc<App>, generations: HashMap<String, i32>) {
    use crate::performance::{Snapshot, SnapshotKind};
    use chrono::TimeZone;
    let mut last_day = None;
    let mut tick = tokio::time::interval(std::time::Duration::from_secs(60));
    loop {
        tick.tick().await;
        let Ok(usd_krw) = app.fx.usd_krw().await else { continue };
        let now = app.broker.now();
        let day = now.with_timezone(&chrono_tz::Asia::Seoul).date_naive();
        let daily_at = (last_day != Some(day))
            .then(|| chrono_tz::Asia::Seoul.from_local_datetime(&day.and_hms_opt(0, 0, 0).expect("midnight")).single())
            .flatten()
            .map(|t| t.with_timezone(&chrono::Utc));
        for (account, generation) in &generations {
            let Some(pf) = app.broker.portfolio(account) else { continue };
            let v = value_account(&app.broker, &pf, usd_krw);
            let mut snaps = vec![(now, SnapshotKind::Minute)];
            snaps.extend(daily_at.map(|at| (at, SnapshotKind::Daily)));
            for (at, kind) in snaps {
                let s = Snapshot {
                    account: account.clone(),
                    generation: *generation,
                    at,
                    kind,
                    equity_krw: v.equity_krw.round_dp(0),
                    cash_krw: v.cash_krw.round_dp(0),
                    positions_krw: v.positions_krw.round_dp(0),
                };
                if let Err(e) = app.store.save_snapshot(&s).await {
                    tracing::warn!(error = %e, account, "could not save equity snapshot");
                }
            }
        }
        last_day = Some(day);
    }
}

/// Roll KRX and US trade prints from the bus into stored 1-minute bars.
pub async fn bar_loop(mut rx: tokio::sync::broadcast::Receiver<crate::market::BusEvent>, store: Arc<Store>) {
    use crate::market::BusEvent;
    use tokio::sync::broadcast::error::RecvError;
    let mut builder = crate::candles::BarBuilder::default();
    let mut flush = tokio::time::interval(std::time::Duration::from_secs(10));
    loop {
        let done = tokio::select! {
            ev = rx.recv() => match ev {
                Ok(BusEvent::Market(crate::feed::MarketEvent::Trade(t))) if t.instrument.venue.has_session() => {
                    builder.on_trade(&t).into_iter().collect::<Vec<_>>()
                }
                Ok(_) => continue,
                Err(RecvError::Lagged(n)) => {
                    tracing::warn!(skipped = n, "bar builder fell behind the bus");
                    continue;
                }
                Err(RecvError::Closed) => return,
            },
            _ = flush.tick() => builder.flush_before(chrono::Utc::now() - chrono::Duration::seconds(60)),
        };
        if !done.is_empty() {
            if let Err(e) = store.save_bars(&done).await {
                tracing::warn!(error = %e, "could not save bars");
            }
        }
    }
}
```

`app.rs` also needs `use std::collections::HashMap;`, which it already has.

In `src/tools/mod.rs` `valuation`, replace the per-position price and value computation with a call to `crate::app::value_account(&self.app.broker, &pf, usd_krw)`. Keep the `ensure_fresh` loop over `pf.positions` before the call. Build each `PositionView` from a `PositionLine`: `unrealized_pnl = value - qty*avg_cost` and the percentage as before. Take `cash` and `equity` from the `Valuation`. The existing tools tests pin that behaviour and must stay green.

In `src/cli.rs` `serve`, after spawning `persist`, spawn the loops. `generations` was moved into `persist`, so clone it first (`let writer = tokio::spawn(persist(journal_rx, store.clone(), bus.clone(), generations.clone()));`):

```rust
    tokio::spawn(crate::app::bar_loop(bus.subscribe(), store.clone()));
```

Once `app` exists:

```rust
    tokio::spawn(crate::app::snapshot_loop(app.clone(), generations));
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test` (with `DATABASE_URL`)
Expected: all pass, including the new performance, store and app tests.

- [ ] **Step 5: Commit**

```bash
git add migrations src tests/store.rs
git commit -m "Store bars and equity snapshots; compute account performance"
git log -1 --format=%B
```

---

### Task 5: Research tools

**Files:**
- Modify: `src/market.rs` (`feed` accessor), `src/tools/mod.rs`, `src/tools/dto.rs`, `tests/tools.rs`

**Interfaces:**
- Consumes: Tasks 1–4.
- Produces:
  - `Market::feed(Venue) -> Option<Arc<dyn MarketFeed>>`.
  - Tools:
    - `get_candles(id, interval, limit)`
    - `get_indicators(id, interval, indicators: Vec<String>, points: Option<u32>)`
    - `screen(venue, ranking, limit)`
    - `get_performance(account, period)`
  - DTOs: `CandleView`, `IndicatorLine { name, values: Vec<IndicatorValue { at, value: Option<f64> }> }`, `ScreenRowView`, `PerformanceView`.

- [ ] **Step 1: Write the failing tests** (append to `tests/tools.rs`)

The fake feed gains candles and a ranking. Add these methods to `impl MarketFeed for Fake`:

```rust
    async fn candles(&self, _: &InstrumentId, interval: atrader::candles::Interval, limit: usize) -> anyhow::Result<Vec<atrader::candles::Candle>> {
        let step = chrono::Duration::seconds(interval.secs());
        let start = self.clock.now() - step * limit as i32;
        Ok((0..limit)
            .map(|i| {
                let p = Decimal::from(100 + i as i64);
                atrader::candles::Candle { start: start + step * i as i32, open: p, high: p + dec!(1), low: p - dec!(1), close: p, volume: dec!(1), value: p }
            })
            .collect())
    }
    async fn screen(&self, ranking: atrader::screen::Ranking, limit: usize) -> anyhow::Result<Vec<atrader::screen::ScreenRow>> {
        let row = atrader::screen::ScreenRow {
            id: "UPBIT:KRW-BTC".parse().unwrap(),
            name: None,
            price: dec!(100000000),
            change_pct: dec!(1.5),
            volume: dec!(10),
            value: dec!(1000000000),
        };
        Ok(atrader::screen::rank(vec![row], ranking, limit))
    }
```

Then add the tests:

```rust
#[sqlx::test]
async fn research_tools(pool: PgPool) {
    let (app, t) = rig(pool).await;
    let c = t.get_candles("UPBIT:KRW-BTC".into(), "5m".into(), Some(30)).await.unwrap();
    assert_eq!(c.len(), 30);
    assert!(c[0].start < c[29].start);
    assert_eq!(code(&t.get_candles("UPBIT:KRW-BTC".into(), "2h".into(), None).await.unwrap_err()), "InvalidParams");

    let ind = t.get_indicators("UPBIT:KRW-BTC".into(), "1d".into(), vec!["sma:5".into(), "macd".into()], Some(3)).await.unwrap();
    assert_eq!(ind.iter().map(|l| l.name.as_str()).collect::<Vec<_>>(), vec!["sma_5", "macd", "macd_signal", "macd_hist"]);
    assert_eq!(ind[0].values.len(), 3);
    assert!(ind[0].values[2].value.is_some());
    assert_eq!(code(&t.get_indicators("UPBIT:KRW-BTC".into(), "1d".into(), vec!["magic".into()], None).await.unwrap_err()), "InvalidParams");

    let rows = t.screen("UPBIT".into(), "gainers".into(), Some(5)).await.unwrap();
    assert_eq!(rows[0].id, "UPBIT:KRW-BTC");
    assert_eq!(rows[0].name, "비트코인 (Bitcoin)"); // filled from the instrument list
    assert_eq!(code(&t.screen("KRX".into(), "gainers".into(), None).await.unwrap_err()), "INVALID_REQUEST"); // no KRX feed here

    t.place_order(buy("bot", Some(dec!(0.1)), "entry")).await.unwrap();
    let p = t.get_performance("bot".into(), "all".into()).await.unwrap();
    assert!(p.trades <= 1); // the journal may not have landed yet; no panic either way
    assert_eq!(code(&t.get_performance("bot".into(), "1y".into()).await.unwrap_err()), "InvalidParams");
    let _ = app;
}
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test --test tools research`
Expected: compile errors (`get_candles` not found).

- [ ] **Step 3: Implement**

Add this to `Market`:

```rust
    pub fn feed(&self, venue: Venue) -> Option<Arc<dyn MarketFeed>> {
        self.venues.get(&venue).map(|v| v.feed.clone())
    }
```

Append these DTOs to `src/tools/dto.rs`:

```rust
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct CandleView {
    /// Bucket start.
    pub start: DateTime<Utc>,
    pub open: Decimal,
    pub high: Decimal,
    pub low: Decimal,
    pub close: Decimal,
    /// Shares or coins.
    pub volume: Decimal,
    /// Traded value, quote currency.
    pub value: Decimal,
}

impl From<&crate::candles::Candle> for CandleView {
    fn from(c: &crate::candles::Candle) -> Self {
        CandleView { start: c.start, open: c.open, high: c.high, low: c.low, close: c.close, volume: c.volume, value: c.value }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct IndicatorValue {
    /// Start of the candle this value belongs to.
    pub at: DateTime<Utc>,
    /// Absent while the indicator lacks history.
    pub value: Option<f64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct IndicatorLine {
    /// e.g. `sma_20`, `rsi_14`, `macd`, `macd_signal`, `macd_hist`, `bb_upper`, `atr_14`, `vol_20`.
    pub name: String,
    /// Oldest first; the last is the latest.
    pub values: Vec<IndicatorValue>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct ScreenRowView {
    pub id: String,
    pub name: String,
    pub price: Decimal,
    /// Versus the previous close (24 h for crypto), percent.
    pub change_pct: Decimal,
    pub volume: Decimal,
    /// Traded value, quote currency.
    pub value: Decimal,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct PerformanceView {
    pub period: String,
    pub start_equity_krw: Decimal,
    pub end_equity_krw: Decimal,
    pub return_pct: Decimal,
    /// Largest peak-to-trough fall of equity within the period, percent.
    pub max_drawdown_pct: Decimal,
    /// Annualized volatility of daily returns, percent; needs a few days of history.
    pub volatility_pct: Option<f64>,
    pub sharpe: Option<f64>,
    pub trades: usize,
    pub sells: usize,
    /// Share of sells with positive realized profit, percent.
    pub win_rate_pct: Option<Decimal>,
    pub realized_pnl_krw: Decimal,
    pub fees_krw: Decimal,
    /// Traded value over average equity.
    pub turnover: Option<Decimal>,
}
```

Add the four methods to the `Trader` trait:

```rust
    /// OHLCV candles, oldest first (the last may still be forming). `interval`: 1m, 5m, 15m, 1h,
    /// 1d, 1w. `limit` default 100, at most 200. KRX/US minute candles exist only for periods
    /// when ATrader was streaming that stock.
    async fn get_candles(&self, id: String, interval: String, limit: Option<u32>) -> zyris::Result<Vec<CandleView>>;

    /// Technical indicators computed on the server from candles. `indicators`: up to 8 of
    /// `sma:N`, `ema:N`, `rsi[:N]`, `macd[:fast:slow:signal]`, `bb[:N:width]`, `atr[:N]`,
    /// `vol[:N]` (stdev of log returns per bar, %). `points`: latest values per line, default
    /// 1, at most 100.
    async fn get_indicators(&self, id: String, interval: String, indicators: Vec<String>, points: Option<u32>) -> zyris::Result<Vec<IndicatorLine>>;

    /// Venue ranking to find candidates. `venue`: KRX, US, UPBIT or BINANCE. `ranking`:
    /// gainers, losers, volume or value (traded value). `limit` default 20, at most 50.
    async fn screen(&self, venue: String, ranking: String, limit: Option<u32>) -> zyris::Result<Vec<ScreenRowView>>;

    /// Account performance over `period`: 1d, 1w, 1m, 3m or all — return, max drawdown,
    /// volatility, Sharpe, win rate, realized profit, fees and turnover (KRW).
    async fn get_performance(&self, account: String, period: String) -> zyris::Result<PerformanceView>;
```

Add the implementations to `impl Trader for TraderTools`, with the needed imports: `crate::candles::{Candle, Interval, resample}`, `crate::indicators::{IndicatorSpec, compute}`, `crate::screen::Ranking` and `crate::performance::performance`:

```rust
    async fn get_candles(&self, id: String, interval: String, limit: Option<u32>) -> zyris::Result<Vec<CandleView>> {
        let id = self.known(&id)?;
        let interval = Interval::parse(&interval).ok_or_else(|| bad("interval must be one of 1m, 5m, 15m, 1h, 1d, 1w"))?;
        let limit = limit.unwrap_or(100).clamp(1, 200) as usize;
        Ok(self.candles(&id, interval, limit).await?.iter().map(CandleView::from).collect())
    }

    async fn get_indicators(&self, id: String, interval: String, indicators: Vec<String>, points: Option<u32>) -> zyris::Result<Vec<IndicatorLine>> {
        let id = self.known(&id)?;
        let interval = Interval::parse(&interval).ok_or_else(|| bad("interval must be one of 1m, 5m, 15m, 1h, 1d, 1w"))?;
        if indicators.is_empty() || indicators.len() > 8 {
            return Err(bad("ask for 1 to 8 indicators"));
        }
        let specs = indicators.iter().map(|s| IndicatorSpec::parse(s)).collect::<Result<Vec<_>, _>>().map_err(bad)?;
        let points = points.unwrap_or(1).clamp(1, 100) as usize;
        let candles = self.candles(&id, interval, 200).await?;
        let mut out = Vec::new();
        for spec in &specs {
            for (name, series) in compute(spec, &candles) {
                let skip = series.len().saturating_sub(points);
                let values = candles.iter().zip(series).skip(skip).map(|(c, v)| IndicatorValue { at: c.start, value: v.filter(|x| x.is_finite()) }).collect();
                out.push(IndicatorLine { name, values });
            }
        }
        Ok(out)
    }

    async fn screen(&self, venue: String, ranking: String, limit: Option<u32>) -> zyris::Result<Vec<ScreenRowView>> {
        let venue = parse_venue(&venue)?;
        let ranking = Ranking::parse(&ranking).ok_or_else(|| bad("ranking must be gainers, losers, volume or value"))?;
        let limit = limit.unwrap_or(20).clamp(1, 50) as usize;
        let feed = self
            .app
            .market
            .feed(venue)
            .ok_or_else(|| order_error(OrderError::InvalidRequest(format!("{} market data is not enabled", venue.tag()))))?;
        let rows = feed.screen(ranking, limit).await.map_err(feed_error)?;
        Ok(rows
            .into_iter()
            .map(|r| {
                let name = r.name.clone().or_else(|| self.app.broker.instrument(&r.id).map(|i| i.name)).unwrap_or_default();
                ScreenRowView { id: r.id.to_string(), name, price: r.price, change_pct: r.change_pct, volume: r.volume, value: r.value }
            })
            .collect())
    }

    async fn get_performance(&self, account: String, period: String) -> zyris::Result<PerformanceView> {
        let row = self.app.agent_account(&account)?;
        let days = match period.as_str() {
            "1d" => Some(1),
            "1w" => Some(7),
            "1m" => Some(30),
            "3m" => Some(90),
            "all" => None,
            _ => return Err(bad("period must be 1d, 1w, 1m, 3m or all")),
        };
        let since = days.map(|d| self.app.broker.now() - chrono::Duration::days(d));
        let snaps = self.app.store.snapshots(&row.id, row.generation, since).await.map_err(upstream)?;
        let fills = self.app.store.fills(&row.id, row.generation, since, 100_000).await.map_err(upstream)?;
        let usd_krw = self.app.fx.usd_krw().await.map_err(|e| upstream(format!("{e:#}")))?;
        let p = performance(&snaps, &fills, usd_krw);
        Ok(PerformanceView {
            period,
            start_equity_krw: p.start_equity_krw,
            end_equity_krw: p.end_equity_krw,
            return_pct: p.return_pct,
            max_drawdown_pct: p.max_drawdown_pct,
            volatility_pct: p.volatility_pct.map(|v| (v * 100.0).round() / 100.0),
            sharpe: p.sharpe.map(|v| (v * 100.0).round() / 100.0),
            trades: p.trades,
            sells: p.sells,
            win_rate_pct: p.win_rate_pct,
            realized_pnl_krw: p.realized_pnl_krw,
            fees_krw: p.fees_krw,
            turnover: p.turnover,
        })
    }
```

Add these helpers. `feed_error` sends "unsupported:" to `INVALID_REQUEST` and everything else to `UPSTREAM_ERROR`. `candles` routes KRX/US minute intervals to stored bars:

```rust
fn feed_error(e: anyhow::Error) -> zyris::Error {
    let msg = format!("{e:#}");
    match msg.strip_prefix("unsupported: ") {
        Some(rest) => order_error(OrderError::InvalidRequest(rest.to_string())),
        None => upstream(msg),
    }
}
```

The `candles` helper goes in `impl TraderTools`:

```rust
    async fn candles(&self, id: &InstrumentId, interval: Interval, limit: usize) -> zyris::Result<Vec<Candle>> {
        if interval.is_intraday() && id.venue.has_session() {
            let since = self.app.broker.now() - chrono::Duration::seconds(interval.secs() * limit as i64 * 3);
            let bars = self.app.store.bars(id, since).await.map_err(upstream)?;
            let mut out = resample(&bars, interval);
            let skip = out.len().saturating_sub(limit);
            return Ok(out.split_off(skip));
        }
        let feed = self
            .app
            .market
            .feed(id.venue)
            .ok_or_else(|| order_error(OrderError::InvalidRequest(format!("{} market data is not enabled", id.venue.tag()))))?;
        feed.candles(id, interval, limit).await.map_err(feed_error)
    }
```

(Minute bars cover roughly 3× the requested span, which absorbs closed hours. The ceiling: after a long weekend, a request for many 1m candles can come back shorter than `limit`.)

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test` (with `DATABASE_URL`)
Expected: all pass.

- [ ] **Step 5: Smoke**

Run `serve --no-zyris` for 70 s (long enough for one snapshot tick).
Expected: no errors in the log, and `SELECT count(*) FROM equity_snapshots` against the `atrader` database is at least 2 (one minute snapshot and one daily).

```bash
nix shell nixpkgs#postgresql_16 -c psql -h 127.0.0.1 -p 54329 -U atrader atrader -c "SELECT kind, count(*) FROM equity_snapshots GROUP BY kind"
```

- [ ] **Step 6: Commit**

```bash
git add src tests/tools.rs
git commit -m "Add candle, indicator, screener and performance tools"
git log -1 --format=%B
```
