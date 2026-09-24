# ATrader Phase 3 (zyris `trader` Capability and Process Wiring) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** An `atrader` binary that trades Upbit and Binance on paper for an Attacca agent. It serves the zyris `trader` capability (discovery, quotes, order book, estimate/place/cancel, history, account, positions, currency conversion), persists everything it changes, survives restarts, and gives the user a CLI to create, list and reset accounts.

**Architecture:**
- **Journal.** `SimBroker` emits a `Journal` event (order, fill or conversion) for every state change into an unbounded channel. A `persist` task writes each event to Postgres, retrying on failure, and then republishes it on the `BusEvent` channel, so persistence and the future SSE/alert consumers see one ordered stream.
- **App.** `App` bundles the broker, store, market and FX rate.
- **Tools.** `TraderTools` implements the `#[zyris::capability(name = "trader", version = 1)]` trait by translating DTOs into broker and store calls.
- **CLI.** `cli` parses commands and runs either account maintenance or `serve`. `serve` starts the feeds, pump, persister, timers and the zyris `Link`, which reconnects on its own.

**Tech Stack:**
- zyris (git `attacca-cc/zyris-protocol`, rev `4614f0ea16f646408df590c0b249bd34c7a05d69`, feature `tls-ring`)
- schemars 1.2.2 (features `chrono04` and `rust_decimal1`)
- tracing-subscriber
- Everything from Phases 1 and 2.

**Spec:** `docs/superpowers/specs/2026-09-24-atrader-design.md`. This plan covers §15 step 3 and implements the discovery, quote, trading and account rows of §7, and parts of §9, §12 and §13. Candles, indicators, the screener and fundamentals are Phase 5. `get_performance` is Phase 5, which introduces equity snapshots. Alerts are Phase 6.

## Global Constraints

- Earlier phases' constraints still apply: `Decimal` for money, English text, no Claude attribution in commits, and `ponytail:` comments on shortcuts.
- The capability is named `trader` with version `1`. Doc comments on tool methods and DTO fields are the text the model reads, so they must be concrete about units, currency and what to call next.
- Only accounts with an `agent_id` are visible to or usable by tools. Any other account id returns error code `UNKNOWN_ACCOUNT`.
- Tool errors use `zyris::Error::new(ErrorCode::Other(CODE), message).with_data(json)`, with these codes: `MARKET_CLOSED`, `STALE_DATA`, `NO_LIQUIDITY`, `INVALID_TICK`, `INVALID_QTY`, `PRICE_LIMIT`, `INSUFFICIENT_FUNDS`, `INSUFFICIENT_POSITION`, `UNKNOWN_INSTRUMENT`, `UNKNOWN_ACCOUNT`, `NOT_TRADABLE`, `NOT_FOUND`, `INVALID_REQUEST`, `UPSTREAM_ERROR`. Malformed arguments use `ErrorCode::InvalidParams`. Only `UPSTREAM_ERROR` is retriable.
- `reason` is required on `place_order` and must not be blank.
- Default TIF: market orders are `ioc`. Limit orders are `day` on stock venues and `gtc` on crypto.
- A quote is `stale` when its book is more than 5 s old. `get_quotes` accepts at most 20 ids, and `get_orderbook` has depth 10 by default and 30 at most.
- History limits default to 50 and are clamped to between 1 and 200.
- FX spread is 0.1%. Valuation is in KRW, with USDT counted as USD.
- Configuration comes from the environment:
  - `DATABASE_URL`, required.
  - `ZYRIS_SERVER_URL`, defaulting to `zyris::DEFAULT_SERVER_URL`.
  - The credential, from `ZYRIS_CREDENTIAL` or else `ZYRIS_CREDENTIAL_FILE`.
  - `ATRADER_NODE_NAME`, defaulting to `atrader`.
  - `RUST_LOG`, defaulting to `atrader=info,zyris_core=info`.
- Default cash for `account create` without `--cash` is `KRW=10000000`, `USD=7000` and `USDT=7000`.
- Deliberate deviation from spec §13, recorded as a ruling and also written into the spec: a failed journal write is retried up to 10 times with backoff and then logged with the full event. It is not reversed in memory.

## Review Focus

1. **Every state change must reach Postgres in order.** This covers a place with fills, a resting fill, a cancel, an expiry and a conversion. Fill rows must never precede their order row, and a slow or failing database must not lose events. Tasks 1 and 3 test this.
2. **A restart must reproduce the account.** Cash (including FX), positions, resting orders and their reservations must be restored, and new order ids must continue past the highest stored id. Task 6 tests this.
3. **An agent must not reach accounts it does not own.** Every tool that takes an account must refuse an account without an `agent_id` using `UNKNOWN_ACCOUNT`. Task 5 tests this.
4. **Every OrderError must reach the agent with its recovery fields.** A tool error must carry its code and fields in data (for example `MARKET_CLOSED` with `next_open` and `INSUFFICIENT_FUNDS` with `available`), never a bare string. Task 4 tests this.
5. **Nothing the agent sends may panic the node.** A malformed id, both or neither of qty and notional, an empty reason, a huge depth or an unknown currency must each return an error. Task 4 tests this.

## File Structure

| File | Responsibility |
| --- | --- |
| `src/broker.rs` | Modified: `Journal`, `with_journal`, last trades, `BookView`, `book_view`, `search`, `instrument`, `market_open`, `now`, `restore_account`, `restore_order`, a monotonic `set_next_order_id` |
| `src/store.rs` | Modified: `AccountRow`, `list_accounts`, `orders`, `fills` (row → domain parsing) |
| `src/persist.rs` | `persist(rx, store, bus)`: the ordered, retrying journal writer |
| `src/market.rs` | Modified: `BusEvent::Order`; the pump no longer broadcasts fills |
| `src/fx.rs` | Modified: `FxCache::fixed(rate)` for tests |
| `src/app.rs` | `App`, `krw_per`, `restore` |
| `src/tools/dto.rs` | Tool input and output types |
| `src/tools/mod.rs` | The `Trader` capability trait, `TraderTools`, error mapping |
| `src/cli.rs` | `Command`, `parse`, `run`, `serve` |
| `src/main.rs` | Calls `cli::run` |
| `tests/tools.rs` | Tool tests against Postgres with a fake feed |
| `README.md`, `CLAUDE.md`, spec §13 | Run instructions; the persistence ruling |

---

### Task 1: Broker journal, read views and restore

**Files:**
- Modify: `src/broker.rs`, `tests/broker.rs`, `Cargo.toml`

**Interfaces:**
- Produces:
  - `enum Journal { Order(Order), Fill(Fill), Conversion { account: AccountId, conversion: Conversion, at: DateTime<Utc> } }`.
  - `SimBroker::with_journal(self, mpsc::UnboundedSender<Journal>) -> Self`.
  - `BookView { instrument, shadow_bids, shadow_asks, real_bids, real_asks: Vec<Level>, offset: f64, received_at, last_trade: Option<(Decimal, DateTime<Utc>)> }`.
  - These methods on `SimBroker`:
    - `book_view(&InstrumentId, depth: usize) -> Option<BookView>`
    - `search(query: &str, venue: Option<Venue>, limit: usize) -> Vec<Instrument>`
    - `instrument(&InstrumentId) -> Option<Instrument>`
    - `market_open(Venue) -> (bool, Option<DateTime<Utc>>)`
    - `now() -> DateTime<Utc>`
    - `restore_account(&str, Portfolio)`
    - `restore_order(Order) -> Result<(), OrderError>`
  - `set_next_order_id` now only moves forward.
- Journal order: `place_sync` emits `Order` and then each `Fill`. A resting fill emits `Order` (the updated order) and then `Fill`. Cancel and expire emit `Order`, and `convert_sync` emits `Conversion`.

- [ ] **Step 1: Write the failing tests** (append to `tests/broker.rs`)

```rust
fn journaled() -> (SimBroker, ManualClock, tokio::sync::mpsc::UnboundedReceiver<Journal>) {
    let (b, clock) = setup();
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
    (b.with_journal(tx), clock, rx)
}

fn drain(rx: &mut tokio::sync::mpsc::UnboundedReceiver<Journal>) -> Vec<Journal> {
    std::iter::from_fn(|| rx.try_recv().ok()).collect()
}

#[test]
fn journal_records_every_change_in_order() {
    let (b, clock, mut rx) = journaled();
    let (order, fills) = b.place_sync("a", market_buy(dec!(0.1))).unwrap();
    assert_eq!(drain(&mut rx), vec![Journal::Order(order.clone()), Journal::Fill(fills[0].clone())]);

    let (resting, _) = b.place_sync("a", limit(btc(), Side::Buy, dec!(0.1), dec!(99998000), Tif::Gtc)).unwrap();
    drain(&mut rx);
    let f = b.on_trade(trade(&clock, btc(), dec!(99990000), dec!(1)));
    let j = drain(&mut rx);
    assert!(matches!(&j[0], Journal::Order(o) if o.id == resting.id && o.status == OrderStatus::Filled));
    assert_eq!(j[1], Journal::Fill(f[0].clone()));

    let (open, _) = b.place_sync("a", limit(btc(), Side::Buy, dec!(0.1), dec!(99998000), Tif::Gtc)).unwrap();
    drain(&mut rx);
    b.cancel_sync("a", open.id).unwrap();
    assert!(matches!(&drain(&mut rx)[..], [Journal::Order(o)] if o.status == OrderStatus::Cancelled));

    let c = b.convert_sync("a", Currency::Krw, Currency::Usd, dec!(1365350), dec!(1365.35), dec!(0.001)).unwrap();
    assert!(matches!(&drain(&mut rx)[..], [Journal::Conversion { account, conversion, .. }] if account == "a" && *conversion == c));
}

#[test]
fn book_view_shows_shadow_real_and_last_trade() {
    let (b, clock) = setup();
    b.on_trade(trade(&clock, btc(), dec!(100000000), dec!(0.01)));
    b.place_sync("a", market_buy(dec!(0.5))).unwrap();
    let v = b.book_view(&btc(), 5).unwrap();
    assert_eq!(v.real_asks[0].price, dec!(100000000));
    assert!(v.shadow_asks[0].price > dec!(100000000)); // first level used up, the rest pushed up
    assert!(v.offset > 0.0);
    assert_eq!(v.last_trade.map(|t| t.0), Some(dec!(100000000)));
    assert!(b.book_view(&"UPBIT:KRW-XRP".parse().unwrap(), 5).is_none());
}

#[test]
fn search_matches_symbol_or_name() {
    let (b, _) = setup();
    assert_eq!(b.search("btc", None, 10)[0].id, btc());
    assert_eq!(b.search("samsung", None, 10)[0].id, samsung());
    assert_eq!(b.search("005930", Some(Venue::Krx), 10).len(), 1);
    assert!(b.search("005930", Some(Venue::Upbit), 10).is_empty());
    assert_eq!(b.search("", None, 10).len(), 0);
}

#[test]
fn restored_orders_reserve_and_fill_again() {
    let (b, clock) = setup();
    let (order, _) = b.place_sync("a", limit(btc(), Side::Buy, dec!(0.1), dec!(99998000), Tif::Gtc)).unwrap();
    let pf = b.portfolio("a").unwrap();

    let (fresh, _) = setup();
    let mut unreserved = pf.clone();
    unreserved.reserved_cash.clear();
    fresh.restore_account("a", unreserved);
    fresh.restore_order(order.clone()).unwrap();
    assert_eq!(fresh.portfolio("a").unwrap().available_cash(Currency::Krw), pf.available_cash(Currency::Krw));
    let f = fresh.on_trade(trade(&clock, btc(), dec!(99990000), dec!(1)));
    assert_eq!(f[0].order_id, order.id);
    fresh.set_next_order_id(1); // never moves backwards
    let (next, _) = fresh.place_sync("a", market_buy(dec!(0.1))).unwrap();
    assert!(next.id > order.id);
}
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test --test broker`
Expected: compile errors (`Journal`, `with_journal`, `book_view` and `search` not found).

- [ ] **Step 3: Implement**

Run `cargo add tokio --features rt-multi-thread,macros,sync,time,signal` (the `signal` feature is for `serve` later).

In `src/broker.rs`:

1. Imports: add `use tokio::sync::mpsc;`.
2. Add this after `Conversion`:

```rust
/// Every change the broker makes that must be persisted, in the order it happened.
#[derive(Debug, Clone, PartialEq)]
pub enum Journal {
    Order(Order),
    Fill(Fill),
    Conversion { account: AccountId, conversion: Conversion, at: DateTime<Utc> },
}

/// The shadow book next to the real one, for quotes and order-book tools.
#[derive(Debug, Clone, PartialEq)]
pub struct BookView {
    pub instrument: InstrumentId,
    pub shadow_bids: Vec<Level>,
    pub shadow_asks: Vec<Level>,
    pub real_bids: Vec<Level>,
    pub real_asks: Vec<Level>,
    pub offset: f64,
    pub received_at: DateTime<Utc>,
    pub last_trade: Option<(Decimal, DateTime<Utc>)>,
}
```

3. Add `last_trades: HashMap<InstrumentId, (Decimal, DateTime<Utc>)>` to `World`.
4. Add `journal: Option<mpsc::UnboundedSender<Journal>>` to `SimBroker`, set it to `None` in `new`, and add:

```rust
    /// Send every persisted-state change to `tx` (see `Journal`).
    pub fn with_journal(mut self, tx: mpsc::UnboundedSender<Journal>) -> Self {
        self.journal = Some(tx);
        self
    }

    fn emit(&self, j: Journal) {
        if let Some(tx) = &self.journal {
            let _ = tx.send(j);
        }
    }
```

5. Emission points:
   - In `place_sync`, after `w.orders.insert(id, order.clone());`: `self.emit(Journal::Order(order.clone())); for f in &fills { self.emit(Journal::Fill(f.clone())); }`.
   - In `on_book` and in `on_trade`, replace `fills.push(record_fill(...));` with `let fill = record_fill(...);`. After `w.orders.insert(r.order_id, order...)`, emit `Order`, then `Fill`, then push:

```rust
            let fill = record_fill(w, &mut order, qty, qty * r.price, Liquidity::Maker, now);
            if r.remaining.is_zero() {
                order.status = OrderStatus::Filled;
            }
            self.emit(Journal::Order(order.clone()));
            self.emit(Journal::Fill(fill.clone()));
            fills.push(fill);
            w.orders.insert(r.order_id, order);
```

   (In `on_trade`, use `q` in place of `qty`.)
   - `close_order` becomes a method `fn close_order(&self, w: &mut World, order_id, status) -> Order` that emits `Journal::Order(order.clone())` before returning. Update its two callers to `self.close_order(w, ...)`.
   - In `convert_sync`, before `Ok(...)`: build the `Conversion` into `let c = ...;`, call `self.emit(Journal::Conversion { account: account.to_string(), conversion: c.clone(), at: self.clock.now() });`, then return `Ok(c)`.
6. In `on_trade`, right after the price/qty sanity check and before the `is_open` check, record the print: `w.last_trades.insert(id.clone(), (trade.price, trade.at));`. Split the combined `if` so that sane prints are recorded even when the market is closed:

```rust
        if trade.price <= Decimal::ZERO || trade.qty <= Decimal::ZERO {
            return Vec::new();
        }
        w.last_trades.insert(id.clone(), (trade.price, trade.at));
        if !self.calendar.is_open(id.venue, now) {
            return Vec::new();
        }
```

7. Add the read and restore methods to `impl SimBroker`:

```rust
    pub fn now(&self) -> DateTime<Utc> {
        self.clock.now()
    }

    pub fn instrument(&self, id: &InstrumentId) -> Option<Instrument> {
        self.world.lock().unwrap().instruments.get(id).cloned()
    }

    /// Whether `venue` is open now, and when it next opens (`None` for 24/7 venues).
    pub fn market_open(&self, venue: Venue) -> (bool, Option<DateTime<Utc>>) {
        let now = self.clock.now();
        (self.calendar.is_open(venue, now), self.calendar.next_open(venue, now))
    }

    /// Instruments whose symbol or name contains `query` (case-insensitive); exact symbols first.
    pub fn search(&self, query: &str, venue: Option<Venue>, limit: usize) -> Vec<Instrument> {
        let q = query.trim().to_lowercase();
        if q.is_empty() {
            return Vec::new();
        }
        let w = self.world.lock().unwrap();
        let mut hits: Vec<&Instrument> = w
            .instruments
            .values()
            .filter(|i| venue.is_none_or(|v| i.id.venue == v))
            .filter(|i| i.id.symbol.to_lowercase().contains(&q) || i.name.to_lowercase().contains(&q))
            .collect();
        hits.sort_by_key(|i| (!i.id.symbol.to_lowercase().ends_with(&q), i.id.symbol.len(), i.id.to_string()));
        hits.into_iter().take(limit).cloned().collect()
    }

    /// The shadow and real books to `depth` levels, decayed to now.
    pub fn book_view(&self, id: &InstrumentId, depth: usize) -> Option<BookView> {
        let now = self.clock.now();
        let mut guard = self.world.lock().unwrap();
        let w = &mut *guard;
        let book = w.books.get(id)?;
        let inst = w.instruments.get(id)?;
        let shadow = w.shadows.entry(id.clone()).or_default();
        shadow.decay(now, &SimParams::default_for(id.venue));
        let levels = |v: Vec<sim::ShadowLevel>| v.into_iter().take(depth).map(|l| Level { price: l.price, qty: l.qty }).collect();
        Some(BookView {
            instrument: id.clone(),
            shadow_bids: levels(shadow.shadow_side(&book.bids, Side::Buy, &inst.tick)),
            shadow_asks: levels(shadow.shadow_side(&book.asks, Side::Sell, &inst.tick)),
            real_bids: book.bids.iter().take(depth).copied().collect(),
            real_asks: book.asks.iter().take(depth).copied().collect(),
            offset: shadow.offset,
            received_at: book.received_at,
            last_trade: w.last_trades.get(id).copied(),
        })
    }

    /// Replace an account's portfolio with one rebuilt from the store.
    pub fn restore_account(&self, id: &str, portfolio: Portfolio) {
        self.world.lock().unwrap().accounts.insert(id.to_string(), portfolio);
    }

    /// Put a persisted open limit order back on the book, reserving what it still needs. Its
    /// queue position restarts at zero.
    pub fn restore_order(&self, order: Order) -> Result<(), OrderError> {
        let (Size::Qty(qty), Some(price), OrderStatus::Open) = (order.req.size, order.req.limit_price, order.status) else {
            return Err(OrderError::InvalidRequest(format!("order {} is not an open limit order", order.id)));
        };
        let remaining = qty - order.filled_qty;
        let mut guard = self.world.lock().unwrap();
        let w = &mut *guard;
        let venue = order.req.instrument.venue;
        let pf = w.accounts.get_mut(&order.account).ok_or(OrderError::UnknownAccount)?;
        match order.req.side {
            Side::Buy => pf.reserve_cash(venue.currency(), buy_reserve_per_unit(price, venue) * remaining),
            Side::Sell => pf.reserve_qty(&order.req.instrument, remaining),
        }
        w.resting.entry(order.req.instrument.clone()).or_default().push(Resting {
            order_id: order.id,
            side: order.req.side,
            price,
            remaining,
            queue_ahead: Decimal::ZERO,
        });
        w.next_order_id = w.next_order_id.max(order.id + 1);
        w.orders.insert(order.id, order);
        Ok(())
    }
```

8. Change `set_next_order_id` so it only moves forward: `let mut w = ...; w.next_order_id = w.next_order_id.max(id);`.

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test --lib --test broker`
Expected: all pass, including 4 new broker tests.

- [ ] **Step 5: Commit**

```bash
git add src/broker.rs tests/broker.rs Cargo.toml Cargo.lock
git commit -m "Add broker journal, book views, search and order restore"
git log -1 --format=%B
```

---

### Task 2: Store reads (accounts, orders, fills)

**Files:**
- Modify: `src/store.rs`, `tests/store.rs`

**Interfaces:**
- Produces:
  - `AccountRow { id, name, agent_id: Option<String>, generation: i32 }`.
  - These methods on `Store`:
    - `list_accounts() -> sqlx::Result<Vec<AccountRow>>`, ordered by id.
    - `orders(account, generation, open_only: bool, limit: i64) -> sqlx::Result<Vec<Order>>`, newest first.
    - `fills(account, generation, since: Option<DateTime<Utc>>, limit: i64) -> sqlx::Result<Vec<Fill>>`, newest first.

- [ ] **Step 1: Write the failing tests** (append to `tests/store.rs`)

```rust
fn micros() -> chrono::DateTime<Utc> {
    use chrono::SubsecRound;
    Utc::now().trunc_subsecs(6)
}

#[sqlx::test]
async fn orders_and_fills_round_trip(pool: PgPool) {
    let store = Store::new(pool);
    store.create_account("a", "Test", Some("agent-1"), &[(Currency::Krw, dec!(1000000))], Utc::now()).await.unwrap();
    store.create_account("b", "Manual", None, &[(Currency::Krw, dec!(1))], Utc::now()).await.unwrap();
    assert_eq!(
        store.list_accounts().await.unwrap(),
        vec![
            AccountRow { id: "a".into(), name: "Test".into(), agent_id: Some("agent-1".into()), generation: 1 },
            AccountRow { id: "b".into(), name: "Manual".into(), agent_id: None, generation: 1 },
        ]
    );

    let mut filled = order(1);
    filled.created_at = micros();
    let mut open = order(2);
    open.req.kind = OrderType::Limit;
    open.req.limit_price = Some(dec!(69000));
    open.req.tif = Tif::Day;
    open.status = OrderStatus::Open;
    open.filled_qty = dec!(0);
    open.filled_notional = dec!(0);
    open.created_at = micros();
    store.save_order(&filled, 1).await.unwrap();
    store.save_order(&open, 1).await.unwrap();
    assert_eq!(store.orders("a", 1, true, 10).await.unwrap(), vec![open.clone()]);
    assert_eq!(store.orders("a", 1, false, 10).await.unwrap(), vec![open, filled]);

    let mut f = fill(1, Side::Buy, dec!(10), dec!(700000), dec!(105), dec!(0));
    f.at = micros();
    store.save_fill(&f, 1).await.unwrap();
    assert_eq!(store.fills("a", 1, None, 10).await.unwrap(), vec![f.clone()]);
    assert!(store.fills("a", 1, Some(f.at + chrono::Duration::seconds(1)), 10).await.unwrap().is_empty());
}
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test --test store` (with `DATABASE_URL` set)
Expected: compile errors (`AccountRow`, `list_accounts`, `orders` and `fills` not found).

- [ ] **Step 3: Implement** (in `src/store.rs`)

Change the broker import to `use crate::broker::{Conversion, Fill, Liquidity, Order, OrderRequest, OrderStatus, OrderType, Tif};`, then add:

```rust
#[derive(Debug, Clone, PartialEq)]
pub struct AccountRow {
    pub id: String,
    pub name: String,
    /// The Attacca agent that trades this account; `None` hides it from tools.
    pub agent_id: Option<String>,
    pub generation: i32,
}

fn parse_order(r: &sqlx::postgres::PgRow) -> sqlx::Result<Order> {
    let text = |col: &str| -> String { r.get(col) };
    let bad = |col: &str, v: String| decode_err(format!("unknown {col} {v:?}"));
    let side = Side::from_code(&text("side")).ok_or_else(|| bad("side", text("side")))?;
    let kind = match text("kind").as_str() {
        "market" => OrderType::Market,
        "limit" => OrderType::Limit,
        v => return Err(bad("kind", v.into())),
    };
    let tif = match text("tif").as_str() {
        "day" => Tif::Day,
        "gtc" => Tif::Gtc,
        "ioc" => Tif::Ioc,
        v => return Err(bad("tif", v.into())),
    };
    let status = match text("status").as_str() {
        "open" => OrderStatus::Open,
        "filled" => OrderStatus::Filled,
        "cancelled" => OrderStatus::Cancelled,
        "expired" => OrderStatus::Expired,
        v => return Err(bad("status", v.into())),
    };
    let size = match (r.get::<Option<Decimal>, _>("qty"), r.get::<Option<Decimal>, _>("notional")) {
        (Some(q), _) => Size::Qty(q),
        (None, Some(n)) => Size::Notional(n),
        (None, None) => return Err(decode_err("order without qty or notional".into())),
    };
    Ok(Order {
        id: r.get::<i64, _>("id") as u64,
        account: text("account_id"),
        req: OrderRequest {
            instrument: text("instrument").parse().map_err(decode_err)?,
            side,
            kind,
            size,
            limit_price: r.get("limit_price"),
            tif,
            reason: text("reason"),
        },
        status,
        filled_qty: r.get("filled_qty"),
        filled_notional: r.get("filled_notional"),
        created_at: r.get("created_at"),
    })
}

fn parse_fill(r: &sqlx::postgres::PgRow) -> sqlx::Result<Fill> {
    let side_code: String = r.get("side");
    let liquidity = match r.get::<String, _>("liquidity").as_str() {
        "taker" => Liquidity::Taker,
        "maker" => Liquidity::Maker,
        v => return Err(decode_err(format!("unknown liquidity {v:?}"))),
    };
    Ok(Fill {
        order_id: r.get::<i64, _>("order_id") as u64,
        account: r.get("account_id"),
        instrument: r.get::<String, _>("instrument").parse().map_err(decode_err)?,
        side: Side::from_code(&side_code).ok_or_else(|| decode_err(format!("unknown side {side_code}")))?,
        qty: r.get("qty"),
        notional: r.get("notional"),
        price: r.get("price"),
        fee: r.get("fee"),
        tax: r.get("tax"),
        realized_pnl: r.get("realized_pnl"),
        liquidity,
        at: r.get("at"),
    })
}
```

Add these methods to `impl Store`:

```rust
    pub async fn list_accounts(&self) -> sqlx::Result<Vec<AccountRow>> {
        let rows = sqlx::query("SELECT id, name, agent_id, generation FROM accounts ORDER BY id").fetch_all(&self.pool).await?;
        Ok(rows
            .iter()
            .map(|r| AccountRow { id: r.get("id"), name: r.get("name"), agent_id: r.get("agent_id"), generation: r.get("generation") })
            .collect())
    }

    /// Newest first.
    pub async fn orders(&self, account: &str, generation: i32, open_only: bool, limit: i64) -> sqlx::Result<Vec<Order>> {
        let rows = sqlx::query(
            "SELECT * FROM orders WHERE account_id = $1 AND generation = $2 AND (NOT $3 OR status = 'open')
             ORDER BY id DESC LIMIT $4",
        )
        .bind(account)
        .bind(generation)
        .bind(open_only)
        .bind(limit)
        .fetch_all(&self.pool)
        .await?;
        rows.iter().map(parse_order).collect()
    }

    /// Newest first; `since` is exclusive.
    pub async fn fills(&self, account: &str, generation: i32, since: Option<DateTime<Utc>>, limit: i64) -> sqlx::Result<Vec<Fill>> {
        let rows = sqlx::query(
            "SELECT * FROM fills WHERE account_id = $1 AND generation = $2 AND ($3::timestamptz IS NULL OR at > $3)
             ORDER BY id DESC LIMIT $4",
        )
        .bind(account)
        .bind(generation)
        .bind(since)
        .bind(limit)
        .fetch_all(&self.pool)
        .await?;
        rows.iter().map(parse_fill).collect()
    }
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test --test store`
Expected: 5 passed.

- [ ] **Step 5: Commit**

```bash
git add src/store.rs tests/store.rs
git commit -m "Add store reads for accounts, orders and fills"
git log -1 --format=%B
```

---

### Task 3: Journal persister and bus republishing

**Files:**
- Create: `src/persist.rs`
- Modify: `src/market.rs` (add `BusEvent::Order`; `pump` stops sending fills), `src/lib.rs`, `tests/store.rs`

**Interfaces:**
- Consumes:
  - `Journal` (Task 1).
  - `Store::{generation, save_order, save_fill, save_conversion}`.
  - `BusEvent`.
- Produces: `persist(rx: mpsc::UnboundedReceiver<Journal>, store: Arc<Store>, bus: broadcast::Sender<BusEvent>)`, an async function. `BusEvent` gains `Order(Order)`. Fills reach the bus only through `persist`.

- [ ] **Step 1: Write the failing test** (append to `tests/store.rs`)

```rust
#[sqlx::test]
async fn persister_writes_in_order_then_republishes(pool: PgPool) {
    use atrader::market::BusEvent;
    use atrader::persist::persist;
    use std::sync::Arc;
    let store = Arc::new(Store::new(pool));
    store.create_account("a", "Test", None, &[(Currency::Krw, dec!(2000000))], Utc::now()).await.unwrap();
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
    let (bus, mut events) = tokio::sync::broadcast::channel(16);
    tx.send(Journal::Order(order(1))).unwrap();
    tx.send(Journal::Fill(fill(1, Side::Buy, dec!(10), dec!(700000), dec!(105), dec!(0)))).unwrap();
    let c = Conversion { from: Currency::Krw, to: Currency::Usd, debit: dec!(1365350), credit: dec!(999), rate: dec!(0.00073167) };
    tx.send(Journal::Conversion { account: "a".into(), conversion: c, at: Utc::now() }).unwrap();
    drop(tx);
    persist(rx, store.clone(), bus).await;

    let cash = store.cash_balances("a", 1).await.unwrap();
    assert_eq!(cash[&Currency::Krw], dec!(2000000) - dec!(700105) - dec!(1365350));
    assert_eq!(cash[&Currency::Usd], dec!(999));
    assert!(matches!(events.recv().await.unwrap(), BusEvent::Order(o) if o.id == 1));
    assert!(matches!(events.recv().await.unwrap(), BusEvent::Fill(f) if f.order_id == 1));
}
```

- [ ] **Step 2: Run the test to verify it fails**

Run: `cargo test --test store persister`
Expected: compile errors (`atrader::persist` and `BusEvent::Order` not found).

- [ ] **Step 3: Implement**

In `src/market.rs`:
- Add `Order(Order)` to `BusEvent` (import `crate::broker::Order`) and document: `/// Orders and fills arrive here only after they are persisted (see persist.rs).`
- In `pump`, drop the fills: `match &ev { MarketEvent::Book(b) => { broker.on_book(b.clone()); } MarketEvent::Trade(t) => { broker.on_trade(t.clone()); } }` and then broadcast only `BusEvent::Market(ev)`.
- In `ensure_fresh`, replace the fill loop with `self.broker.on_book(book);` (the journal carries the fills).
- Update the `pump_applies_events_and_broadcasts_fills` test: rename it to `pump_applies_events_and_broadcasts_market_data`, and replace the final fill assertion with a check on the broker's order status:

```rust
        let (order, _) = r.broker.place_sync("a", bid).unwrap();
        let t = Trade { instrument: btc(), price: dec!(99990000), qty: dec!(1), at: r.clock.now() };
        tx.send(MarketEvent::Trade(t)).await.unwrap();
        assert!(matches!(events.recv().await.unwrap(), BusEvent::Market(MarketEvent::Trade(_))));
        assert_eq!(r.broker.order(order.id).unwrap().status, crate::broker::OrderStatus::Filled);
```

Create `src/persist.rs`:

```rust
//! Writes the broker's journal to Postgres in order, then republishes each event on the bus.

use std::sync::Arc;
use std::time::Duration;

use tokio::sync::{broadcast, mpsc};

use crate::broker::Journal;
use crate::market::BusEvent;
use crate::store::Store;

const ATTEMPTS: u32 = 10;

pub async fn persist(mut rx: mpsc::UnboundedReceiver<Journal>, store: Arc<Store>, bus: broadcast::Sender<BusEvent>) {
    while let Some(j) = rx.recv().await {
        let mut delay = Duration::from_millis(200);
        for attempt in 1..=ATTEMPTS {
            match write(&store, &j).await {
                Ok(()) => break,
                // ponytail: after ATTEMPTS the event is logged and skipped rather than blocking every later write.
                Err(e) if attempt == ATTEMPTS => tracing::error!(error = %e, event = ?j, "journal write failed; event dropped"),
                Err(e) => {
                    tracing::warn!(error = %e, attempt, "journal write failed; retrying");
                    tokio::time::sleep(delay).await;
                    delay = (delay * 2).min(Duration::from_secs(30));
                }
            }
        }
        let event = match j {
            Journal::Order(o) => BusEvent::Order(o),
            Journal::Fill(f) => BusEvent::Fill(f),
            Journal::Conversion { .. } => continue,
        };
        let _ = bus.send(event);
    }
}

async fn write(store: &Store, j: &Journal) -> anyhow::Result<()> {
    match j {
        Journal::Order(o) => store.save_order(o, store.generation(&o.account).await?).await?,
        Journal::Fill(f) => {
            store.save_fill(f, store.generation(&f.account).await?).await?;
        }
        Journal::Conversion { account, conversion, at } => {
            store.save_conversion(account, store.generation(account).await?, conversion, *at).await?
        }
    }
    Ok(())
}
```

Add `pub mod persist;` to `src/lib.rs`.

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test` (with `DATABASE_URL`)
Expected: all pass.

- [ ] **Step 5: Commit**

```bash
git add src/persist.rs src/market.rs src/lib.rs tests/store.rs
git commit -m "Persist the broker journal in order and republish it on the bus"
git log -1 --format=%B
```

---

### Task 4: App, tool DTOs, error mapping and discovery/quote tools

**Files:**
- Create: `src/app.rs`, `src/tools/mod.rs`, `src/tools/dto.rs`, `tests/tools.rs`
- Modify: `src/fx.rs` (`FxCache::fixed`), `src/lib.rs`, `Cargo.toml`

**Interfaces:**
- Produces:
  - `App { broker, store, market, fx, fx_spread }` with `App::new(broker, store, market, fx) -> anyhow::Result<App>`, `reload_accounts`, `agent_accounts() -> Vec<AccountRow>` and `agent_account(&str) -> Result<AccountRow, zyris::Error>`.
  - `krw_per(Currency, usd_krw) -> Decimal`.
  - `restore(&Store, &SimBroker) -> anyhow::Result<usize>`.
  - The `Trader` capability trait (all 13 tools declared here; trading and account tools are implemented in Task 5).
  - `TraderTools::new(Arc<App>)`.
  - `order_error(OrderError) -> zyris::Error`.

- [ ] **Step 1: Add dependencies**

```bash
cargo add zyris --git https://github.com/attacca-cc/zyris-protocol --rev 4614f0ea16f646408df590c0b249bd34c7a05d69 --features tls-ring
cargo add schemars@1.2.2 --features chrono04,rust_decimal1
cargo add tracing-subscriber --features env-filter
cargo build
```

Expected: it builds. If `zyris`'s `rustls` pulls in a second provider, `init_tls` still installs `ring` explicitly, so this is fine.

- [ ] **Step 2: Write the failing tests** (create `tests/tools.rs`)

```rust
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use atrader::app::{App, restore};
use atrader::broker::SimBroker;
use atrader::domain::*;
use atrader::feed::{MarketEvent, MarketFeed};
use atrader::fx::FxCache;
use atrader::market::Market;
use atrader::persist::persist;
use atrader::sim::DailyStats;
use atrader::store::Store;
use atrader::tools::*;
use atrader::venue::{Calendar, Instrument, LotRule, TickRule};
use chrono::{TimeZone, Utc};
use rust_decimal::Decimal;
use rust_decimal_macros::dec;
use sqlx::PgPool;
use tokio::sync::{broadcast, mpsc};
use zyris::ErrorCode;

struct Fake {
    clock: ManualClock,
}

#[async_trait]
impl MarketFeed for Fake {
    fn venue(&self) -> Venue {
        Venue::Upbit
    }
    async fn instruments(&self) -> anyhow::Result<Vec<Instrument>> {
        Ok(vec![Instrument {
            id: "UPBIT:KRW-BTC".parse().unwrap(),
            name: "비트코인 (Bitcoin)".into(),
            tick: TickRule::Upbit,
            lot: LotRule { step: dec!(0.00000001), min_qty: dec!(0.00000001), min_notional: dec!(5000) },
            tradable: true,
        }])
    }
    async fn snapshot(&self, id: &InstrumentId) -> anyhow::Result<Book> {
        Ok(Book {
            instrument: id.clone(),
            bids: vec![Level { price: dec!(99999000), qty: dec!(1) }],
            asks: vec![Level { price: dec!(100000000), qty: dec!(1) }],
            prev_close: None,
            received_at: self.clock.now(),
        })
    }
    async fn daily_stats(&self, _: &InstrumentId) -> anyhow::Result<DailyStats> {
        Ok(DailyStats { sigma: 0.02, adv_notional: dec!(50000000000) })
    }
    async fn stream(&self, _: &[InstrumentId], _: &mpsc::Sender<MarketEvent>) -> anyhow::Result<()> {
        anyhow::bail!("unused")
    }
}

async fn rig(pool: PgPool) -> (Arc<App>, TraderTools) {
    let clock = ManualClock::new(Utc.with_ymd_and_hms(2026, 9, 23, 1, 0, 0).unwrap());
    let store = Arc::new(Store::new(pool));
    store.create_account("bot", "Bot", Some("agent-1"), &[(Currency::Krw, dec!(1000000000))], Utc::now()).await.unwrap();
    store.create_account("manual", "Manual", None, &[(Currency::Krw, dec!(1000000000))], Utc::now()).await.unwrap();
    let (jtx, jrx) = mpsc::unbounded_channel();
    let broker = Arc::new(SimBroker::new(Arc::new(clock.clone()), Calendar::default()).with_journal(jtx));
    restore(&store, &broker).await.unwrap();
    let (bus, _) = broadcast::channel(64);
    let mut market = Market::new(broker.clone());
    let _subs = market.add_feed(Arc::new(Fake { clock }), 10);
    market.load_instruments().await.unwrap();
    tokio::spawn(persist(jrx, store.clone(), bus));
    let app = Arc::new(App::new(broker, store, market, FxCache::fixed(dec!(1400))).await.unwrap());
    (app.clone(), TraderTools::new(app))
}

fn code(e: &zyris::Error) -> String {
    match &e.code {
        ErrorCode::Other(c) => c.clone(),
        other => format!("{other:?}"),
    }
}

fn buy(account: &str, qty: Option<Decimal>, reason: &str) -> OrderInput {
    OrderInput {
        account: account.into(),
        instrument: "UPBIT:KRW-BTC".into(),
        side: SideDto::Buy,
        kind: KindDto::Market,
        qty,
        notional: None,
        limit_price: None,
        tif: None,
        reason: reason.into(),
    }
}

#[sqlx::test]
async fn discovery_and_quotes(pool: PgPool) {
    let (_, t) = rig(pool).await;
    let hits = t.search_instruments("bitcoin".into(), None).await.unwrap();
    assert_eq!(hits[0].id, "UPBIT:KRW-BTC");
    assert_eq!(hits[0].currency, "KRW");
    assert_eq!(t.search_instruments("비트코인".into(), Some("UPBIT".into())).await.unwrap().len(), 1);
    let status = t.market_status().await.unwrap();
    assert_eq!(status.len(), 4);
    assert!(status.iter().any(|s| s.venue == "UPBIT" && s.open));
    let q = t.get_quotes(vec!["UPBIT:KRW-BTC".into()]).await.unwrap();
    assert_eq!((q[0].bid, q[0].ask, q[0].stale), (Some(dec!(99999000)), Some(dec!(100000000)), false));
    let book = t.get_orderbook("UPBIT:KRW-BTC".into(), Some(500)).await.unwrap();
    assert_eq!(book.asks.len(), 1);
}

#[sqlx::test]
async fn malformed_arguments_are_errors_not_panics(pool: PgPool) {
    let (_, t) = rig(pool).await;
    assert_eq!(code(&t.get_quotes(vec!["BTC".into()]).await.unwrap_err()), "UNKNOWN_INSTRUMENT");
    assert!(t.get_quotes((0..21).map(|_| "UPBIT:KRW-BTC".to_string()).collect()).await.is_err());
    assert_eq!(t.search_instruments("x".into(), Some("NYSE".into())).await.unwrap_err().code, ErrorCode::InvalidParams);
    assert_eq!(t.estimate_order(buy("bot", None, "why")).await.unwrap_err().code, ErrorCode::InvalidParams);
    let mut both = buy("bot", Some(dec!(1)), "why");
    both.notional = Some(dec!(1));
    assert_eq!(t.estimate_order(both).await.unwrap_err().code, ErrorCode::InvalidParams);
    assert_eq!(t.place_order(buy("bot", Some(dec!(0.1)), "  ")).await.unwrap_err().code, ErrorCode::InvalidParams);
}

#[sqlx::test]
async fn order_errors_keep_their_code_and_fields(pool: PgPool) {
    let (_, t) = rig(pool).await;
    let mut big = buy("bot", Some(dec!(100)), "too big"); // rests 99 BTC: reservation exceeds cash
    big.kind = KindDto::Limit;
    big.limit_price = Some(dec!(100000000));
    let e = t.place_order(big).await.unwrap_err();
    assert_eq!(code(&e), "INSUFFICIENT_FUNDS");
    let data = serde_json::to_value(e.data.unwrap()).unwrap();
    assert!(data.get("available").is_some(), "data {data}");
    assert_eq!(code(&t.place_order(buy("manual", Some(dec!(0.1)), "not mine")).await.unwrap_err()), "UNKNOWN_ACCOUNT");
    assert_eq!(code(&t.place_order(buy("nobody", Some(dec!(0.1)), "why")).await.unwrap_err()), "UNKNOWN_ACCOUNT");
}

#[test]
fn every_order_error_maps_to_a_code() {
    use atrader::broker::OrderError::*;
    let cases = [
        (MarketClosed { next_open: None }, "MARKET_CLOSED"),
        (StaleData { age_secs: Some(9) }, "STALE_DATA"),
        (NoLiquidity, "NO_LIQUIDITY"),
        (InvalidTick { lower: dec!(1), upper: dec!(2) }, "INVALID_TICK"),
        (InvalidQty { step: dec!(1), min_qty: dec!(1), min_notional: dec!(0) }, "INVALID_QTY"),
        (PriceLimit { lower: dec!(1), upper: dec!(2) }, "PRICE_LIMIT"),
        (InsufficientFunds { required: dec!(2), available: dec!(1) }, "INSUFFICIENT_FUNDS"),
        (InsufficientPosition { available: dec!(1) }, "INSUFFICIENT_POSITION"),
        (UnknownInstrument, "UNKNOWN_INSTRUMENT"),
        (UnknownAccount, "UNKNOWN_ACCOUNT"),
        (NotTradable, "NOT_TRADABLE"),
        (NotFound, "NOT_FOUND"),
        (InvalidRequest("x".into()), "INVALID_REQUEST"),
    ];
    for (e, want) in cases {
        let z = order_error(e);
        assert_eq!(code(&z), want);
        assert!(!z.retriable);
    }
    let _ = Duration::ZERO;
}
```

- [ ] **Step 3: Run the tests to verify they fail**

Run: `cargo test --test tools`
Expected: compile errors (`atrader::app` and `atrader::tools` not found).

- [ ] **Step 4: Implement**

Add this to `src/fx.rs`:

```rust
    /// A cache that always answers `rate` (tests, and offline runs).
    pub fn fixed(rate: Decimal) -> Self {
        crate::init_tls();
        FxCache { http: reqwest::Client::new(), cached: Mutex::new(Some((rate, Instant::now()))), fixed: true }
    }
```

To support it:
1. Add a `fixed: bool` field.
2. Set `fixed: false` in `new`.
3. At the top of `usd_krw`, add: `if self.fixed { if let Some((rate, _)) = *self.cached.lock().unwrap() { return Ok(rate); } }`.

Create `src/app.rs`:

```rust
//! Everything a running node shares: broker, store, market data and FX.

use std::collections::HashMap;
use std::sync::{Arc, RwLock};

use rust_decimal::Decimal;
use rust_decimal_macros::dec;

use crate::broker::{OrderError, SimBroker};
use crate::domain::Currency;
use crate::fx::FxCache;
use crate::market::Market;
use crate::store::{AccountRow, Store};
use crate::tools::order_error;

pub struct App {
    pub broker: Arc<SimBroker>,
    pub store: Arc<Store>,
    pub market: Market,
    pub fx: FxCache,
    pub fx_spread: Decimal,
    accounts: RwLock<HashMap<String, AccountRow>>,
}

impl App {
    pub async fn new(broker: Arc<SimBroker>, store: Arc<Store>, market: Market, fx: FxCache) -> anyhow::Result<Self> {
        let app = App { broker, store, market, fx, fx_spread: dec!(0.001), accounts: RwLock::new(HashMap::new()) };
        app.reload_accounts().await?;
        Ok(app)
    }

    pub async fn reload_accounts(&self) -> anyhow::Result<()> {
        let rows = self.store.list_accounts().await?;
        *self.accounts.write().unwrap() = rows.into_iter().map(|r| (r.id.clone(), r)).collect();
        Ok(())
    }

    /// Accounts an agent may use: the ones with an agent id, sorted by id.
    pub fn agent_accounts(&self) -> Vec<AccountRow> {
        let mut rows: Vec<AccountRow> =
            self.accounts.read().unwrap().values().filter(|r| r.agent_id.is_some()).cloned().collect();
        rows.sort_by(|a, b| a.id.cmp(&b.id));
        rows
    }

    pub fn agent_account(&self, id: &str) -> Result<AccountRow, zyris::Error> {
        self.accounts
            .read()
            .unwrap()
            .get(id)
            .filter(|r| r.agent_id.is_some())
            .cloned()
            .ok_or_else(|| order_error(OrderError::UnknownAccount))
    }
}

/// KRW per unit of `c`; USDT counts as USD.
pub fn krw_per(c: Currency, usd_krw: Decimal) -> Decimal {
    if c == Currency::Krw { Decimal::ONE } else { usd_krw }
}

/// Load every account's portfolio and open orders from the store into `broker`. Returns how
/// many accounts were restored.
pub async fn restore(store: &Store, broker: &SimBroker) -> anyhow::Result<usize> {
    let accounts = store.list_accounts().await?;
    for a in &accounts {
        broker.restore_account(&a.id, store.load_portfolio(&a.id, a.generation).await?);
        for o in store.orders(&a.id, a.generation, true, 100_000).await?.into_iter().rev() {
            let id = o.id;
            if let Err(e) = broker.restore_order(o) {
                tracing::warn!(order = id, error = %e, "could not restore open order");
            }
        }
    }
    broker.set_next_order_id(store.max_order_id().await? + 1);
    Ok(accounts.len())
}
```

Create `src/tools/dto.rs`:

```rust
//! What the `trader` tools take and return. Doc comments here are the field descriptions the
//! model reads.

use chrono::{DateTime, Utc};
use rust_decimal::Decimal;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::broker::{Conversion, Estimate, Fill, Order, OrderType, Tif};
use crate::domain::{Level, Side};
use crate::sim::Size;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum SideDto {
    Buy,
    Sell,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum KindDto {
    Market,
    Limit,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum TifDto {
    Day,
    Gtc,
    Ioc,
}

impl From<SideDto> for Side {
    fn from(s: SideDto) -> Side {
        match s {
            SideDto::Buy => Side::Buy,
            SideDto::Sell => Side::Sell,
        }
    }
}

impl From<Side> for SideDto {
    fn from(s: Side) -> SideDto {
        match s {
            Side::Buy => SideDto::Buy,
            Side::Sell => SideDto::Sell,
        }
    }
}

impl From<KindDto> for OrderType {
    fn from(k: KindDto) -> OrderType {
        match k {
            KindDto::Market => OrderType::Market,
            KindDto::Limit => OrderType::Limit,
        }
    }
}

impl From<OrderType> for KindDto {
    fn from(k: OrderType) -> KindDto {
        match k {
            OrderType::Market => KindDto::Market,
            OrderType::Limit => KindDto::Limit,
        }
    }
}

impl From<TifDto> for Tif {
    fn from(t: TifDto) -> Tif {
        match t {
            TifDto::Day => Tif::Day,
            TifDto::Gtc => Tif::Gtc,
            TifDto::Ioc => Tif::Ioc,
        }
    }
}

impl From<Tif> for TifDto {
    fn from(t: Tif) -> TifDto {
        match t {
            Tif::Day => TifDto::Day,
            Tif::Gtc => TifDto::Gtc,
            Tif::Ioc => TifDto::Ioc,
        }
    }
}

/// An order to estimate or place.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct OrderInput {
    /// Account id from `list_accounts`.
    pub account: String,
    /// Instrument id `VENUE:SYMBOL`, e.g. `UPBIT:KRW-BTC`, `BINANCE:BTCUSDT`, `KRX:005930`, `US:AAPL`.
    pub instrument: String,
    pub side: SideDto,
    /// `market` executes now against the order book, walking levels (bigger orders pay more);
    /// `limit` executes up to `limit_price` now and rests the remainder.
    pub kind: KindDto,
    /// Quantity in shares or coins. Give exactly one of `qty` or `notional`.
    #[serde(default)]
    pub qty: Option<Decimal>,
    /// Quote-currency amount to spend; market buys only. Fees come out of it.
    #[serde(default)]
    pub notional: Option<Decimal>,
    /// Limit price in the quote currency; required for `limit`, must sit on a valid tick.
    #[serde(default)]
    pub limit_price: Option<Decimal>,
    /// Stocks: `day` (default for limit) or `ioc`. Crypto: `gtc` (default for limit) or `ioc`.
    /// Market orders are always `ioc`.
    #[serde(default)]
    pub tif: Option<TifDto>,
    /// Why you are placing this order, in a sentence or two. Required; the user reads it next
    /// to the fill.
    pub reason: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct OrderView {
    pub id: u64,
    pub account: String,
    pub instrument: String,
    pub side: SideDto,
    pub kind: KindDto,
    pub qty: Option<Decimal>,
    pub notional: Option<Decimal>,
    pub limit_price: Option<Decimal>,
    pub tif: TifDto,
    /// `open`, `filled`, `cancelled` or `expired`. A cancelled order may be partly filled.
    pub status: String,
    pub filled_qty: Decimal,
    /// Average fill price so far.
    pub avg_price: Option<Decimal>,
    pub reason: String,
    pub created_at: DateTime<Utc>,
}

impl From<&Order> for OrderView {
    fn from(o: &Order) -> Self {
        let (qty, notional) = match o.req.size {
            Size::Qty(q) => (Some(q), None),
            Size::Notional(n) => (None, Some(n)),
        };
        OrderView {
            id: o.id,
            account: o.account.clone(),
            instrument: o.req.instrument.to_string(),
            side: o.req.side.into(),
            kind: o.req.kind.into(),
            qty,
            notional,
            limit_price: o.req.limit_price,
            tif: o.req.tif.into(),
            status: format!("{:?}", o.status).to_lowercase(),
            filled_qty: o.filled_qty,
            avg_price: o.avg_price(),
            reason: o.req.reason.clone(),
            created_at: o.created_at,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct FillView {
    pub order_id: u64,
    pub instrument: String,
    pub side: SideDto,
    pub qty: Decimal,
    /// Average price of this execution, quote currency.
    pub price: Decimal,
    pub notional: Decimal,
    pub fee: Decimal,
    pub tax: Decimal,
    /// Sells only: profit versus average cost, after this sale's fee and tax.
    pub realized_pnl: Option<Decimal>,
    /// `taker` (crossed the book) or `maker` (a resting limit order was hit).
    pub liquidity: String,
    pub at: DateTime<Utc>,
}

impl From<&Fill> for FillView {
    fn from(f: &Fill) -> Self {
        FillView {
            order_id: f.order_id,
            instrument: f.instrument.to_string(),
            side: f.side.into(),
            qty: f.qty,
            price: f.price,
            notional: f.notional,
            fee: f.fee,
            tax: f.tax,
            realized_pnl: f.realized_pnl,
            liquidity: format!("{:?}", f.liquidity).to_lowercase(),
            at: f.at,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct PlaceResult {
    pub order: OrderView,
    /// Executions that happened immediately. A resting limit order fills later; see `list_fills`.
    pub fills: Vec<FillView>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct EstimateView {
    /// Quantity that would execute immediately.
    pub filled_qty: Decimal,
    pub avg_price: Option<Decimal>,
    pub notional: Decimal,
    pub fee: Decimal,
    pub tax: Decimal,
    /// Quantity that would rest as a limit order.
    pub rest_qty: Decimal,
    /// Distance of the average price from the current mid, in basis points.
    pub slippage_bps: Decimal,
    /// How far this execution would push the price for the next trades, in basis points; it
    /// fades over time (minutes to half an hour).
    pub impact_bps: f64,
}

impl From<Estimate> for EstimateView {
    fn from(e: Estimate) -> Self {
        EstimateView {
            filled_qty: e.filled_qty,
            avg_price: e.avg_price,
            notional: e.notional,
            fee: e.fee,
            tax: e.tax,
            rest_qty: e.rest_qty,
            slippage_bps: e.slippage_bps,
            impact_bps: (e.impact_bps * 100.0).round() / 100.0,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct LevelView {
    pub price: Decimal,
    pub qty: Decimal,
}

impl From<&Level> for LevelView {
    fn from(l: &Level) -> Self {
        LevelView { price: l.price, qty: l.qty }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct InstrumentInfo {
    /// The id every other tool takes.
    pub id: String,
    pub name: String,
    /// Quote currency: KRW, USD or USDT.
    pub currency: String,
    /// Quantities must be multiples of this.
    pub qty_step: Decimal,
    pub min_qty: Decimal,
    /// Smallest order value accepted, quote currency.
    pub min_order_value: Decimal,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct VenueStatus {
    /// KRX, US, UPBIT or BINANCE.
    pub venue: String,
    pub open: bool,
    /// Next session open (stock venues); absent for 24/7 crypto venues.
    pub next_open: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema)]
pub struct Quote {
    pub id: String,
    pub currency: String,
    /// Best bid in the simulated book your orders execute against.
    pub bid: Option<Decimal>,
    /// Best ask in the simulated book your orders execute against.
    pub ask: Option<Decimal>,
    pub mid: Option<Decimal>,
    /// Best bid on the real exchange.
    pub real_bid: Option<Decimal>,
    /// Best ask on the real exchange.
    pub real_ask: Option<Decimal>,
    pub last_trade_price: Option<Decimal>,
    pub last_trade_at: Option<DateTime<Utc>>,
    /// How far all accounts' recent trading has pushed this price, in basis points (fades).
    pub impact_offset_bps: f64,
    /// When the book was received.
    pub as_of: Option<DateTime<Utc>>,
    /// True when the data is more than 5 s old; orders would be rejected.
    pub stale: bool,
    /// Why data is missing, if it is.
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct OrderBook {
    pub id: String,
    pub currency: String,
    /// Simulated book (what your orders execute against), best first.
    pub bids: Vec<LevelView>,
    pub asks: Vec<LevelView>,
    /// Real exchange book, best first.
    pub real_bids: Vec<LevelView>,
    pub real_asks: Vec<LevelView>,
    pub as_of: DateTime<Utc>,
    pub stale: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct AccountInfo {
    pub id: String,
    pub name: String,
    /// Increases each time the user resets the account.
    pub generation: i32,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct CashView {
    pub currency: String,
    pub balance: Decimal,
    /// Balance minus cash reserved for open buy orders.
    pub available: Decimal,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct AccountSummary {
    pub id: String,
    pub name: String,
    pub cash: Vec<CashView>,
    pub positions_value_krw: Decimal,
    /// Cash plus positions at mid prices, in KRW.
    pub equity_krw: Decimal,
    /// KRW per USD used for the valuation (USDT counts as USD).
    pub usd_krw: Decimal,
    pub as_of: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct PositionView {
    pub instrument: String,
    pub name: String,
    pub currency: String,
    pub qty: Decimal,
    /// Moving average cost per unit, excluding fees.
    pub avg_cost: Decimal,
    /// Current simulated mid; absent when there is no market data (valued at cost then).
    pub price: Option<Decimal>,
    pub market_value: Decimal,
    pub unrealized_pnl: Decimal,
    pub unrealized_pct: Decimal,
    /// Share of account equity, percent.
    pub weight_pct: Decimal,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct ConversionView {
    pub from: String,
    pub to: String,
    pub debit: Decimal,
    pub credit: Decimal,
    /// Units of `to` received per unit of `from`, after the spread.
    pub rate: Decimal,
}

impl From<&Conversion> for ConversionView {
    fn from(c: &Conversion) -> Self {
        ConversionView { from: c.from.code().into(), to: c.to.code().into(), debit: c.debit, credit: c.credit, rate: c.rate }
    }
}
```

Create `src/tools/mod.rs`. It holds the trait, the helpers, and the discovery and quote tools. Tools that Task 5 implements return `Err(zyris::Error::internal("not yet implemented"))` until then; Task 5 replaces every one of them.

```rust
//! The `trader` zyris capability: what an Attacca agent can do with ATrader.

pub mod dto;

use std::sync::Arc;

use chrono::{DateTime, Utc};
use rust_decimal::Decimal;
use serde_json::json;
use zyris::{ErrorCode, Payload};

pub use dto::*;

use crate::app::App;
use crate::broker::OrderError;
use crate::domain::{Currency, InstrumentId, Level, Venue};

const STALE_AFTER_SECS: i64 = 5;
const MAX_QUOTES: usize = 20;

/// Paper trading on real market data. Orders execute against a simulated copy of the real
/// order book: large orders walk the book and pay slippage, and trading moves the price for a
/// while afterwards, so splitting big orders and using limit orders matters. Start with
/// `list_accounts`, find instruments with `search_instruments`, check `get_quotes`, size with
/// `estimate_order`, then `place_order`.
#[zyris::capability(name = "trader", version = 1)]
pub trait Trader {
    /// Find instruments by symbol or name (Korean or English). `venue` narrows to KRX, US,
    /// UPBIT or BINANCE. Returns up to 20 matches with the `VENUE:SYMBOL` id the other tools take.
    async fn search_instruments(&self, query: String, venue: Option<String>) -> zyris::Result<Vec<InstrumentInfo>>;

    /// Whether each venue is open now and when stock venues next open. Crypto trades 24/7.
    async fn market_status(&self) -> zyris::Result<Vec<VenueStatus>>;

    /// Quotes for up to 20 instruments: best bid/ask of the simulated book you trade against,
    /// the real exchange's best bid/ask, the last real trade, and how far recent trading has
    /// pushed the price. Fetches fresh data first.
    async fn get_quotes(&self, ids: Vec<String>) -> zyris::Result<Vec<Quote>>;

    /// Order book for one instrument, simulated and real side by side. `depth` levels per side,
    /// default 10, at most 30.
    async fn get_orderbook(&self, id: String, depth: Option<u32>) -> zyris::Result<OrderBook>;

    /// Dry run of `place_order`: expected fill quantity, average price, fees, tax, slippage and
    /// the price impact it would leave. Changes nothing.
    async fn estimate_order(&self, order: OrderInput) -> zyris::Result<EstimateView>;

    /// Place an order. Market orders execute immediately (unfilled remainder cancelled); limit
    /// orders execute what they can and rest the rest. Errors carry a code (MARKET_CLOSED,
    /// INSUFFICIENT_FUNDS, INVALID_TICK, ...) and the fields needed to fix the order.
    async fn place_order(&self, order: OrderInput) -> zyris::Result<PlaceResult>;

    /// Cancel an open order; its unfilled part is released.
    async fn cancel_order(&self, account: String, order_id: u64) -> zyris::Result<OrderView>;

    /// Orders of an account, newest first. `open_only` shows resting orders only; `limit`
    /// default 50, at most 200.
    async fn list_orders(&self, account: String, open_only: Option<bool>, limit: Option<u32>) -> zyris::Result<Vec<OrderView>>;

    /// Executions of an account, newest first, optionally only after `since`. `limit` default
    /// 50, at most 200.
    async fn list_fills(&self, account: String, since: Option<DateTime<Utc>>, limit: Option<u32>) -> zyris::Result<Vec<FillView>>;

    /// Accounts you may trade.
    async fn list_accounts(&self) -> zyris::Result<Vec<AccountInfo>>;

    /// Cash per currency, positions value and total equity in KRW.
    async fn get_account(&self, account: String) -> zyris::Result<AccountSummary>;

    /// Holdings with average cost, current price, unrealized profit and portfolio weight.
    async fn get_positions(&self, account: String) -> zyris::Result<Vec<PositionView>>;

    /// Exchange cash between KRW, USD and USDT at the reference rate less a 0.1% spread. USD
    /// buys US stocks, USDT buys Binance coins, KRW buys KRX stocks and Upbit coins.
    async fn convert_currency(&self, account: String, from: String, to: String, amount: Decimal) -> zyris::Result<ConversionView>;
}

pub struct TraderTools {
    app: Arc<App>,
}

impl TraderTools {
    pub fn new(app: Arc<App>) -> Self {
        TraderTools { app }
    }
}

fn bad(msg: impl Into<String>) -> zyris::Error {
    zyris::Error::invalid_params(msg)
}

fn coded(code: &str, message: String, data: serde_json::Value) -> zyris::Error {
    zyris::Error::new(ErrorCode::Other(code.into()), message).with_data(Payload::from_json(data))
}

pub(crate) fn upstream(e: impl std::fmt::Display) -> zyris::Error {
    coded("UPSTREAM_ERROR", e.to_string(), json!({})).retriable(true)
}

/// An order error as the agent sees it: a stable code plus the fields needed to recover.
pub fn order_error(e: OrderError) -> zyris::Error {
    let msg = e.to_string();
    let (code, data) = match e {
        OrderError::MarketClosed { next_open } => ("MARKET_CLOSED", json!({ "next_open": next_open })),
        OrderError::StaleData { age_secs } => ("STALE_DATA", json!({ "age_secs": age_secs })),
        OrderError::NoLiquidity => ("NO_LIQUIDITY", json!({})),
        OrderError::InvalidTick { lower, upper } => ("INVALID_TICK", json!({ "lower": lower, "upper": upper })),
        OrderError::InvalidQty { step, min_qty, min_notional } => {
            ("INVALID_QTY", json!({ "step": step, "min_qty": min_qty, "min_notional": min_notional }))
        }
        OrderError::PriceLimit { lower, upper } => ("PRICE_LIMIT", json!({ "lower": lower, "upper": upper })),
        OrderError::InsufficientFunds { required, available } => {
            ("INSUFFICIENT_FUNDS", json!({ "required": required, "available": available }))
        }
        OrderError::InsufficientPosition { available } => ("INSUFFICIENT_POSITION", json!({ "available": available })),
        OrderError::UnknownInstrument => ("UNKNOWN_INSTRUMENT", json!({})),
        OrderError::UnknownAccount => ("UNKNOWN_ACCOUNT", json!({})),
        OrderError::NotTradable => ("NOT_TRADABLE", json!({})),
        OrderError::NotFound => ("NOT_FOUND", json!({})),
        OrderError::InvalidRequest(_) => ("INVALID_REQUEST", json!({})),
    };
    coded(code, msg, data)
}

fn parse_id(s: &str) -> Result<InstrumentId, zyris::Error> {
    s.trim().parse().map_err(|m: String| coded("UNKNOWN_INSTRUMENT", m, json!({})))
}

fn parse_venue(s: &str) -> Result<Venue, zyris::Error> {
    Venue::from_tag(&s.trim().to_uppercase()).ok_or_else(|| bad(format!("unknown venue {s:?}; use KRX, US, UPBIT or BINANCE")))
}

fn parse_currency(s: &str) -> Result<Currency, zyris::Error> {
    Currency::from_code(&s.trim().to_uppercase()).ok_or_else(|| bad(format!("unknown currency {s:?}; use KRW, USD or USDT")))
}

fn mid(bids: &[Level], asks: &[Level]) -> Option<Decimal> {
    Some((bids.first()?.price + asks.first()?.price) / Decimal::TWO)
}

fn clamp_limit(limit: Option<u32>) -> i64 {
    i64::from(limit.unwrap_or(50).clamp(1, 200))
}

impl TraderTools {
    fn quote(&self, id: &InstrumentId, error: Option<String>) -> Quote {
        let currency = id.venue.currency().code().to_string();
        let Some(v) = self.app.broker.book_view(id, 1) else {
            return Quote {
                id: id.to_string(),
                currency,
                stale: true,
                error: error.or_else(|| Some("no market data for this instrument".into())),
                ..Default::default()
            };
        };
        Quote {
            id: id.to_string(),
            currency,
            bid: v.shadow_bids.first().map(|l| l.price),
            ask: v.shadow_asks.first().map(|l| l.price),
            mid: mid(&v.shadow_bids, &v.shadow_asks),
            real_bid: v.real_bids.first().map(|l| l.price),
            real_ask: v.real_asks.first().map(|l| l.price),
            last_trade_price: v.last_trade.map(|t| t.0),
            last_trade_at: v.last_trade.map(|t| t.1),
            impact_offset_bps: (v.offset * 1_000_000.0).round() / 100.0,
            as_of: Some(v.received_at),
            stale: (self.app.broker.now() - v.received_at).num_seconds() > STALE_AFTER_SECS,
            error,
        }
    }
}

#[zyris::async_trait]
impl Trader for TraderTools {
    async fn search_instruments(&self, query: String, venue: Option<String>) -> zyris::Result<Vec<InstrumentInfo>> {
        let venue = venue.as_deref().map(parse_venue).transpose()?;
        Ok(self
            .app
            .broker
            .search(&query, venue, 20)
            .into_iter()
            .map(|i| InstrumentInfo {
                id: i.id.to_string(),
                name: i.name,
                currency: i.id.venue.currency().code().into(),
                qty_step: i.lot.step.normalize(),
                min_qty: i.lot.min_qty.normalize(),
                min_order_value: i.lot.min_notional.normalize(),
            })
            .collect())
    }

    async fn market_status(&self) -> zyris::Result<Vec<VenueStatus>> {
        Ok([Venue::Krx, Venue::Us, Venue::Upbit, Venue::Binance]
            .into_iter()
            .map(|v| {
                let (open, next_open) = self.app.broker.market_open(v);
                VenueStatus { venue: v.tag().into(), open, next_open }
            })
            .collect())
    }

    async fn get_quotes(&self, ids: Vec<String>) -> zyris::Result<Vec<Quote>> {
        if ids.len() > MAX_QUOTES {
            return Err(bad(format!("at most {MAX_QUOTES} ids per call")));
        }
        let ids = ids.iter().map(|s| parse_id(s)).collect::<Result<Vec<_>, _>>()?;
        let mut out = Vec::with_capacity(ids.len());
        for id in &ids {
            let error = self.app.market.ensure_fresh(id).await.err().map(|e| format!("{e:#}"));
            out.push(self.quote(id, error));
        }
        Ok(out)
    }

    async fn get_orderbook(&self, id: String, depth: Option<u32>) -> zyris::Result<OrderBook> {
        let id = parse_id(&id)?;
        let depth = depth.unwrap_or(10).clamp(1, 30) as usize;
        self.app.market.ensure_fresh(&id).await.map_err(|e| upstream(format!("{e:#}")))?;
        let v = self.app.broker.book_view(&id, depth).ok_or_else(|| order_error(OrderError::UnknownInstrument))?;
        let levels = |l: &[Level]| l.iter().map(LevelView::from).collect::<Vec<_>>();
        Ok(OrderBook {
            id: id.to_string(),
            currency: id.venue.currency().code().into(),
            bids: levels(&v.shadow_bids),
            asks: levels(&v.shadow_asks),
            real_bids: levels(&v.real_bids),
            real_asks: levels(&v.real_asks),
            as_of: v.received_at,
            stale: (self.app.broker.now() - v.received_at).num_seconds() > STALE_AFTER_SECS,
        })
    }

    async fn estimate_order(&self, _order: OrderInput) -> zyris::Result<EstimateView> {
        Err(zyris::Error::internal("not yet implemented"))
    }

    async fn place_order(&self, _order: OrderInput) -> zyris::Result<PlaceResult> {
        Err(zyris::Error::internal("not yet implemented"))
    }

    async fn cancel_order(&self, _account: String, _order_id: u64) -> zyris::Result<OrderView> {
        Err(zyris::Error::internal("not yet implemented"))
    }

    async fn list_orders(&self, _account: String, _open_only: Option<bool>, _limit: Option<u32>) -> zyris::Result<Vec<OrderView>> {
        Err(zyris::Error::internal("not yet implemented"))
    }

    async fn list_fills(&self, _account: String, _since: Option<DateTime<Utc>>, _limit: Option<u32>) -> zyris::Result<Vec<FillView>> {
        Err(zyris::Error::internal("not yet implemented"))
    }

    async fn list_accounts(&self) -> zyris::Result<Vec<AccountInfo>> {
        Err(zyris::Error::internal("not yet implemented"))
    }

    async fn get_account(&self, _account: String) -> zyris::Result<AccountSummary> {
        Err(zyris::Error::internal("not yet implemented"))
    }

    async fn get_positions(&self, _account: String) -> zyris::Result<Vec<PositionView>> {
        Err(zyris::Error::internal("not yet implemented"))
    }

    async fn convert_currency(&self, _account: String, _from: String, _to: String, _amount: Decimal) -> zyris::Result<ConversionView> {
        Err(zyris::Error::internal("not yet implemented"))
    }
}
```

Add `pub mod app;` and `pub mod tools;` to `src/lib.rs`.

- [ ] **Step 5: Run the tests**

Run: `cargo test --test tools`
Expected: `discovery_and_quotes` and `every_order_error_maps_to_a_code` pass. `malformed_arguments_are_errors_not_panics` and `order_errors_keep_their_code_and_fields` still fail with "not yet implemented", which is correct because Task 5 implements the trading tools.

- [ ] **Step 6: Commit**

```bash
git add Cargo.toml Cargo.lock src/app.rs src/tools src/fx.rs src/lib.rs tests/tools.rs
git commit -m "Add trader capability with discovery and quote tools"
git log -1 --format=%B
```

---

### Task 5: Trading and account tools

**Files:**
- Modify: `src/tools/mod.rs`, `tests/tools.rs`

**Interfaces:**
- Consumes:
  - `SimBroker::{estimate, place_sync, cancel_sync, portfolio, convert_sync, book_view, instrument}`.
  - `Store::{orders, fills}`.
  - `Market::{ensure_fresh, refresh_pins}`.
  - `App::{agent_account, agent_accounts, fx, fx_spread}`.
  - `krw_per`.

- [ ] **Step 1: Write the failing tests** (append to `tests/tools.rs`)

```rust
#[sqlx::test]
async fn place_then_read_history_and_account(pool: PgPool) {
    let (app, t) = rig(pool).await;
    assert_eq!(t.list_accounts().await.unwrap().iter().map(|a| a.id.clone()).collect::<Vec<_>>(), vec!["bot"]);
    let est = t.estimate_order(buy("bot", Some(dec!(0.1)), "sizing")).await.unwrap();
    assert_eq!(est.filled_qty, dec!(0.1));
    let placed = t.place_order(buy("bot", Some(dec!(0.1)), "momentum entry")).await.unwrap();
    assert_eq!(placed.order.status, "filled");
    assert_eq!(placed.fills.len(), 1);

    let mut fills = Vec::new();
    for _ in 0..100 {
        fills = t.list_fills("bot".into(), None, None).await.unwrap();
        if !fills.is_empty() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert_eq!(fills.len(), 1);
    let orders = t.list_orders("bot".into(), None, None).await.unwrap();
    assert_eq!(orders[0].reason, "momentum entry");

    let acct = t.get_account("bot".into()).await.unwrap();
    assert!(acct.equity_krw < dec!(1000000000) && acct.equity_krw > dec!(999900000), "equity {}", acct.equity_krw);
    let pos = t.get_positions("bot".into()).await.unwrap();
    assert_eq!(pos[0].qty, dec!(0.1));
    assert!(pos[0].weight_pct > dec!(0) && pos[0].weight_pct < dec!(2));

    let c = t.convert_currency("bot".into(), "krw".into(), "USD".into(), dec!(1400000)).await.unwrap();
    assert_eq!(c.credit, dec!(999));
    let acct = t.get_account("bot".into()).await.unwrap();
    assert!(acct.cash.iter().any(|c| c.currency == "USD" && c.balance == dec!(999)));
    assert_eq!(code(&t.convert_currency("bot".into(), "KRW".into(), "EUR".into(), dec!(1)).await.unwrap_err()), "InvalidParams");
    let _ = app;
}

#[sqlx::test]
async fn resting_orders_can_be_listed_and_cancelled(pool: PgPool) {
    let (_, t) = rig(pool).await;
    let mut o = buy("bot", Some(dec!(0.1)), "bid below market");
    o.kind = KindDto::Limit;
    o.limit_price = Some(dec!(99000000));
    let placed = t.place_order(o).await.unwrap();
    assert_eq!((placed.order.status.as_str(), placed.order.tif), ("open", TifDto::Gtc));
    let cancelled = t.cancel_order("bot".into(), placed.order.id).await.unwrap();
    assert_eq!(cancelled.status, "cancelled");
    assert_eq!(code(&t.cancel_order("bot".into(), 999).await.unwrap_err()), "NOT_FOUND");
    assert_eq!(code(&t.cancel_order("manual".into(), placed.order.id).await.unwrap_err()), "UNKNOWN_ACCOUNT");
    assert_eq!(code(&t.get_account("manual".into()).await.unwrap_err()), "UNKNOWN_ACCOUNT");
    assert_eq!(code(&t.list_orders("manual".into(), None, None).await.unwrap_err()), "UNKNOWN_ACCOUNT");
}
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test --test tools`
Expected: the new tests and the two Task 4 trading tests fail with "not yet implemented".

- [ ] **Step 3: Implement** (in `src/tools/mod.rs`)

Add these imports: `use crate::app::krw_per;`, `use crate::broker::{OrderRequest, OrderType, Tif};` and `use crate::sim::Size;`. Then add:

```rust
fn to_request(o: &OrderInput) -> Result<OrderRequest, zyris::Error> {
    let instrument = parse_id(&o.instrument)?;
    let size = match (o.qty, o.notional) {
        (Some(q), None) => Size::Qty(q),
        (None, Some(n)) => Size::Notional(n),
        _ => return Err(bad("give exactly one of qty or notional")),
    };
    if o.reason.trim().is_empty() {
        return Err(bad("reason is required: say why you are placing this order"));
    }
    let kind: OrderType = o.kind.into();
    let tif = match (o.tif, kind) {
        (_, OrderType::Market) => Tif::Ioc,
        (Some(t), OrderType::Limit) => t.into(),
        (None, OrderType::Limit) if instrument.venue.has_session() => Tif::Day,
        (None, OrderType::Limit) => Tif::Gtc,
    };
    Ok(OrderRequest { instrument, side: o.side.into(), kind, size, limit_price: o.limit_price, tif, reason: o.reason.trim().to_string() })
}
```

Add a valuation helper to `impl TraderTools`:

```rust
    async fn valuation(&self, account: &str) -> zyris::Result<(AccountSummary, Vec<PositionView>)> {
        let row = self.app.agent_account(account)?;
        let pf = self.app.broker.portfolio(&row.id).ok_or_else(|| order_error(OrderError::UnknownAccount))?;
        let usd_krw = self.app.fx.usd_krw().await.map_err(|e| upstream(format!("{e:#}")))?;
        let mut rows = Vec::new();
        for (id, p) in &pf.positions {
            if p.qty.is_zero() {
                continue;
            }
            if let Err(e) = self.app.market.ensure_fresh(id).await {
                tracing::debug!(instrument = %id, error = %e, "valuing without fresh data");
            }
            let price = self.app.broker.book_view(id, 1).and_then(|v| mid(&v.shadow_bids, &v.shadow_asks));
            let cur = id.venue.currency();
            let value = (p.qty * price.unwrap_or(p.avg_cost)).round_dp(cur.decimals());
            let cost = p.qty * p.avg_cost;
            let pct = if cost.is_zero() { Decimal::ZERO } else { ((value - cost) / cost * Decimal::ONE_HUNDRED).round_dp(2) };
            let view = PositionView {
                instrument: id.to_string(),
                name: self.app.broker.instrument(id).map(|i| i.name).unwrap_or_default(),
                currency: cur.code().into(),
                qty: p.qty,
                avg_cost: p.avg_cost.round_dp(8),
                price,
                market_value: value,
                unrealized_pnl: (value - cost).round_dp(cur.decimals()),
                unrealized_pct: pct,
                weight_pct: Decimal::ZERO,
            };
            rows.push((view, value * krw_per(cur, usd_krw)));
        }
        let mut cash = Vec::new();
        let mut cash_krw = Decimal::ZERO;
        for c in [Currency::Krw, Currency::Usd, Currency::Usdt] {
            if let Some(balance) = pf.cash.get(&c).copied() {
                cash.push(CashView { currency: c.code().into(), balance, available: pf.available_cash(c) });
                cash_krw += balance * krw_per(c, usd_krw);
            }
        }
        let positions_krw: Decimal = rows.iter().map(|(_, v)| *v).sum();
        let equity = cash_krw + positions_krw;
        rows.sort_by(|a, b| b.1.cmp(&a.1));
        let positions = rows
            .into_iter()
            .map(|(mut p, v)| {
                p.weight_pct = if equity.is_zero() { Decimal::ZERO } else { (v / equity * Decimal::ONE_HUNDRED).round_dp(2) };
                p
            })
            .collect();
        let summary = AccountSummary {
            id: row.id,
            name: row.name,
            cash,
            positions_value_krw: positions_krw.round_dp(0),
            equity_krw: equity.round_dp(0),
            usd_krw,
            as_of: self.app.broker.now(),
        };
        Ok((summary, positions))
    }
```

Replace the "not yet implemented" bodies:

```rust
    async fn estimate_order(&self, order: OrderInput) -> zyris::Result<EstimateView> {
        let row = self.app.agent_account(&order.account)?;
        let req = to_request(&order)?;
        self.app.market.ensure_fresh(&req.instrument).await.map_err(|e| upstream(format!("{e:#}")))?;
        self.app.broker.estimate(&row.id, &req).map(EstimateView::from).map_err(order_error)
    }

    async fn place_order(&self, order: OrderInput) -> zyris::Result<PlaceResult> {
        let row = self.app.agent_account(&order.account)?;
        let req = to_request(&order)?;
        self.app.market.ensure_fresh(&req.instrument).await.map_err(|e| upstream(format!("{e:#}")))?;
        let (placed, fills) = self.app.broker.place_sync(&row.id, req).map_err(order_error)?;
        self.app.market.refresh_pins();
        Ok(PlaceResult { order: OrderView::from(&placed), fills: fills.iter().map(FillView::from).collect() })
    }

    async fn cancel_order(&self, account: String, order_id: u64) -> zyris::Result<OrderView> {
        let row = self.app.agent_account(&account)?;
        let order = self.app.broker.cancel_sync(&row.id, order_id).map_err(order_error)?;
        self.app.market.refresh_pins();
        Ok(OrderView::from(&order))
    }

    async fn list_orders(&self, account: String, open_only: Option<bool>, limit: Option<u32>) -> zyris::Result<Vec<OrderView>> {
        let row = self.app.agent_account(&account)?;
        let orders = self
            .app
            .store
            .orders(&row.id, row.generation, open_only.unwrap_or(false), clamp_limit(limit))
            .await
            .map_err(upstream)?;
        Ok(orders.iter().map(OrderView::from).collect())
    }

    async fn list_fills(&self, account: String, since: Option<DateTime<Utc>>, limit: Option<u32>) -> zyris::Result<Vec<FillView>> {
        let row = self.app.agent_account(&account)?;
        let fills = self.app.store.fills(&row.id, row.generation, since, clamp_limit(limit)).await.map_err(upstream)?;
        Ok(fills.iter().map(FillView::from).collect())
    }

    async fn list_accounts(&self) -> zyris::Result<Vec<AccountInfo>> {
        Ok(self
            .app
            .agent_accounts()
            .into_iter()
            .map(|r| AccountInfo { id: r.id, name: r.name, generation: r.generation })
            .collect())
    }

    async fn get_account(&self, account: String) -> zyris::Result<AccountSummary> {
        Ok(self.valuation(&account).await?.0)
    }

    async fn get_positions(&self, account: String) -> zyris::Result<Vec<PositionView>> {
        Ok(self.valuation(&account).await?.1)
    }

    async fn convert_currency(&self, account: String, from: String, to: String, amount: Decimal) -> zyris::Result<ConversionView> {
        let row = self.app.agent_account(&account)?;
        let (from, to) = (parse_currency(&from)?, parse_currency(&to)?);
        let usd_krw = self.app.fx.usd_krw().await.map_err(|e| upstream(format!("{e:#}")))?;
        let c = self.app.broker.convert_sync(&row.id, from, to, amount, usd_krw, self.app.fx_spread).map_err(order_error)?;
        Ok(ConversionView::from(&c))
    }
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test --test tools` (with `DATABASE_URL`)
Expected: 6 passed.

- [ ] **Step 5: Commit**

```bash
git add src/tools/mod.rs tests/tools.rs
git commit -m "Implement trading and account tools"
git log -1 --format=%B
```

---

### Task 6: CLI, `serve` wiring and restart restore

**Files:**
- Create: `src/cli.rs`
- Modify: `src/main.rs`, `src/lib.rs`, `tests/tools.rs`, `README.md`, `CLAUDE.md`, the spec (§13)

**Interfaces:**
- Produces:
  - `enum Command { Serve { zyris: bool }, AccountCreate { id, name, agent: Option<String>, cash: Vec<(Currency, Decimal)> }, AccountList, AccountReset { id, cash }, Version, Help }`.
  - `parse(&[String]) -> Result<Command, String>`.
  - `run(args: Vec<String>) -> anyhow::Result<()>`.

- [ ] **Step 1: Write the failing tests**

Create `src/cli.rs` holding only the tests:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use rust_decimal_macros::dec;

    fn args(s: &[&str]) -> Vec<String> {
        s.iter().map(|x| x.to_string()).collect()
    }

    #[test]
    fn parses_commands() {
        assert_eq!(parse(&args(&["serve"])), Ok(Command::Serve { zyris: true }));
        assert_eq!(parse(&args(&["serve", "--no-zyris"])), Ok(Command::Serve { zyris: false }));
        assert_eq!(parse(&args(&["account", "list"])), Ok(Command::AccountList));
        assert_eq!(
            parse(&args(&["account", "create", "bot", "My Bot", "--agent", "ag1", "--cash", "KRW=10000000", "--cash", "usd=1000"])),
            Ok(Command::AccountCreate {
                id: "bot".into(),
                name: "My Bot".into(),
                agent: Some("ag1".into()),
                cash: vec![(Currency::Krw, dec!(10000000)), (Currency::Usd, dec!(1000))],
            })
        );
        assert_eq!(
            parse(&args(&["account", "reset", "bot"])),
            Ok(Command::AccountReset { id: "bot".into(), cash: default_cash() })
        );
        assert_eq!(parse(&args(&[])), Ok(Command::Help));
        assert_eq!(parse(&args(&["--version"])), Ok(Command::Version));
    }

    #[test]
    fn rejects_bad_input() {
        assert!(parse(&args(&["trade"])).is_err());
        assert!(parse(&args(&["account", "create", "bot"])).is_err());
        assert!(parse(&args(&["account", "create", "bot", "Bot", "--cash", "KRW10"])).is_err());
        assert!(parse(&args(&["account", "create", "bot", "Bot", "--cash", "EUR=5"])).is_err());
        assert!(parse(&args(&["account", "create", "bot", "Bot", "--cash", "KRW=-5"])).is_err());
        assert!(parse(&args(&["account", "create", "bot", "Bot", "--agent"])).is_err());
        assert!(parse(&args(&["serve", "--fast"])).is_err());
    }
}
```

Append this restart test to `tests/tools.rs`:

```rust
#[sqlx::test]
async fn restart_restores_cash_positions_and_open_orders(pool: PgPool) {
    let (app, t) = rig(pool).await;
    t.place_order(buy("bot", Some(dec!(0.1)), "entry")).await.unwrap();
    let mut o = buy("bot", Some(dec!(0.1)), "resting bid");
    o.kind = KindDto::Limit;
    o.limit_price = Some(dec!(99000000));
    let resting = t.place_order(o).await.unwrap().order;
    t.convert_currency("bot".into(), "KRW".into(), "USD".into(), dec!(1400000)).await.unwrap();
    for _ in 0..100 {
        if app.store.orders("bot", 1, true, 10).await.unwrap().len() == 1 && app.store.cash_balances("bot", 1).await.unwrap().contains_key(&Currency::Usd) {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    let before = app.broker.portfolio("bot").unwrap();

    let clock = ManualClock::new(Utc.with_ymd_and_hms(2026, 9, 23, 1, 0, 0).unwrap());
    let fresh = SimBroker::new(Arc::new(clock), Calendar::default());
    assert_eq!(restore(&app.store, &fresh).await.unwrap(), 2);
    assert_eq!(fresh.portfolio("bot").unwrap(), before);
    assert_eq!(fresh.order(resting.id).unwrap().status, atrader::broker::OrderStatus::Open);
}
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test --lib cli:: && cargo test --test tools restart`
Expected: `cli` fails to compile (`parse` not found). The restart test may already pass if the restore from Tasks 1–5 is right. That is acceptable, because it is a regression guard for Review Focus 2. Record in the ledger that it passed on first run.

- [ ] **Step 3: Implement**

Prepend this to `src/cli.rs`:

```rust
//! Command line: account maintenance and `serve`.

use std::sync::Arc;

use anyhow::{Context, anyhow};
use rust_decimal::Decimal;
use rust_decimal_macros::dec;
use tokio::sync::{broadcast, mpsc};

use crate::app::{App, restore};
use crate::broker::SimBroker;
use crate::domain::{Clock, Currency, SystemClock};
use crate::feed::binance::BinanceFeed;
use crate::feed::upbit::UpbitFeed;
use crate::feed::{MarketFeed, run_feed};
use crate::fx::FxCache;
use crate::market::{Market, pump};
use crate::persist::persist;
use crate::store::Store;
use crate::tools::{TraderServer, TraderTools};
use crate::venue::Calendar;

const USAGE: &str = "atrader — paper trading for Attacca agents

USAGE:
  atrader serve [--no-zyris]
  atrader account create <id> <name> [--agent <attacca-agent-id>] [--cash KRW=10000000]...
  atrader account list
  atrader account reset <id> [--cash KRW=10000000]...

ENVIRONMENT:
  DATABASE_URL          Postgres connection string (required)
  ZYRIS_CREDENTIAL      zc_ credential issued in Attacca (/settings/zyris), or
  ZYRIS_CREDENTIAL_FILE file holding it
  ZYRIS_SERVER_URL      zyris server (default: Attacca's)
  ATRADER_NODE_NAME     node name shown in Attacca (default: atrader)
  RUST_LOG              log filter (default: atrader=info,zyris_core=info)";

#[derive(Debug, Clone, PartialEq)]
pub enum Command {
    Serve { zyris: bool },
    AccountCreate { id: String, name: String, agent: Option<String>, cash: Vec<(Currency, Decimal)> },
    AccountList,
    AccountReset { id: String, cash: Vec<(Currency, Decimal)> },
    Version,
    Help,
}

pub fn default_cash() -> Vec<(Currency, Decimal)> {
    vec![(Currency::Krw, dec!(10000000)), (Currency::Usd, dec!(7000)), (Currency::Usdt, dec!(7000))]
}

fn parse_cash(s: &str) -> Result<(Currency, Decimal), String> {
    let (c, amount) = s.split_once('=').ok_or_else(|| format!("--cash wants CURRENCY=AMOUNT, got {s:?}"))?;
    let c = Currency::from_code(&c.to_uppercase()).ok_or_else(|| format!("unknown currency {c:?}; use KRW, USD or USDT"))?;
    let amount: Decimal = amount.parse().map_err(|_| format!("bad amount {amount:?}"))?;
    if amount < Decimal::ZERO {
        return Err("cash cannot be negative".into());
    }
    Ok((c, amount))
}

/// `--agent X` and repeated `--cash C=N` after the positional arguments.
fn parse_flags(rest: &[String]) -> Result<(Option<String>, Vec<(Currency, Decimal)>), String> {
    let (mut agent, mut cash) = (None, Vec::new());
    let mut it = rest.iter();
    while let Some(flag) = it.next() {
        let value = it.next().ok_or_else(|| format!("{flag} needs a value"))?;
        match flag.as_str() {
            "--agent" => agent = Some(value.clone()),
            "--cash" => cash.push(parse_cash(value)?),
            other => return Err(format!("unknown option {other}")),
        }
    }
    Ok((agent, if cash.is_empty() { default_cash() } else { cash }))
}

pub fn parse(args: &[String]) -> Result<Command, String> {
    let words: Vec<&str> = args.iter().map(String::as_str).collect();
    match words.as_slice() {
        [] | ["help"] | ["--help"] | ["-h"] => Ok(Command::Help),
        ["--version"] | ["-V"] => Ok(Command::Version),
        ["serve"] => Ok(Command::Serve { zyris: true }),
        ["serve", "--no-zyris"] => Ok(Command::Serve { zyris: false }),
        ["account", "list"] => Ok(Command::AccountList),
        ["account", "create", id, name, ..] => {
            let (agent, cash) = parse_flags(&args[4..])?;
            Ok(Command::AccountCreate { id: id.to_string(), name: name.to_string(), agent, cash })
        }
        ["account", "reset", id, ..] => {
            let (agent, cash) = parse_flags(&args[3..])?;
            if agent.is_some() {
                return Err("reset does not take --agent".into());
            }
            Ok(Command::AccountReset { id: id.to_string(), cash })
        }
        _ => Err(format!("unrecognised command: {}\n\n{USAGE}", words.join(" "))),
    }
}

pub fn run(args: Vec<String>) -> anyhow::Result<()> {
    let command = parse(&args).map_err(|e| anyhow!(e))?;
    match command {
        Command::Help => {
            println!("{USAGE}");
            return Ok(());
        }
        Command::Version => {
            println!("atrader {}", env!("CARGO_PKG_VERSION"));
            return Ok(());
        }
        _ => {}
    }
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "atrader=info,zyris_core=info".into()),
        )
        .init();
    let rt = tokio::runtime::Builder::new_multi_thread().enable_all().build()?;
    rt.block_on(async move {
        let url = std::env::var("DATABASE_URL").context("DATABASE_URL is not set")?;
        let store = Arc::new(Store::connect(&url).await?);
        match command {
            Command::AccountCreate { id, name, agent, cash } => {
                store.create_account(&id, &name, agent.as_deref(), &cash, chrono::Utc::now()).await?;
                println!("created account {id}");
            }
            Command::AccountList => {
                for a in store.list_accounts().await? {
                    let cash = store.cash_balances(&a.id, a.generation).await?;
                    let cash: Vec<String> = cash.iter().map(|(c, v)| format!("{}={v}", c.code())).collect();
                    println!(
                        "{:<16} {:<24} agent={:<24} gen={} {}",
                        a.id,
                        a.name,
                        a.agent_id.as_deref().unwrap_or("-"),
                        a.generation,
                        cash.join(" ")
                    );
                }
            }
            Command::AccountReset { id, cash } => {
                let generation = store.reset_account(&id, &cash, chrono::Utc::now()).await?;
                println!("account {id} reset (generation {generation}); restart a running `atrader serve` to pick it up");
            }
            Command::Serve { zyris } => serve(store, zyris).await?,
            Command::Help | Command::Version => unreachable!("handled above"),
        }
        Ok(())
    })
}

fn credential() -> anyhow::Result<String> {
    if let Ok(c) = std::env::var("ZYRIS_CREDENTIAL") {
        if !c.trim().is_empty() {
            return Ok(c.trim().to_string());
        }
    }
    if let Ok(path) = std::env::var("ZYRIS_CREDENTIAL_FILE") {
        let c = std::fs::read_to_string(&path).with_context(|| format!("reading {path}"))?;
        return Ok(c.trim().to_string());
    }
    Err(anyhow!(
        "no zyris credential: set ZYRIS_CREDENTIAL (issue one in Attacca under /settings/zyris) or run `atrader serve --no-zyris`"
    ))
}

async fn serve(store: Arc<Store>, with_zyris: bool) -> anyhow::Result<()> {
    crate::init_tls();
    let token = if with_zyris { Some(credential()?) } else { None };
    let clock: Arc<dyn Clock> = Arc::new(SystemClock);
    let calendar = Calendar::from_toml(include_str!("../holidays.toml"))?;
    let (journal_tx, journal_rx) = mpsc::unbounded_channel();
    let broker = Arc::new(SimBroker::new(clock.clone(), calendar).with_journal(journal_tx));
    let (bus, _) = broadcast::channel(1024);
    let mut market = Market::new(broker.clone());

    let (events_tx, events_rx) = mpsc::channel(4096);
    let feeds: Vec<(Arc<dyn MarketFeed>, usize)> =
        vec![(Arc::new(UpbitFeed::new(clock.clone())), 50), (Arc::new(BinanceFeed::new(clock.clone())), 100)];
    for (feed, cap) in feeds {
        let subs = market.add_feed(feed.clone(), cap);
        tokio::spawn(run_feed(feed, subs, events_tx.clone()));
    }
    drop(events_tx);
    let instruments = market.load_instruments().await?;
    let accounts = restore(&store, &broker).await?;
    tracing::info!(instruments, accounts, "state restored");

    tokio::spawn(pump(events_rx, broker.clone(), bus.clone()));
    tokio::spawn(persist(journal_rx, store.clone(), bus.clone()));
    let app = Arc::new(App::new(broker.clone(), store, market, FxCache::new()).await?);
    app.market.refresh_pins();

    let timers = app.clone();
    tokio::spawn(async move {
        let mut tick = tokio::time::interval(std::time::Duration::from_secs(5));
        loop {
            tick.tick().await;
            timers.market.refresh_pins();
            timers.broker.expire_day_orders();
        }
    });

    let Some(token) = token else {
        tracing::info!("running without zyris; Ctrl-C to stop");
        tokio::signal::ctrl_c().await?;
        return Ok(());
    };
    let server = std::env::var("ZYRIS_SERVER_URL").unwrap_or_else(|_| zyris::DEFAULT_SERVER_URL.to_string());
    let name = std::env::var("ATRADER_NODE_NAME").unwrap_or_else(|_| "atrader".into());
    let link = zyris::Node::builder()
        .name(name)
        .kind(zyris::NodeKind::Service)
        .capability(TraderServer(TraderTools::new(app)))
        .build()?
        .connect(&server, &token)
        .await?;
    tracing::info!(node = %link.node_id(), %server, "serving trader capability");
    tokio::select! {
        closed = link.wait_closed() => closed?,
        _ = tokio::signal::ctrl_c() => link.disconnect().await,
    }
    Ok(())
}
```

Replace `src/main.rs` with:

```rust
fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if let Err(e) = atrader::cli::run(args) {
        eprintln!("error: {e:#}");
        std::process::exit(1);
    }
}
```

Add `pub mod cli;` to `src/lib.rs`.

In the spec, replace §13's order-processing bullet's last sentence with: "Every change is journaled; a writer task persists it in order and retries a failed write up to 10 times before logging the event and moving on (in-memory state is never rolled back)."

In `README.md`, replace the Status note with a "Running" section. It describes `scripts/dev-db.sh`, `atrader account create bot "Bot" --agent <attacca-agent-id>`, `ZYRIS_CREDENTIAL=zc_… atrader serve`, and `atrader serve --no-zyris` for running without Attacca. Mention that phases 4–8 are in progress.

In `CLAUDE.md`, add the `cargo run -- serve --no-zyris` and `cargo run -- account …` commands.

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test` (with `DATABASE_URL`)
Expected: every test passes.

- [ ] **Step 5: Smoke run without zyris**

```bash
cargo run -q -- account create bot "Bot" --agent smoke-test
timeout 25 cargo run -q -- serve --no-zyris 2>&1 | tail -5
cargo run -q -- account list
```

Expected: the log contains `state restored` with instruments > 500 and accounts ≥ 1, there are no panics or errors, and `account list` shows `bot` with its cash.

- [ ] **Step 6: Commit**

```bash
git add src/cli.rs src/main.rs src/lib.rs tests/tools.rs README.md CLAUDE.md docs/superpowers/specs/2026-09-24-atrader-design.md
git commit -m "Add CLI with account commands and serve wiring"
git log -1 --format=%B
```
