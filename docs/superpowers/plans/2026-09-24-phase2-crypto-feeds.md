# ATrader Phase 2 (Crypto Feeds) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Feed `SimBroker` with real Upbit and Binance order books and trades, load their instruments and daily statistics, keep a bounded subscription set, and add USD/KRW conversion backed by a cached reference rate.

**Architecture:** Each venue implements the `MarketFeed` trait with REST calls (instruments, snapshot, daily stats) and a WebSocket `stream`. `run_feed` keeps one stream alive per venue for whatever `Subscriptions` currently holds, and reconnects with capped backoff. Every event goes through an `mpsc` channel into `pump`, which applies it to `SimBroker` and re-broadcasts it and any fills on the `BusEvent` channel. `Market` ties the pieces together for callers: `ensure_fresh`, `refresh_pins` and `load_instruments`. Parsing is written as pure functions and tested against recorded JSON.

**Tech Stack:** tokio (sync, time), tokio-tungstenite with rustls, reqwest with rustls, serde_json, futures-util, and tracing. Everything from Phase 1 carries over.

**Spec:** `docs/superpowers/specs/2026-09-24-atrader-design.md`. This plan covers §15 step 2 and implements the crypto half of §4, the FX part of §3, and the stats inputs of §5.

## Global Constraints

- Phase 1's Global Constraints still apply: `Decimal` for money, English text, no Claude attribution in commits, and `ponytail:` comments on shortcuts.
- Endpoints:
  - Upbit REST: `https://api.upbit.com/v1`
  - Upbit WebSocket: `wss://api.upbit.com/websocket/v1`
  - Binance REST: `https://api.binance.com/api/v3`
  - Binance WebSocket: `wss://stream.binance.com:9443/stream?streams=`
  - Frankfurter: `https://api.frankfurter.dev/v1/latest?base=USD&symbols=KRW`
- Only Upbit `KRW-*` markets and Binance `*USDT` spot symbols with status `TRADING` are loaded.
- The Upbit KRW tick table is the one on docs.upbit.com "KRW market info", checked 2026-09-24. Upbit's per-coin exceptions are not modelled.
- `Book.received_at` is the local clock at receipt, not the exchange timestamp, because staleness is about our own view.
- Stream idle timeout is 30 s. Reconnect backoff starts at 1 s, doubles up to 60 s, adds up to 25% jitter, and resets after a stream that lasted more than 60 s.
- A book counts as fresh for `ensure_fresh` when it is at most 2 s old.
- FX: USDT counts as USD, the rate is cached for 1 h, and the default spread is 0.1%. Credits truncate to the target currency's minor unit.
- Daily stats use the last 21 daily candles:
  - `σ` is the sample standard deviation of log returns.
  - ADV is the mean daily quote-currency traded value.
  - If the feed fails, the broker falls back to `SimParams` defaults.
- Tests never touch the network, except those in `tests/live.rs`. Those are `#[ignore]` and run explicitly in Task 9.

## Review Focus

1. **Exchange numbers must parse exactly.** A JSON number such as Upbit's `0.00173353` must become exactly that `Decimal`. A float round trip that introduces drift is not acceptable. Task 4 tests this.
2. **Changing the subscription set must reconnect without losing the feed.** The runner must restart the stream with the new set and must not exit or spin. Task 3 tests this.
3. **A dead socket must not look healthy.** A socket that stops sending must end the stream within 30 s so that the runner reconnects. The idle timeout lives in each `stream`, and Task 9's live run checks that data flows.
4. **Instruments with open exposure must stay subscribed.** Heavy querying of other instruments must never push a pinned instrument (one with a position or resting order) out of the capped set. Task 6 tests this.
5. **FX conversion must not create or destroy money beyond the spread.** A currency pair that is the same, a non-positive amount, or an amount above available cash must return an error without changing any balance. Task 7 tests this.

## File Structure

| File | Responsibility |
| --- | --- |
| `src/venue.rs` | Modified: `TickRule::Upbit` |
| `src/stats.rs` | `daily_stats(closes, values) -> Option<DailyStats>` |
| `src/feed/mod.rs` | `MarketEvent`, `MarketFeed` trait, `run_feed` |
| `src/feed/upbit.rs` | Upbit parsing and `UpbitFeed` |
| `src/feed/binance.rs` | Binance parsing and `BinanceFeed` |
| `src/subs.rs` | `select`, `Subscriptions` (pinned + LRU, capped) |
| `src/fx.rs` | `parse_frankfurter`, `FxCache` |
| `src/broker.rs` | Modified: `book_age`, `active_instruments`, `has_stats`, `Conversion`, `convert_sync` |
| `src/store.rs` | Modified: `save_conversion`; FX entries count in replay |
| `src/market.rs` | `BusEvent`, `pump`, `Market` |
| `tests/live.rs` | `#[ignore]` network smoke tests |

---

### Task 1: Upbit KRW tick table

**Files:**
- Modify: `src/venue.rs`

**Interfaces:**
- Produces: `TickRule::Upbit`. The existing `size_at`, `floor`, `ceil`, `round` and `is_valid` all apply to it.

- [ ] **Step 1: Write the failing test** (append it to `venue.rs` tests)

```rust
    #[test]
    fn upbit_krw_ticks() {
        let t = TickRule::Upbit;
        assert_eq!(t.size_at(dec!(114851000)), dec!(1000));
        assert_eq!(t.size_at(dec!(1000000)), dec!(1000));
        assert_eq!(t.size_at(dec!(750000)), dec!(500));
        assert_eq!(t.size_at(dec!(120000)), dec!(100));
        assert_eq!(t.size_at(dec!(60000)), dec!(50));
        assert_eq!(t.size_at(dec!(12000)), dec!(10));
        assert_eq!(t.size_at(dec!(7000)), dec!(5));
        assert_eq!(t.size_at(dec!(3000)), dec!(1));
        assert_eq!(t.size_at(dec!(500)), dec!(1));
        assert_eq!(t.size_at(dec!(50)), dec!(0.1));
        assert_eq!(t.size_at(dec!(5)), dec!(0.01));
        assert_eq!(t.size_at(dec!(0.5)), dec!(0.001));
        assert_eq!(t.size_at(dec!(0.000001)), dec!(0.00000001));
        assert!(t.is_valid(dec!(114851000)));
        assert!(!t.is_valid(dec!(114851500)));
    }
```

- [ ] **Step 2: Run the test to verify it fails**

Run: `cargo test --lib venue::tests::upbit`
Expected: compile error (`no variant Upbit`).

- [ ] **Step 3: Implement**

Add this variant to `TickRule`, after `Us`:

```rust
    /// Upbit KRW market table (docs.upbit.com "KRW market info", checked 2026-09-24).
    // ponytail: Upbit's per-coin tick exceptions are not modelled; add them when a listed coin trips INVALID_TICK.
    Upbit,
```

Add this next to `KRX_TICKS`:

```rust
/// (lower bound inclusive, tick), highest first; below the last bound the tick is 0.00000001.
const UPBIT_TICKS: [(Decimal, Decimal); 14] = [
    (dec!(1000000), dec!(1000)),
    (dec!(500000), dec!(500)),
    (dec!(100000), dec!(100)),
    (dec!(50000), dec!(50)),
    (dec!(10000), dec!(10)),
    (dec!(5000), dec!(5)),
    (dec!(100), dec!(1)),
    (dec!(10), dec!(0.1)),
    (dec!(1), dec!(0.01)),
    (dec!(0.1), dec!(0.001)),
    (dec!(0.01), dec!(0.0001)),
    (dec!(0.001), dec!(0.00001)),
    (dec!(0.0001), dec!(0.000001)),
    (dec!(0.00001), dec!(0.0000001)),
];
```

Add this arm to `size_at`:

```rust
            TickRule::Upbit => UPBIT_TICKS
                .iter()
                .find(|(lower, _)| price >= *lower)
                .map_or(dec!(0.00000001), |(_, t)| *t),
```

- [ ] **Step 4: Run the test to verify it passes**

Run: `cargo test --lib venue::`
Expected: every venue test passes.

- [ ] **Step 5: Commit**

```bash
git add src/venue.rs
git commit -m "Add Upbit KRW tick table"
git log -1 --format=%B
```

---

### Task 2: Daily statistics

**Files:**
- Create: `src/stats.rs`
- Modify: `src/lib.rs` (add `pub mod stats;`)

**Interfaces:**
- Consumes: `DailyStats` (from `sim`).
- Produces: `daily_stats(closes: &[Decimal], values: &[Decimal]) -> Option<DailyStats>`. Both slices are ordered oldest first. It returns `None` when there are fewer than 3 closes or no values.

- [ ] **Step 1: Write the failing tests**

Create `src/stats.rs`:

```rust
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
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test --lib stats::`
Expected: compile error (`daily_stats` not found).

- [ ] **Step 3: Implement** (prepend to `src/stats.rs`)

```rust
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
```

Add `pub mod stats;` to `src/lib.rs`.

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test --lib stats::`
Expected: 3 passed.

- [ ] **Step 5: Commit**

```bash
git add src/lib.rs src/stats.rs
git commit -m "Add daily volatility and traded value stats"
git log -1 --format=%B
```

---

### Task 3: Feed trait and reconnecting runner

**Files:**
- Create: `src/feed/mod.rs`
- Modify: `src/lib.rs` (add `pub mod feed;`), `Cargo.toml`

**Interfaces:**
- Consumes: `Book`, `Trade`, `InstrumentId`, `Venue`, `Instrument`, `DailyStats`.
- Produces:
  - `MarketEvent { Book(Book), Trade(Trade) }`.
  - `trait MarketFeed: Send + Sync` with these methods:
    - `fn venue(&self) -> Venue`
    - `async fn instruments(&self) -> anyhow::Result<Vec<Instrument>>`
    - `async fn snapshot(&self, id: &InstrumentId) -> anyhow::Result<Book>`
    - `async fn daily_stats(&self, id: &InstrumentId) -> anyhow::Result<DailyStats>`
    - `async fn stream(&self, ids: &[InstrumentId], tx: &mpsc::Sender<MarketEvent>) -> anyhow::Result<()>`, which returns `Ok` only when `tx` is closed.
  - `run_feed(feed: Arc<dyn MarketFeed>, subs: watch::Receiver<Vec<InstrumentId>>, tx: mpsc::Sender<MarketEvent>)`, an async function. It returns when either `tx` or `subs` closes.

- [ ] **Step 1: Add dependencies**

```bash
cargo add tokio --features rt-multi-thread,macros,sync,time
cargo add tokio --dev --features test-util,rt-multi-thread,macros,sync,time
cargo add tokio-tungstenite --features rustls-tls-webpki-roots
cargo add reqwest --no-default-features --features json,rustls-tls
cargo add serde_json futures-util
```

Run: `cargo build`
Expected: it builds. If cargo says a feature does not exist in the resolved version, use that version's documented rustls feature and record the substitution as a ruling. If the build needs a C toolchain it does not have (aws-lc), switch to the crate's ring-based rustls feature.

- [ ] **Step 2: Write the failing tests**

Add `pub mod feed;` to `src/lib.rs`. Create `src/feed/mod.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;
    use rust_decimal::Decimal;
    use std::sync::atomic::{AtomicUsize, Ordering};

    /// Sends one trade per connection, then drops.
    struct Flaky {
        calls: AtomicUsize,
    }

    #[async_trait]
    impl MarketFeed for Flaky {
        fn venue(&self) -> Venue {
            Venue::Upbit
        }
        async fn instruments(&self) -> anyhow::Result<Vec<Instrument>> {
            Ok(vec![])
        }
        async fn snapshot(&self, _: &InstrumentId) -> anyhow::Result<Book> {
            anyhow::bail!("unused")
        }
        async fn daily_stats(&self, _: &InstrumentId) -> anyhow::Result<DailyStats> {
            anyhow::bail!("unused")
        }
        async fn stream(&self, ids: &[InstrumentId], tx: &mpsc::Sender<MarketEvent>) -> anyhow::Result<()> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            let t = Trade { instrument: ids[0].clone(), price: Decimal::ONE, qty: Decimal::ONE, at: Utc::now() };
            tx.send(MarketEvent::Trade(t)).await?;
            anyhow::bail!("dropped")
        }
    }

    fn btc() -> InstrumentId {
        "UPBIT:KRW-BTC".parse().unwrap()
    }

    #[tokio::test(start_paused = true)]
    async fn reconnects_after_a_drop_and_stops_when_closed() {
        let feed = Arc::new(Flaky { calls: AtomicUsize::new(0) });
        let (subs_tx, subs_rx) = watch::channel(vec![btc()]);
        let (tx, mut rx) = mpsc::channel(8);
        let task = tokio::spawn(run_feed(feed.clone(), subs_rx, tx));
        rx.recv().await.unwrap();
        rx.recv().await.unwrap();
        assert!(feed.calls.load(Ordering::SeqCst) >= 2);
        drop(subs_tx);
        drop(rx);
        task.await.unwrap();
    }

    #[tokio::test(start_paused = true)]
    async fn idles_until_something_is_subscribed() {
        let feed = Arc::new(Flaky { calls: AtomicUsize::new(0) });
        let (subs_tx, subs_rx) = watch::channel(vec![]);
        let (tx, mut rx) = mpsc::channel(8);
        tokio::spawn(run_feed(feed.clone(), subs_rx, tx));
        tokio::time::sleep(StdDuration::from_secs(5)).await;
        assert_eq!(feed.calls.load(Ordering::SeqCst), 0);
        subs_tx.send(vec![btc()]).unwrap();
        assert!(rx.recv().await.is_some());
    }
}
```

- [ ] **Step 3: Run the tests to verify they fail**

Run: `cargo test --lib feed::`
Expected: compile error (`MarketFeed` not found).

- [ ] **Step 4: Implement** (prepend to `src/feed/mod.rs`)

```rust
//! Market data feeds. Each venue streams books and trades for a subscription set that
//! `run_feed` keeps alive across disconnects.

pub mod binance;
pub mod upbit;

use std::sync::Arc;
use std::time::{Duration as StdDuration, Instant, SystemTime, UNIX_EPOCH};

use async_trait::async_trait;
use tokio::sync::{mpsc, watch};

use crate::domain::{Book, InstrumentId, Trade, Venue};
use crate::sim::DailyStats;
use crate::venue::Instrument;

#[derive(Debug, Clone, PartialEq)]
pub enum MarketEvent {
    Book(Book),
    Trade(Trade),
}

#[async_trait]
pub trait MarketFeed: Send + Sync {
    fn venue(&self) -> Venue;
    async fn instruments(&self) -> anyhow::Result<Vec<Instrument>>;
    async fn snapshot(&self, id: &InstrumentId) -> anyhow::Result<Book>;
    async fn daily_stats(&self, id: &InstrumentId) -> anyhow::Result<DailyStats>;
    /// Stream books and trades for `ids` into `tx`. Returns `Ok` only when `tx` is closed; any
    /// disconnect or idle timeout is an error so the runner reconnects.
    async fn stream(&self, ids: &[InstrumentId], tx: &mpsc::Sender<MarketEvent>) -> anyhow::Result<()>;
}

/// No data for this long means the socket is dead.
pub const IDLE_TIMEOUT: StdDuration = StdDuration::from_secs(30);

/// Keep `feed` streaming whatever `subs` currently holds. A changed set restarts the stream.
/// Drops reconnect with capped exponential backoff.
// ponytail: resubscribing = reconnecting; send in-band SUBSCRIBE messages if churn gets high.
pub async fn run_feed(feed: Arc<dyn MarketFeed>, mut subs: watch::Receiver<Vec<InstrumentId>>, tx: mpsc::Sender<MarketEvent>) {
    let mut backoff = StdDuration::from_secs(1);
    loop {
        let ids = subs.borrow_and_update().clone();
        if ids.is_empty() {
            if subs.changed().await.is_err() {
                return;
            }
            continue;
        }
        let started = Instant::now();
        tokio::select! {
            r = feed.stream(&ids, &tx) => {
                if tx.is_closed() {
                    return;
                }
                if let Err(e) = r {
                    tracing::warn!(venue = feed.venue().tag(), error = %e, "feed stream ended");
                }
                if started.elapsed() > StdDuration::from_secs(60) {
                    backoff = StdDuration::from_secs(1);
                }
                tokio::time::sleep(backoff + jitter(backoff)).await;
                backoff = (backoff * 2).min(StdDuration::from_secs(60));
            }
            changed = subs.changed() => {
                if changed.is_err() {
                    return;
                }
            }
        }
    }
}

/// Up to +25% of `base`, so reconnecting clients do not stampede together.
// ponytail: clock nanoseconds as the random source; enough to spread reconnects.
fn jitter(base: StdDuration) -> StdDuration {
    let n = SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.subsec_nanos());
    base.mul_f64(f64::from(n % 1000) / 4000.0)
}
```

Create stub files so the module compiles. Tasks 4 and 5 fill them in.

```bash
printf '//! Upbit KRW market feed.\n' > src/feed/upbit.rs
printf '//! Binance USDT spot feed.\n' > src/feed/binance.rs
```

- [ ] **Step 5: Run the tests to verify they pass**

Run: `cargo test --lib feed::`
Expected: 2 passed.

- [ ] **Step 6: Commit**

```bash
git add Cargo.toml Cargo.lock src/lib.rs src/feed
git commit -m "Add market feed trait and reconnecting runner"
git log -1 --format=%B
```

---

### Task 4: Upbit feed

**Files:**
- Modify: `src/feed/upbit.rs`

**Interfaces:**
- Consumes:
  - `MarketEvent`, `MarketFeed`, `IDLE_TIMEOUT` (Task 3).
  - `daily_stats` (Task 2).
  - `TickRule::Upbit` (Task 1).
- Produces:
  - `parse_ws(bytes: &[u8], now) -> anyhow::Result<Option<MarketEvent>>`
  - `parse_book_rest(bytes, now) -> anyhow::Result<Book>`
  - `parse_markets(bytes) -> anyhow::Result<Vec<Instrument>>`
  - `parse_candles(bytes) -> anyhow::Result<DailyStats>`
  - `subscribe_message(ids) -> String`
  - `UpbitFeed::new(clock: Arc<dyn Clock>)`, which implements `MarketFeed`.

- [ ] **Step 1: Write the failing tests** (append to `src/feed/upbit.rs`)

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;
    use rust_decimal_macros::dec;

    const BOOK_WS: &str = r#"{"type":"orderbook","code":"KRW-BTC","timestamp":1790247036776,"total_ask_size":0.87,"total_bid_size":2.47,"orderbook_units":[{"ask_price":114850000,"bid_price":114833000,"ask_size":0.11621747,"bid_size":0.00173353},{"ask_price":114851000,"bid_price":114831000,"ask_size":0.16136247,"bid_size":0.00137989}],"stream_type":"REALTIME"}"#;
    const TRADE_WS: &str = r#"{"type":"trade","code":"KRW-BTC","timestamp":1790247036800,"trade_date":"2026-09-24","trade_time":"09:30:36","trade_timestamp":1790247036700,"trade_price":114850000,"trade_volume":0.0021,"ask_bid":"BID","prev_closing_price":115908000,"change":"FALL","change_price":1058000,"sequential_id":1,"stream_type":"REALTIME"}"#;

    fn now() -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 9, 24, 0, 30, 0).unwrap()
    }

    #[test]
    fn parses_orderbook_frames_exactly() {
        let Some(MarketEvent::Book(b)) = parse_ws(BOOK_WS.as_bytes(), now()).unwrap() else { panic!("not a book") };
        assert_eq!(b.instrument.to_string(), "UPBIT:KRW-BTC");
        assert_eq!(b.bids[0], Level { price: dec!(114833000), qty: dec!(0.00173353) });
        assert_eq!(b.asks[1], Level { price: dec!(114851000), qty: dec!(0.16136247) });
        assert_eq!(b.received_at, now());
    }

    #[test]
    fn parses_trade_frames() {
        let Some(MarketEvent::Trade(t)) = parse_ws(TRADE_WS.as_bytes(), now()).unwrap() else { panic!("not a trade") };
        assert_eq!((t.price, t.qty), (dec!(114850000), dec!(0.0021)));
        assert_eq!(t.at.timestamp_millis(), 1790247036700);
    }

    #[test]
    fn ignores_other_frames() {
        assert_eq!(parse_ws(br#"{"type":"ticker","code":"KRW-BTC"}"#, now()).unwrap(), None);
        assert_eq!(parse_ws(br#"{"status":"UP"}"#, now()).unwrap(), None);
    }

    #[test]
    fn parses_rest_orderbook() {
        let body = format!("[{}]", BOOK_WS.replace(r#""code""#, r#""market""#));
        let b = parse_book_rest(body.as_bytes(), now()).unwrap();
        assert_eq!(b.asks.len(), 2);
    }

    #[test]
    fn loads_only_krw_markets() {
        let body = r#"[{"market":"BTC-FIL","korean_name":"파일코인","english_name":"Filecoin"},{"market":"KRW-BTC","korean_name":"비트코인","english_name":"Bitcoin"}]"#;
        let list = parse_markets(body.as_bytes()).unwrap();
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].id.to_string(), "UPBIT:KRW-BTC");
        assert_eq!(list[0].name, "비트코인 (Bitcoin)");
        assert_eq!(list[0].tick, TickRule::Upbit);
        assert_eq!(list[0].lot.min_notional, dec!(5000));
    }

    #[test]
    fn candles_newest_first_become_stats() {
        let body = r#"[{"trade_price":99,"candle_acc_trade_price":30},{"trade_price":110,"candle_acc_trade_price":20},{"trade_price":100,"candle_acc_trade_price":10}]"#;
        let s = parse_candles(body.as_bytes()).unwrap();
        assert!((s.sigma - 0.141895).abs() < 1e-5);
        assert_eq!(s.adv_notional, dec!(20));
    }

    #[test]
    fn subscribe_message_lists_codes_for_both_types() {
        let ids: Vec<InstrumentId> = vec!["UPBIT:KRW-BTC".parse().unwrap(), "UPBIT:KRW-ETH".parse().unwrap()];
        let v: Value = serde_json::from_str(&subscribe_message(&ids)).unwrap();
        assert_eq!(v[1]["type"], "orderbook");
        assert_eq!(v[1]["codes"], serde_json::json!(["KRW-BTC", "KRW-ETH"]));
        assert_eq!(v[2]["type"], "trade");
    }
}
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test --lib feed::upbit`
Expected: compile errors (`parse_ws` not found).

- [ ] **Step 3: Implement** (above the tests in `src/feed/upbit.rs`)

```rust
use std::sync::Arc;

use anyhow::{Context, anyhow};
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use futures_util::{SinkExt, StreamExt};
use rust_decimal::Decimal;
use rust_decimal_macros::dec;
use serde::Deserialize;
use serde_json::{Value, json};
use tokio::sync::mpsc;
use tokio_tungstenite::tungstenite::Message;

use super::{IDLE_TIMEOUT, MarketEvent, MarketFeed};
use crate::domain::{Book, Clock, InstrumentId, Level, Trade, Venue};
use crate::sim::DailyStats;
use crate::stats::daily_stats;
use crate::venue::{Instrument, LotRule, TickRule};

const REST: &str = "https://api.upbit.com/v1";
const WS: &str = "wss://api.upbit.com/websocket/v1";

#[derive(Deserialize)]
struct Unit {
    ask_price: Decimal,
    bid_price: Decimal,
    ask_size: Decimal,
    bid_size: Decimal,
}

#[derive(Deserialize)]
struct RawBook {
    #[serde(alias = "market")]
    code: String,
    orderbook_units: Vec<Unit>,
}

#[derive(Deserialize)]
struct RawTrade {
    code: String,
    trade_price: Decimal,
    trade_volume: Decimal,
    trade_timestamp: i64,
}

#[derive(Deserialize)]
struct RawMarket {
    market: String,
    korean_name: String,
    english_name: String,
}

#[derive(Deserialize)]
struct RawCandle {
    trade_price: Decimal,
    candle_acc_trade_price: Decimal,
}

fn id(code: &str) -> InstrumentId {
    InstrumentId { venue: Venue::Upbit, symbol: code.to_string() }
}

fn to_book(raw: RawBook, now: DateTime<Utc>) -> Book {
    Book {
        instrument: id(&raw.code),
        bids: raw.orderbook_units.iter().map(|u| Level { price: u.bid_price, qty: u.bid_size }).collect(),
        asks: raw.orderbook_units.iter().map(|u| Level { price: u.ask_price, qty: u.ask_size }).collect(),
        prev_close: None,
        received_at: now,
    }
}

/// Parse one WebSocket frame. Frames that are neither books nor trades yield `None`.
pub fn parse_ws(bytes: &[u8], now: DateTime<Utc>) -> anyhow::Result<Option<MarketEvent>> {
    let v: Value = serde_json::from_slice(bytes)?;
    match v.get("type").and_then(Value::as_str) {
        Some("orderbook") => Ok(Some(MarketEvent::Book(to_book(serde_json::from_value(v)?, now)))),
        Some("trade") => {
            let t: RawTrade = serde_json::from_value(v)?;
            Ok(Some(MarketEvent::Trade(Trade {
                instrument: id(&t.code),
                price: t.trade_price,
                qty: t.trade_volume,
                at: DateTime::from_timestamp_millis(t.trade_timestamp).unwrap_or(now),
            })))
        }
        _ => Ok(None),
    }
}

pub fn parse_book_rest(bytes: &[u8], now: DateTime<Utc>) -> anyhow::Result<Book> {
    let raw: Vec<RawBook> = serde_json::from_slice(bytes)?;
    let first = raw.into_iter().next().ok_or_else(|| anyhow!("empty orderbook response"))?;
    Ok(to_book(first, now))
}

pub fn parse_markets(bytes: &[u8]) -> anyhow::Result<Vec<Instrument>> {
    let raw: Vec<RawMarket> = serde_json::from_slice(bytes)?;
    Ok(raw
        .into_iter()
        .filter(|m| m.market.starts_with("KRW-"))
        .map(|m| Instrument {
            id: id(&m.market),
            name: format!("{} ({})", m.korean_name, m.english_name),
            tick: TickRule::Upbit,
            lot: LotRule { step: dec!(0.00000001), min_qty: dec!(0.00000001), min_notional: dec!(5000) },
            tradable: true,
        })
        .collect())
}

/// Daily candles arrive newest first.
pub fn parse_candles(bytes: &[u8]) -> anyhow::Result<DailyStats> {
    let mut raw: Vec<RawCandle> = serde_json::from_slice(bytes)?;
    raw.reverse();
    let closes: Vec<Decimal> = raw.iter().map(|c| c.trade_price).collect();
    let values: Vec<Decimal> = raw.iter().map(|c| c.candle_acc_trade_price).collect();
    daily_stats(&closes, &values).ok_or_else(|| anyhow!("not enough daily history"))
}

pub fn subscribe_message(ids: &[InstrumentId]) -> String {
    let codes: Vec<&str> = ids.iter().map(|i| i.symbol.as_str()).collect();
    json!([{"ticket": "atrader"}, {"type": "orderbook", "codes": codes}, {"type": "trade", "codes": codes}]).to_string()
}

pub struct UpbitFeed {
    http: reqwest::Client,
    clock: Arc<dyn Clock>,
}

impl UpbitFeed {
    pub fn new(clock: Arc<dyn Clock>) -> Self {
        UpbitFeed { http: reqwest::Client::new(), clock }
    }

    async fn get(&self, path: &str) -> anyhow::Result<Vec<u8>> {
        let r = self.http.get(format!("{REST}{path}")).send().await?.error_for_status()?;
        Ok(r.bytes().await?.to_vec())
    }
}

#[async_trait]
impl MarketFeed for UpbitFeed {
    fn venue(&self) -> Venue {
        Venue::Upbit
    }

    async fn instruments(&self) -> anyhow::Result<Vec<Instrument>> {
        parse_markets(&self.get("/market/all").await?)
    }

    async fn snapshot(&self, id: &InstrumentId) -> anyhow::Result<Book> {
        let body = self.get(&format!("/orderbook?markets={}", id.symbol)).await?;
        parse_book_rest(&body, self.clock.now()).with_context(|| format!("orderbook for {id}"))
    }

    async fn daily_stats(&self, id: &InstrumentId) -> anyhow::Result<DailyStats> {
        parse_candles(&self.get(&format!("/candles/days?market={}&count=21", id.symbol)).await?)
    }

    async fn stream(&self, ids: &[InstrumentId], tx: &mpsc::Sender<MarketEvent>) -> anyhow::Result<()> {
        let (mut ws, _) = tokio_tungstenite::connect_async(WS).await?;
        ws.send(Message::text(subscribe_message(ids))).await?;
        loop {
            let msg = tokio::time::timeout(IDLE_TIMEOUT, ws.next())
                .await
                .map_err(|_| anyhow!("upbit: no data for {IDLE_TIMEOUT:?}"))?
                .ok_or_else(|| anyhow!("upbit websocket closed"))??;
            let bytes = match msg {
                Message::Binary(b) => b.to_vec(),
                Message::Text(t) => t.as_bytes().to_vec(),
                Message::Close(_) => return Err(anyhow!("upbit websocket closed")),
                _ => continue,
            };
            if let Some(ev) = parse_ws(&bytes, self.clock.now())? {
                if tx.send(ev).await.is_err() {
                    return Ok(());
                }
            }
        }
    }
}
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test --lib feed::upbit`
Expected: 7 passed.

If `parses_orderbook_frames_exactly` shows drift (for example `0.0017335300000000001`), the JSON float path is lossy. In that case:
1. Enable `serde_json`'s `arbitrary_precision` feature and `rust_decimal`'s `serde-with-arbitrary-precision` feature.
2. Rerun the test.
3. Record a ruling.

- [ ] **Step 5: Commit**

```bash
git add src/feed/upbit.rs Cargo.toml Cargo.lock
git commit -m "Add Upbit market feed"
git log -1 --format=%B
```

---

### Task 5: Binance feed

**Files:**
- Modify: `src/feed/binance.rs`

**Interfaces:**
- Consumes: the same items as Task 4.
- Produces:
  - `parse_ws(bytes, now) -> anyhow::Result<Option<MarketEvent>>`
  - `parse_depth(symbol, bytes, now) -> anyhow::Result<Book>`
  - `parse_exchange_info(bytes) -> anyhow::Result<Vec<Instrument>>`
  - `parse_klines(bytes) -> anyhow::Result<DailyStats>`
  - `stream_url(ids) -> String`
  - `BinanceFeed::new(clock)`, which implements `MarketFeed`.

- [ ] **Step 1: Write the failing tests** (append to `src/feed/binance.rs`)

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;
    use rust_decimal_macros::dec;

    fn now() -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 9, 24, 0, 30, 0).unwrap()
    }

    #[test]
    fn parses_depth_frames() {
        let f = r#"{"stream":"btcusdt@depth20@100ms","data":{"lastUpdateId":1,"bids":[["83532.15000000","1.53981000"]],"asks":[["83532.16000000","4.45844000"],["83532.17000000","0.00039000"]]}}"#;
        let Some(MarketEvent::Book(b)) = parse_ws(f.as_bytes(), now()).unwrap() else { panic!("not a book") };
        assert_eq!(b.instrument.to_string(), "BINANCE:BTCUSDT");
        assert_eq!(b.bids[0], Level { price: dec!(83532.15), qty: dec!(1.53981) });
        assert_eq!(b.asks.len(), 2);
    }

    #[test]
    fn parses_trade_frames() {
        let f = r#"{"stream":"btcusdt@trade","data":{"e":"trade","E":1,"s":"BTCUSDT","t":5,"p":"83532.16000000","q":"0.00100000","T":1790247036700,"m":true,"M":true}}"#;
        let Some(MarketEvent::Trade(t)) = parse_ws(f.as_bytes(), now()).unwrap() else { panic!("not a trade") };
        assert_eq!((t.price, t.qty), (dec!(83532.16), dec!(0.001)));
        assert_eq!(t.at.timestamp_millis(), 1790247036700);
    }

    #[test]
    fn exchange_info_keeps_trading_usdt_symbols() {
        let body = r#"{"symbols":[
          {"symbol":"BTCUSDT","status":"TRADING","baseAsset":"BTC","quoteAsset":"USDT","filters":[
            {"filterType":"PRICE_FILTER","minPrice":"0.01000000","maxPrice":"1000000.00000000","tickSize":"0.01000000"},
            {"filterType":"LOT_SIZE","minQty":"0.00001000","maxQty":"9000.00000000","stepSize":"0.00001000"},
            {"filterType":"NOTIONAL","minNotional":"5.00000000","applyMinToMarket":true}]},
          {"symbol":"ETHBTC","status":"TRADING","baseAsset":"ETH","quoteAsset":"BTC","filters":[]},
          {"symbol":"OLDUSDT","status":"BREAK","baseAsset":"OLD","quoteAsset":"USDT","filters":[]}]}"#;
        let list = parse_exchange_info(body.as_bytes()).unwrap();
        assert_eq!(list.len(), 1);
        let i = &list[0];
        assert_eq!((i.id.to_string(), i.name.as_str()), ("BINANCE:BTCUSDT".to_string(), "BTC/USDT"));
        assert_eq!(i.tick, TickRule::Fixed(dec!(0.01)));
        assert_eq!((i.lot.step, i.lot.min_qty, i.lot.min_notional), (dec!(0.00001), dec!(0.00001), dec!(5)));
    }

    #[test]
    fn klines_become_stats() {
        let body = r#"[[0,"1","1","1","100","1",0,"10",1,"1","1","0"],[0,"1","1","1","110","1",0,"20",1,"1","1","0"],[0,"1","1","1","99","1",0,"30",1,"1","1","0"]]"#;
        let s = parse_klines(body.as_bytes()).unwrap();
        assert!((s.sigma - 0.141895).abs() < 1e-5);
        assert_eq!(s.adv_notional, dec!(20));
    }

    #[test]
    fn stream_url_combines_depth_and_trade() {
        let ids: Vec<InstrumentId> = vec!["BINANCE:BTCUSDT".parse().unwrap(), "BINANCE:ETHUSDT".parse().unwrap()];
        assert_eq!(
            stream_url(&ids),
            "wss://stream.binance.com:9443/stream?streams=btcusdt@depth20@100ms/btcusdt@trade/ethusdt@depth20@100ms/ethusdt@trade"
        );
    }
}
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test --lib feed::binance`
Expected: compile errors.

- [ ] **Step 3: Implement** (above the tests)

```rust
use std::sync::Arc;

use anyhow::{Context, anyhow};
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use futures_util::StreamExt;
use rust_decimal::Decimal;
use serde::Deserialize;
use serde_json::Value;
use tokio::sync::mpsc;
use tokio_tungstenite::tungstenite::Message;

use super::{IDLE_TIMEOUT, MarketEvent, MarketFeed};
use crate::domain::{Book, Clock, InstrumentId, Level, Trade, Venue};
use crate::sim::DailyStats;
use crate::stats::daily_stats;
use crate::venue::{Instrument, LotRule, TickRule};

const REST: &str = "https://api.binance.com/api/v3";
const WS: &str = "wss://stream.binance.com:9443/stream?streams=";

#[derive(Deserialize)]
struct RawDepth {
    bids: Vec<(Decimal, Decimal)>,
    asks: Vec<(Decimal, Decimal)>,
}

#[derive(Deserialize)]
struct RawTrade {
    s: String,
    p: Decimal,
    q: Decimal,
    #[serde(rename = "T")]
    time: i64,
}

#[derive(Deserialize)]
struct Envelope {
    stream: String,
    data: Value,
}

#[derive(Deserialize)]
struct RawInfo {
    symbols: Vec<RawSymbol>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawSymbol {
    symbol: String,
    status: String,
    base_asset: String,
    quote_asset: String,
    filters: Vec<Value>,
}

fn id(symbol: &str) -> InstrumentId {
    InstrumentId { venue: Venue::Binance, symbol: symbol.to_uppercase() }
}

fn to_book(symbol: &str, d: RawDepth, now: DateTime<Utc>) -> Book {
    let levels = |v: Vec<(Decimal, Decimal)>| v.into_iter().map(|(price, qty)| Level { price, qty }).collect();
    Book { instrument: id(symbol), bids: levels(d.bids), asks: levels(d.asks), prev_close: None, received_at: now }
}

/// Parse one combined-stream frame. Streams other than depth and trade yield `None`.
pub fn parse_ws(bytes: &[u8], now: DateTime<Utc>) -> anyhow::Result<Option<MarketEvent>> {
    let env: Envelope = serde_json::from_slice(bytes)?;
    let (symbol, kind) = env.stream.split_once('@').ok_or_else(|| anyhow!("bad stream name {}", env.stream))?;
    if kind.starts_with("depth") {
        Ok(Some(MarketEvent::Book(to_book(symbol, serde_json::from_value(env.data)?, now))))
    } else if kind == "trade" {
        let t: RawTrade = serde_json::from_value(env.data)?;
        Ok(Some(MarketEvent::Trade(Trade {
            instrument: id(&t.s),
            price: t.p,
            qty: t.q,
            at: DateTime::from_timestamp_millis(t.time).unwrap_or(now),
        })))
    } else {
        Ok(None)
    }
}

pub fn parse_depth(symbol: &str, bytes: &[u8], now: DateTime<Utc>) -> anyhow::Result<Book> {
    Ok(to_book(symbol, serde_json::from_slice(bytes)?, now))
}

pub fn parse_exchange_info(bytes: &[u8]) -> anyhow::Result<Vec<Instrument>> {
    let info: RawInfo = serde_json::from_slice(bytes)?;
    Ok(info
        .symbols
        .into_iter()
        .filter(|s| s.status == "TRADING" && s.quote_asset == "USDT")
        .filter_map(|s| {
            let filter = |kind: &str, key: &str| -> Option<Decimal> {
                let f = s.filters.iter().find(|f| f["filterType"] == kind)?;
                f.get(key)?.as_str()?.parse::<Decimal>().ok().map(|d| d.normalize())
            };
            Some(Instrument {
                id: id(&s.symbol),
                name: format!("{}/{}", s.base_asset, s.quote_asset),
                tick: TickRule::Fixed(filter("PRICE_FILTER", "tickSize")?),
                lot: LotRule {
                    step: filter("LOT_SIZE", "stepSize")?,
                    min_qty: filter("LOT_SIZE", "minQty")?,
                    min_notional: filter("NOTIONAL", "minNotional")
                        .or_else(|| filter("MIN_NOTIONAL", "minNotional"))
                        .unwrap_or_default(),
                },
                tradable: true,
            })
        })
        .collect())
}

/// Daily klines, oldest first: index 4 is the close, index 7 the quote-asset volume.
pub fn parse_klines(bytes: &[u8]) -> anyhow::Result<DailyStats> {
    let rows: Vec<Vec<Value>> = serde_json::from_slice(bytes)?;
    let field = |r: &Vec<Value>, i: usize| -> anyhow::Result<Decimal> {
        r.get(i).and_then(Value::as_str).ok_or_else(|| anyhow!("kline field {i} missing"))?.parse().map_err(Into::into)
    };
    let closes = rows.iter().map(|r| field(r, 4)).collect::<anyhow::Result<Vec<_>>>()?;
    let values = rows.iter().map(|r| field(r, 7)).collect::<anyhow::Result<Vec<_>>>()?;
    daily_stats(&closes, &values).ok_or_else(|| anyhow!("not enough daily history"))
}

pub fn stream_url(ids: &[InstrumentId]) -> String {
    let streams: Vec<String> = ids
        .iter()
        .map(|i| {
            let s = i.symbol.to_lowercase();
            format!("{s}@depth20@100ms/{s}@trade")
        })
        .collect();
    format!("{WS}{}", streams.join("/"))
}

pub struct BinanceFeed {
    http: reqwest::Client,
    clock: Arc<dyn Clock>,
}

impl BinanceFeed {
    pub fn new(clock: Arc<dyn Clock>) -> Self {
        BinanceFeed { http: reqwest::Client::new(), clock }
    }

    async fn get(&self, path: &str) -> anyhow::Result<Vec<u8>> {
        let r = self.http.get(format!("{REST}{path}")).send().await?.error_for_status()?;
        Ok(r.bytes().await?.to_vec())
    }
}

#[async_trait]
impl MarketFeed for BinanceFeed {
    fn venue(&self) -> Venue {
        Venue::Binance
    }

    async fn instruments(&self) -> anyhow::Result<Vec<Instrument>> {
        parse_exchange_info(&self.get("/exchangeInfo?permissions=SPOT").await?)
    }

    async fn snapshot(&self, id: &InstrumentId) -> anyhow::Result<Book> {
        let body = self.get(&format!("/depth?symbol={}&limit=20", id.symbol)).await?;
        parse_depth(&id.symbol, &body, self.clock.now()).with_context(|| format!("depth for {id}"))
    }

    async fn daily_stats(&self, id: &InstrumentId) -> anyhow::Result<DailyStats> {
        parse_klines(&self.get(&format!("/klines?symbol={}&interval=1d&limit=21", id.symbol)).await?)
    }

    async fn stream(&self, ids: &[InstrumentId], tx: &mpsc::Sender<MarketEvent>) -> anyhow::Result<()> {
        let (mut ws, _) = tokio_tungstenite::connect_async(stream_url(ids)).await?;
        loop {
            let msg = tokio::time::timeout(IDLE_TIMEOUT, ws.next())
                .await
                .map_err(|_| anyhow!("binance: no data for {IDLE_TIMEOUT:?}"))?
                .ok_or_else(|| anyhow!("binance websocket closed"))??;
            let bytes = match msg {
                Message::Text(t) => t.as_bytes().to_vec(),
                Message::Binary(b) => b.to_vec(),
                Message::Close(_) => return Err(anyhow!("binance websocket closed")),
                _ => continue,
            };
            if let Some(ev) = parse_ws(&bytes, self.clock.now())? {
                if tx.send(ev).await.is_err() {
                    return Ok(());
                }
            }
        }
    }
}
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test --lib feed::binance`
Expected: 5 passed.

- [ ] **Step 5: Commit**

```bash
git add src/feed/binance.rs
git commit -m "Add Binance market feed"
git log -1 --format=%B
```

---

### Task 6: Subscription set

**Files:**
- Create: `src/subs.rs`
- Modify: `src/lib.rs` (add `pub mod subs;`)

**Interfaces:**
- Produces:
  - `select(pinned: &[InstrumentId], recent: &VecDeque<InstrumentId>, cap: usize) -> Vec<InstrumentId>`.
  - `Subscriptions::new(cap) -> (Subscriptions, watch::Receiver<Vec<InstrumentId>>)`.
  - `touch(&self, &InstrumentId)`, `pin(&self, Vec<InstrumentId>)`, and `current(&self) -> Vec<InstrumentId>`.

- [ ] **Step 1: Write the failing tests** (create `src/subs.rs` holding only the tests)

```rust
#[cfg(test)]
mod tests {
    use super::*;

    fn ids(s: &[&str]) -> Vec<InstrumentId> {
        s.iter().map(|x| format!("UPBIT:{x}").parse().unwrap()).collect()
    }

    #[test]
    fn pinned_come_first_then_recent_up_to_cap() {
        let recent: VecDeque<InstrumentId> = ids(&["C", "A", "D"]).into();
        assert_eq!(select(&ids(&["A", "B"]), &recent, 3), ids(&["A", "B", "C"]));
    }

    #[test]
    fn heavy_querying_never_evicts_a_pinned_instrument() {
        let (subs, rx) = Subscriptions::new(3);
        subs.pin(ids(&["HELD"]));
        for x in ["Q1", "Q2", "Q3", "Q4", "Q5"] {
            subs.touch(&ids(&[x])[0]);
        }
        let set = rx.borrow().clone();
        assert_eq!(set.len(), 3);
        assert_eq!(set[0], ids(&["HELD"])[0]);
        assert_eq!(&set[1..], &ids(&["Q5", "Q4"])[..]);
    }

    #[test]
    fn unchanged_set_does_not_notify() {
        let (subs, mut rx) = Subscriptions::new(3);
        subs.touch(&ids(&["A"])[0]);
        rx.borrow_and_update();
        subs.touch(&ids(&["A"])[0]);
        assert!(!rx.has_changed().unwrap());
        assert_eq!(subs.current(), ids(&["A"]));
    }
}
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test --lib subs::`
Expected: compile errors.

- [ ] **Step 3: Implement** (prepend)

```rust
//! Which instruments each feed streams: pinned ones (open exposure) always, then the most
//! recently queried, up to the feed's cap.

use std::collections::VecDeque;
use std::sync::Mutex;

use tokio::sync::watch;

use crate::domain::InstrumentId;

pub fn select(pinned: &[InstrumentId], recent: &VecDeque<InstrumentId>, cap: usize) -> Vec<InstrumentId> {
    let mut out: Vec<InstrumentId> = Vec::new();
    for id in pinned.iter().chain(recent.iter()) {
        if out.len() >= cap {
            break;
        }
        if !out.contains(id) {
            out.push(id.clone());
        }
    }
    out
}

pub struct Subscriptions {
    cap: usize,
    pinned: Mutex<Vec<InstrumentId>>,
    recent: Mutex<VecDeque<InstrumentId>>,
    tx: watch::Sender<Vec<InstrumentId>>,
}

impl Subscriptions {
    pub fn new(cap: usize) -> (Self, watch::Receiver<Vec<InstrumentId>>) {
        let (tx, rx) = watch::channel(Vec::new());
        let subs = Subscriptions { cap, pinned: Mutex::new(Vec::new()), recent: Mutex::new(VecDeque::new()), tx };
        (subs, rx)
    }

    /// Mark `id` as just queried.
    pub fn touch(&self, id: &InstrumentId) {
        {
            let mut recent = self.recent.lock().unwrap();
            recent.retain(|x| x != id);
            recent.push_front(id.clone());
            recent.truncate(self.cap);
        }
        self.publish();
    }

    /// Replace the set of instruments that must stay subscribed.
    // ponytail: pins beyond the cap are dropped; only matters when open exposure exceeds a feed's cap.
    pub fn pin(&self, ids: Vec<InstrumentId>) {
        *self.pinned.lock().unwrap() = ids;
        self.publish();
    }

    pub fn current(&self) -> Vec<InstrumentId> {
        self.tx.borrow().clone()
    }

    fn publish(&self) {
        let set = select(&self.pinned.lock().unwrap(), &self.recent.lock().unwrap(), self.cap);
        self.tx.send_if_modified(|cur| {
            let changed = *cur != set;
            if changed {
                *cur = set;
            }
            changed
        });
    }
}
```

Add `pub mod subs;` to `src/lib.rs`.

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test --lib subs::`
Expected: 3 passed.

- [ ] **Step 5: Commit**

```bash
git add src/lib.rs src/subs.rs
git commit -m "Add capped subscription set with pinned instruments"
git log -1 --format=%B
```

---

### Task 7: Broker queries, FX conversion, rate cache and FX persistence

**Files:**
- Create: `src/fx.rs`
- Modify: `src/broker.rs`, `src/store.rs`, `src/lib.rs` (add `pub mod fx;`), `tests/broker.rs`, `tests/store.rs`

**Interfaces:**
- Produces:
  - `SimBroker::book_age(&InstrumentId) -> Option<chrono::Duration>`.
  - `SimBroker::active_instruments() -> Vec<InstrumentId>`, sorted and deduplicated.
  - `SimBroker::has_stats(&InstrumentId) -> bool`.
  - `Conversion { from, to, debit, credit, rate }`.
  - `SimBroker::convert_sync(account, from, to, amount, usd_krw, spread) -> Result<Conversion, OrderError>`.
  - `fx::parse_frankfurter(&[u8]) -> anyhow::Result<Decimal>`.
  - `fx::FxCache::new(reqwest::Client)` with `usd_krw(&self) -> anyhow::Result<Decimal>`.
  - `Store::save_conversion(account, generation, &Conversion, at) -> sqlx::Result<()>`.

- [ ] **Step 1: Write the failing tests**

Append this to `tests/broker.rs`:

```rust
#[test]
fn book_age_and_active_instruments() {
    let (b, clock) = setup();
    clock.advance(Duration::seconds(3));
    assert_eq!(b.book_age(&btc()), Some(Duration::seconds(3)));
    assert_eq!(b.book_age(&"UPBIT:KRW-XRP".parse().unwrap()), None);
    clock.set(Utc.with_ymd_and_hms(2026, 9, 23, 1, 0, 0).unwrap());
    b.place_sync("a", market_buy(dec!(0.1))).unwrap();
    b.place_sync("a", limit(samsung(), Side::Buy, dec!(1), dec!(69900), Tif::Day)).unwrap();
    assert_eq!(b.active_instruments(), vec![samsung(), btc()]);
    assert!(!b.has_stats(&btc()));
    b.set_stats(btc(), atrader::sim::DailyStats { sigma: 0.02, adv_notional: dec!(1) });
    assert!(b.has_stats(&btc()));
}

#[test]
fn convert_moves_cash_at_rate_minus_spread() {
    let (b, _) = setup();
    let c = b.convert_sync("a", Currency::Krw, Currency::Usd, dec!(1365350), dec!(1365.35), dec!(0.001)).unwrap();
    assert_eq!((c.debit, c.credit), (dec!(1365350), dec!(999)));
    let pf = b.portfolio("a").unwrap();
    assert_eq!(pf.cash(Currency::Krw), dec!(1000000000) - dec!(1365350));
    assert_eq!(pf.cash(Currency::Usd), dec!(999));
    let back = b.convert_sync("a", Currency::Usd, Currency::Krw, dec!(100), dec!(1365.35), dec!(0.001)).unwrap();
    assert_eq!(back.credit, dec!(136398)); // 136,398.465 truncated
}

#[test]
fn bad_conversions_change_nothing() {
    let (b, _) = setup();
    let before = b.portfolio("a").unwrap();
    assert!(matches!(b.convert_sync("a", Currency::Krw, Currency::Krw, dec!(1), dec!(1365), dec!(0)), Err(OrderError::InvalidRequest(_))));
    assert!(matches!(b.convert_sync("a", Currency::Krw, Currency::Usd, dec!(0), dec!(1365), dec!(0)), Err(OrderError::InvalidRequest(_))));
    assert!(matches!(b.convert_sync("a", Currency::Usd, Currency::Krw, dec!(1), dec!(1365), dec!(0)), Err(OrderError::InsufficientFunds { .. })));
    assert_eq!(b.convert_sync("nobody", Currency::Krw, Currency::Usd, dec!(1), dec!(1365), dec!(0)), Err(OrderError::UnknownAccount));
    assert_eq!(b.portfolio("a").unwrap(), before);
}
```

Append this to `tests/store.rs`:

```rust
#[sqlx::test]
async fn conversions_survive_replay(pool: PgPool) {
    let store = Store::new(pool);
    store.create_account("a", "Test", None, &[(Currency::Krw, dec!(2000000))], Utc::now()).await.unwrap();
    let c = Conversion { from: Currency::Krw, to: Currency::Usd, debit: dec!(1365350), credit: dec!(999), rate: dec!(0.00073167) };
    store.save_conversion("a", 1, &c, Utc::now()).await.unwrap();
    let cash = store.cash_balances("a", 1).await.unwrap();
    assert_eq!((cash[&Currency::Krw], cash[&Currency::Usd]), (dec!(634650), dec!(999)));
    let pf = store.load_portfolio("a", 1).await.unwrap();
    assert_eq!((pf.cash(Currency::Krw), pf.cash(Currency::Usd)), (dec!(634650), dec!(999)));
}
```

Create `src/fx.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use rust_decimal_macros::dec;

    #[test]
    fn parses_frankfurter_rate_exactly() {
        let body = br#"{"amount":1.0,"base":"USD","date":"2026-09-23","rates":{"KRW":1365.35}}"#;
        assert_eq!(parse_frankfurter(body).unwrap(), dec!(1365.35));
        assert!(parse_frankfurter(br#"{"rates":{}}"#).is_err());
    }
}
```

Add `pub mod fx;` to `src/lib.rs`.

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test --lib fx:: && cargo test --test broker`
Expected: compile errors (`book_age`, `convert_sync` and `parse_frankfurter` not found).

- [ ] **Step 3: Implement**

Prepend this to `src/fx.rs`:

```rust
//! USD/KRW reference rate (ECB via Frankfurter), cached.

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use anyhow::anyhow;
use rust_decimal::Decimal;
use serde::Deserialize;

const URL: &str = "https://api.frankfurter.dev/v1/latest?base=USD&symbols=KRW";
const MAX_AGE: Duration = Duration::from_secs(3600);

pub fn parse_frankfurter(bytes: &[u8]) -> anyhow::Result<Decimal> {
    #[derive(Deserialize)]
    struct Resp {
        rates: HashMap<String, Decimal>,
    }
    let r: Resp = serde_json::from_slice(bytes)?;
    r.rates.get("KRW").copied().ok_or_else(|| anyhow!("no KRW rate in response"))
}

pub struct FxCache {
    http: reqwest::Client,
    cached: Mutex<Option<(Decimal, Instant)>>,
}

impl FxCache {
    pub fn new(http: reqwest::Client) -> Self {
        FxCache { http, cached: Mutex::new(None) }
    }

    /// KRW per USD. Refreshes after an hour; if the refresh fails, the last good rate is used.
    pub async fn usd_krw(&self) -> anyhow::Result<Decimal> {
        let cached = *self.cached.lock().unwrap();
        if let Some((rate, at)) = cached {
            if at.elapsed() < MAX_AGE {
                return Ok(rate);
            }
        }
        let fetched = async {
            let body = self.http.get(URL).send().await?.error_for_status()?.bytes().await?;
            parse_frankfurter(&body)
        }
        .await;
        match (fetched, cached) {
            (Ok(rate), _) => {
                *self.cached.lock().unwrap() = Some((rate, Instant::now()));
                Ok(rate)
            }
            (Err(e), Some((rate, _))) => {
                tracing::warn!(error = %e, "fx refresh failed; using the last rate");
                Ok(rate)
            }
            (Err(e), None) => Err(e),
        }
    }
}
```

In `src/broker.rs`, change the `rust_decimal` import to `use rust_decimal::{Decimal, RoundingStrategy};`. Add this after `Estimate`:

```rust
/// One cash conversion between currencies.
#[derive(Debug, Clone, PartialEq)]
pub struct Conversion {
    pub from: Currency,
    pub to: Currency,
    pub debit: Decimal,
    pub credit: Decimal,
    /// Units of `to` per unit of `from`, after the spread.
    pub rate: Decimal,
}
```

Add these to `impl SimBroker`:

```rust
    /// How old the instrument's latest book is, if there is one.
    pub fn book_age(&self, id: &InstrumentId) -> Option<Duration> {
        let now = self.clock.now();
        self.world.lock().unwrap().books.get(id).map(|b| now - b.received_at)
    }

    /// Instruments with a position or a resting order in any account, sorted.
    pub fn active_instruments(&self) -> Vec<InstrumentId> {
        let w = self.world.lock().unwrap();
        let mut ids: Vec<InstrumentId> = w
            .accounts
            .values()
            .flat_map(|p| p.positions.keys().cloned())
            .chain(w.resting.iter().filter(|(_, r)| !r.is_empty()).map(|(id, _)| id.clone()))
            .collect();
        ids.sort_by_key(|i| i.to_string());
        ids.dedup();
        ids
    }

    pub fn has_stats(&self, id: &InstrumentId) -> bool {
        self.world.lock().unwrap().stats.contains_key(id)
    }

    /// Move cash between currencies at `usd_krw` KRW per USD (USDT counts as USD), less `spread`.
    /// The credit is truncated to the target currency's minor unit.
    pub fn convert_sync(
        &self,
        account: &str,
        from: Currency,
        to: Currency,
        amount: Decimal,
        usd_krw: Decimal,
        spread: Decimal,
    ) -> Result<Conversion, OrderError> {
        if from == to || amount <= Decimal::ZERO || amount > MAX_INPUT || usd_krw <= Decimal::ZERO {
            return Err(OrderError::InvalidRequest(
                "convert needs two different currencies, a positive amount and a positive rate".into(),
            ));
        }
        let mut w = self.world.lock().unwrap();
        let pf = w.accounts.get_mut(account).ok_or(OrderError::UnknownAccount)?;
        let available = pf.available_cash(from);
        if amount > available {
            return Err(OrderError::InsufficientFunds { required: amount, available });
        }
        let krw_per = |c: Currency| if c == Currency::Krw { Decimal::ONE } else { usd_krw };
        let net = Decimal::ONE - spread;
        let credit = (amount * krw_per(from) * net / krw_per(to)).round_dp_with_strategy(to.decimals(), RoundingStrategy::ToZero);
        *pf.cash.entry(from).or_default() -= amount;
        *pf.cash.entry(to).or_default() += credit;
        Ok(Conversion { from, to, debit: amount, credit, rate: (krw_per(from) * net / krw_per(to)).round_dp(8) })
    }
```

In `src/store.rs`:
1. Import `Conversion` alongside `Fill` and `Order`.
2. Rename the `sum_by_currency` flag to `non_trade_only`.
3. Change its SQL condition to `AND (NOT $3 OR kind IN ('deposit', 'fx'))`.
4. Add this method:

```rust
    /// Record a currency conversion as two `fx` ledger entries.
    pub async fn save_conversion(&self, account: &str, generation: i32, c: &Conversion, at: DateTime<Utc>) -> sqlx::Result<()> {
        let mut tx = self.pool.begin().await?;
        for (currency, amount) in [(c.from, -c.debit), (c.to, c.credit)] {
            sqlx::query(
                "INSERT INTO ledger_entries (account_id, generation, currency, amount, kind, at)
                 VALUES ($1, $2, $3, $4, 'fx', $5)",
            )
            .bind(account)
            .bind(generation)
            .bind(currency.code())
            .bind(amount)
            .bind(at)
            .execute(&mut *tx)
            .await?;
        }
        tx.commit().await
    }
```

`load_portfolio` already calls `sum_by_currency(.., true)`. After the rename, that call covers deposits plus FX, and fills are replayed on top as before.

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test` with `DATABASE_URL` exported (`scripts/dev-db.sh`).
Expected: everything passes, including the 3 new broker tests, 1 new store test and 1 fx test.

- [ ] **Step 5: Commit**

```bash
git add src/fx.rs src/broker.rs src/store.rs src/lib.rs tests/broker.rs tests/store.rs
git commit -m "Add FX conversion with cached reference rate and broker queries for feeds"
git log -1 --format=%B
```

---

### Task 8: Event pump and `Market` facade

**Files:**
- Create: `src/market.rs`
- Modify: `src/lib.rs` (add `pub mod market;`)

**Interfaces:**
- Consumes:
  - `MarketFeed` and `MarketEvent` (Task 3).
  - `Subscriptions` (Task 6).
  - `SimBroker::{on_book, on_trade, book_age, has_stats, set_stats, add_instrument, active_instruments}`.
- Produces:
  - `BusEvent { Market(MarketEvent), Fill(Fill) }`.
  - `pump(rx: mpsc::Receiver<MarketEvent>, broker: Arc<SimBroker>, bus: broadcast::Sender<BusEvent>)`, an async function.
  - `Market::new(broker, bus)` with these methods:
    - `add_feed(&mut self, feed, cap) -> watch::Receiver<Vec<InstrumentId>>`
    - `load_instruments() -> anyhow::Result<usize>`
    - `ensure_fresh(&InstrumentId) -> anyhow::Result<()>`
    - `refresh_pins()`

- [ ] **Step 1: Write the failing tests** (create `src/market.rs` holding only the tests)

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::broker::{OrderRequest, OrderType, Tif};
    use crate::domain::{Book, Clock, Currency, Level, ManualClock, Side, Trade};
    use crate::sim::{DailyStats, Size};
    use crate::venue::{Calendar, Instrument, LotRule, TickRule};
    use async_trait::async_trait;
    use chrono::{TimeZone, Utc};
    use rust_decimal_macros::dec;
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn btc() -> InstrumentId {
        "UPBIT:KRW-BTC".parse().unwrap()
    }

    fn book(clock: &ManualClock) -> Book {
        Book {
            instrument: btc(),
            bids: vec![Level { price: dec!(99999000), qty: dec!(1) }],
            asks: vec![Level { price: dec!(100000000), qty: dec!(1) }],
            prev_close: None,
            received_at: clock.now(),
        }
    }

    struct Fake {
        clock: ManualClock,
        snaps: AtomicUsize,
        stats: AtomicUsize,
    }

    #[async_trait]
    impl MarketFeed for Fake {
        fn venue(&self) -> Venue {
            Venue::Upbit
        }
        async fn instruments(&self) -> anyhow::Result<Vec<Instrument>> {
            Ok(vec![Instrument {
                id: btc(),
                name: "Bitcoin".into(),
                tick: TickRule::Upbit,
                lot: LotRule { step: dec!(0.00000001), min_qty: dec!(0.00000001), min_notional: dec!(5000) },
                tradable: true,
            }])
        }
        async fn snapshot(&self, _: &InstrumentId) -> anyhow::Result<Book> {
            self.snaps.fetch_add(1, Ordering::SeqCst);
            Ok(book(&self.clock))
        }
        async fn daily_stats(&self, _: &InstrumentId) -> anyhow::Result<DailyStats> {
            self.stats.fetch_add(1, Ordering::SeqCst);
            Ok(DailyStats { sigma: 0.02, adv_notional: dec!(50000000000) })
        }
        async fn stream(&self, _: &[InstrumentId], _: &mpsc::Sender<MarketEvent>) -> anyhow::Result<()> {
            anyhow::bail!("unused")
        }
    }

    struct Rig {
        clock: ManualClock,
        broker: Arc<SimBroker>,
        feed: Arc<Fake>,
        market: Market,
        subs: watch::Receiver<Vec<InstrumentId>>,
        bus: broadcast::Sender<BusEvent>,
    }

    async fn rig() -> Rig {
        let clock = ManualClock::new(Utc.with_ymd_and_hms(2026, 9, 23, 1, 0, 0).unwrap());
        let broker = Arc::new(SimBroker::new(Arc::new(clock.clone()), Calendar::default()));
        broker.open_account("a", &[(Currency::Krw, dec!(1000000000))]);
        let (bus, _) = broadcast::channel(64);
        let mut market = Market::new(broker.clone(), bus.clone());
        let feed = Arc::new(Fake { clock: clock.clone(), snaps: AtomicUsize::new(0), stats: AtomicUsize::new(0) });
        let subs = market.add_feed(feed.clone(), 10);
        assert_eq!(market.load_instruments().await.unwrap(), 1);
        Rig { clock, broker, feed, market, subs, bus }
    }

    #[tokio::test]
    async fn ensure_fresh_snapshots_only_when_stale_and_subscribes() {
        let r = rig().await;
        r.market.ensure_fresh(&btc()).await.unwrap();
        assert_eq!(r.feed.snaps.load(Ordering::SeqCst), 1);
        assert_eq!(r.subs.borrow().clone(), vec![btc()]);
        assert!(r.broker.has_stats(&btc()));
        r.market.ensure_fresh(&btc()).await.unwrap();
        assert_eq!(r.feed.snaps.load(Ordering::SeqCst), 1);
        r.clock.advance(chrono::Duration::seconds(3));
        r.market.ensure_fresh(&btc()).await.unwrap();
        assert_eq!(r.feed.snaps.load(Ordering::SeqCst), 2);
        assert_eq!(r.feed.stats.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn unknown_venue_is_an_error() {
        let r = rig().await;
        assert!(r.market.ensure_fresh(&"BINANCE:BTCUSDT".parse().unwrap()).await.is_err());
    }

    #[tokio::test]
    async fn refresh_pins_keeps_positions_subscribed() {
        let r = rig().await;
        r.market.ensure_fresh(&btc()).await.unwrap();
        let buy = OrderRequest {
            instrument: btc(),
            side: Side::Buy,
            kind: OrderType::Market,
            size: Size::Qty(dec!(0.1)),
            limit_price: None,
            tif: Tif::Ioc,
            reason: "test".into(),
        };
        r.broker.place_sync("a", buy).unwrap();
        r.market.refresh_pins();
        assert!(r.subs.borrow().contains(&btc()));
    }

    #[tokio::test]
    async fn pump_applies_events_and_broadcasts_fills() {
        let r = rig().await;
        let mut events = r.bus.subscribe();
        let (tx, rx) = mpsc::channel(8);
        tokio::spawn(pump(rx, r.broker.clone(), r.bus.clone()));
        tx.send(MarketEvent::Book(book(&r.clock))).await.unwrap();
        assert!(matches!(events.recv().await.unwrap(), BusEvent::Market(MarketEvent::Book(_))));
        let bid = OrderRequest {
            instrument: btc(),
            side: Side::Buy,
            kind: OrderType::Limit,
            size: Size::Qty(dec!(0.1)),
            limit_price: Some(dec!(99998000)),
            tif: Tif::Gtc,
            reason: "test".into(),
        };
        r.broker.place_sync("a", bid).unwrap();
        let t = Trade { instrument: btc(), price: dec!(99990000), qty: dec!(1), at: r.clock.now() };
        tx.send(MarketEvent::Trade(t)).await.unwrap();
        assert!(matches!(events.recv().await.unwrap(), BusEvent::Market(MarketEvent::Trade(_))));
        let BusEvent::Fill(f) = events.recv().await.unwrap() else { panic!("expected a fill") };
        assert_eq!(f.qty, dec!(0.1));
    }
}
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test --lib market::`
Expected: compile errors.

- [ ] **Step 3: Implement** (prepend to `src/market.rs`)

```rust
//! Wires feeds to the broker: an event pump, and a facade that keeps instruments loaded,
//! subscribed and fresh.

use std::collections::HashMap;
use std::sync::Arc;

use anyhow::anyhow;
use chrono::Duration;
use tokio::sync::{broadcast, mpsc, watch};

use crate::broker::{Fill, SimBroker};
use crate::domain::{InstrumentId, Venue};
use crate::feed::{MarketEvent, MarketFeed};
use crate::sim::{DailyStats, SimParams};
use crate::subs::Subscriptions;

/// Everything downstream consumers (SSE, alerts, persistence) listen to.
#[derive(Debug, Clone)]
pub enum BusEvent {
    Market(MarketEvent),
    Fill(Fill),
}

/// Apply each market event to the broker, then broadcast it and any fills it caused.
pub async fn pump(mut rx: mpsc::Receiver<MarketEvent>, broker: Arc<SimBroker>, bus: broadcast::Sender<BusEvent>) {
    while let Some(ev) = rx.recv().await {
        let fills = match &ev {
            MarketEvent::Book(b) => broker.on_book(b.clone()),
            MarketEvent::Trade(t) => broker.on_trade(t.clone()),
        };
        let _ = bus.send(BusEvent::Market(ev));
        for f in fills {
            let _ = bus.send(BusEvent::Fill(f));
        }
    }
}

struct VenueFeed {
    feed: Arc<dyn MarketFeed>,
    subs: Subscriptions,
}

pub struct Market {
    broker: Arc<SimBroker>,
    bus: broadcast::Sender<BusEvent>,
    venues: HashMap<Venue, VenueFeed>,
}

impl Market {
    pub fn new(broker: Arc<SimBroker>, bus: broadcast::Sender<BusEvent>) -> Self {
        Market { broker, bus, venues: HashMap::new() }
    }

    /// Register a feed. Hand the returned receiver to `feed::run_feed`.
    pub fn add_feed(&mut self, feed: Arc<dyn MarketFeed>, cap: usize) -> watch::Receiver<Vec<InstrumentId>> {
        let (subs, rx) = Subscriptions::new(cap);
        self.venues.insert(feed.venue(), VenueFeed { feed, subs });
        rx
    }

    /// Load every feed's instrument list into the broker. Returns how many were offered.
    pub async fn load_instruments(&self) -> anyhow::Result<usize> {
        let mut n = 0;
        for vf in self.venues.values() {
            for i in vf.feed.instruments().await? {
                self.broker.add_instrument(i);
                n += 1;
            }
        }
        Ok(n)
    }

    /// Make sure the broker has daily stats and a book at most 2 s old for `id`, and keep it
    /// subscribed. Fills caused by a fetched snapshot go out on the bus.
    pub async fn ensure_fresh(&self, id: &InstrumentId) -> anyhow::Result<()> {
        let vf = self.venues.get(&id.venue).ok_or_else(|| anyhow!("no feed for venue {}", id.venue.tag()))?;
        vf.subs.touch(id);
        if !self.broker.has_stats(id) {
            let stats = vf.feed.daily_stats(id).await.unwrap_or_else(|e| {
                tracing::warn!(instrument = %id, error = %e, "daily stats unavailable; using defaults");
                let p = SimParams::default_for(id.venue);
                DailyStats { sigma: p.default_sigma, adv_notional: p.default_adv }
            });
            self.broker.set_stats(id.clone(), stats);
        }
        let fresh = self.broker.book_age(id).is_some_and(|age| age <= Duration::seconds(2));
        if !fresh {
            let book = vf.feed.snapshot(id).await?;
            for f in self.broker.on_book(book) {
                let _ = self.bus.send(BusEvent::Fill(f));
            }
        }
        Ok(())
    }

    /// Pin every instrument with a position or resting order to its venue's subscription set.
    /// Call after fills and on a timer.
    pub fn refresh_pins(&self) {
        let active = self.broker.active_instruments();
        for (venue, vf) in &self.venues {
            vf.subs.pin(active.iter().filter(|i| i.venue == *venue).cloned().collect());
        }
    }
}
```

Add `pub mod market;` to `src/lib.rs`.

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test --lib market::`
Expected: 4 passed.

- [ ] **Step 5: Commit**

```bash
git add src/lib.rs src/market.rs
git commit -m "Add market event pump and Market facade"
git log -1 --format=%B
```

---

### Task 9: Live smoke tests

**Files:**
- Create: `tests/live.rs`

**Interfaces:**
- Consumes: `UpbitFeed`, `BinanceFeed`, `FxCache`, `MarketFeed`, `SystemClock`.

- [ ] **Step 1: Write the tests**

```rust
//! Network smoke tests against the real venues. Run: `cargo test --test live -- --ignored`.

use std::sync::Arc;
use std::time::Duration;

use atrader::domain::{InstrumentId, SystemClock};
use atrader::feed::binance::BinanceFeed;
use atrader::feed::upbit::UpbitFeed;
use atrader::feed::{MarketEvent, MarketFeed};
use atrader::fx::FxCache;
use tokio::sync::mpsc;

async fn check(feed: Arc<dyn MarketFeed>, id: InstrumentId) {
    let instruments = feed.instruments().await.unwrap();
    let inst = instruments.iter().find(|i| i.id == id).expect("instrument listed").clone();
    let book = feed.snapshot(&id).await.unwrap();
    assert!(!book.asks.is_empty() && !book.bids.is_empty());
    assert!(inst.tick.is_valid(book.asks[0].price), "best ask {} off tick", book.asks[0].price);
    assert!(feed.daily_stats(&id).await.unwrap().sigma > 0.0);

    let (tx, mut rx) = mpsc::channel(256);
    let streaming = feed.clone();
    let ids = vec![id.clone()];
    tokio::spawn(async move { streaming.stream(&ids, &tx).await });
    let (mut books, mut trades) = (0, 0);
    let deadline = tokio::time::Instant::now() + Duration::from_secs(20);
    while (books == 0 || trades == 0) && tokio::time::Instant::now() < deadline {
        match tokio::time::timeout(Duration::from_secs(20), rx.recv()).await {
            Ok(Some(MarketEvent::Book(b))) => {
                assert_eq!(b.instrument, id);
                books += 1;
            }
            Ok(Some(MarketEvent::Trade(_))) => trades += 1,
            _ => break,
        }
    }
    assert!(books > 0, "no books streamed");
    assert!(trades > 0, "no trades streamed");
}

#[tokio::test]
#[ignore]
async fn upbit_live() {
    check(Arc::new(UpbitFeed::new(Arc::new(SystemClock))), "UPBIT:KRW-BTC".parse().unwrap()).await;
}

#[tokio::test]
#[ignore]
async fn binance_live() {
    check(Arc::new(BinanceFeed::new(Arc::new(SystemClock))), "BINANCE:BTCUSDT".parse().unwrap()).await;
}

#[tokio::test]
#[ignore]
async fn fx_live() {
    let rate = FxCache::new(reqwest::Client::new()).usd_krw().await.unwrap();
    assert!(rate > rust_decimal::Decimal::from(500) && rate < rust_decimal::Decimal::from(5000), "rate {rate}");
}
```

- [ ] **Step 2: Run them**

Run: `cargo test --test live -- --ignored`
Expected: 3 passed.

If rustls panics with "no process-level CryptoProvider", two TLS providers were linked in. Install `ring` as the default once in the library, with a `pub fn init_tls()` called from `UpbitFeed::new` and `BinanceFeed::new` through `std::sync::Once`, and record a ruling.

If a live check fails because the venue itself behaves differently from what this plan assumes, fix the parser, add the real payload as a unit-test fixture, and record a ruling.

- [ ] **Step 3: Run the full suite, then commit**

Run: `cargo test` with `DATABASE_URL` exported.
Expected: everything passes, and the live tests are reported as ignored.

```bash
git add tests/live.rs
git commit -m "Add live smoke tests for Upbit, Binance and FX"
git log -1 --format=%B
```
