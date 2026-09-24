# ATrader — Design Spec

Date: 2026-09-24
Status: Draft for review

## 1. Purpose

ATrader lets an Attacca agent trade: first on a paper-trading simulator that runs on real market
data, and later on a real brokerage account the user connects. It reaches Attacca as a zyris node,
and it gives the user a web dashboard that shows what the agent did and how the accounts perform.

It exists to test how well an Attacca agent trades when it relies on its own strengths: collecting
data, analysing it, and remembering. So the main requirement is that the agent gets enough
**structured market data and well-designed tools**. The agent's research (news, web) and memory
stay in Attacca. ATrader owns market data, execution, and accounting.

### Decisions made during brainstorming

| Topic | Decision |
| --- | --- |
| Markets | Korea (KRX), US (NYSE/NASDAQ/AMEX), crypto (Upbit KRW, Binance USDT) |
| Market data | KIS Open API for stocks (real-time quotes, order book, trades); Upbit and Binance public WebSockets for crypto |
| Market impact | Shadow order book over the real one: depletion that recovers + permanent offset that decays |
| Users / accounts | One user, many paper accounts (one per agent/strategy), resettable |
| Extra data for the AI | Candles + indicators, filings + financials (DART, SEC EDGAR), screener/rankings, event push to Attacca |
| Access | Home server, reachable only over Tailscale; login is still mandatory |
| Time | Real time only. No replay or backtest in v1 (the `Clock` seam allows adding it later) |
| Architecture | One Rust binary, a modular monolith, Postgres, React SPA served by the same binary |

### Non-goals (v1)

- Real-money trading. Only the `Broker` seam exists (see §9).
- Short selling, margin, derivatives, fractional US shares.
- Pre-market, after-hours, and KRX opening/closing auctions. Stocks trade in the regular session only.
- Replay or backtesting.
- Multiple users and read-only sharing links.
- Accounts matching orders against each other. Every account trades against the shadow book of
  the real market, never against another account's resting orders.

## 2. Architecture

```
                     Tailscale (tailscale serve, HTTPS)
                                 │
┌──────────────────────────── atrader (one process) ───────────────────────────┐
│                                                                              │
│  web ── axum: /api/*, /api/stream (SSE), static SPA, auth                    │
│   │                                                                          │
│  tools ── zyris capability "trader" v1 ◄──── wss ──── Attacca                 │
│   │                                                                          │
│  alerts ── watcher ──► attacca_api.create_session_with / send_message ──►    │
│   │                                                                          │
│  broker ── trait Broker ── SimBroker (v1)   [KisBroker: later]               │
│   │            │                                                             │
│  sim ── shadow book, matching, impact (pure, no I/O)                         │
│   │                                                                          │
│  ledger ── accounts, cash, positions, PnL, snapshots                          │
│   │                                                                          │
│  market ── trait MarketFeed: kis, upbit, binance  ·  fx  ·  instruments      │
│  research ── candles, indicators, screener, dart, edgar                      │
│                                                                              │
└──────────────────────────────── Postgres ────────────────────────────────────┘
```

- **One crate, `atrader`**, split into modules along the boxes above. A dependency may only point
  down the diagram. `sim` is pure: it takes book and trade events and returns fills, so it can be
  tested without I/O.
- **Internal bus:** a `tokio::sync::broadcast` channel of `MarketEvent` (book, trade, status) and
  one of `AccountEvent` (order, fill, equity). The SSE stream and the alert watcher subscribe to
  them. No external message broker.
- **Clock:** `trait Clock { fn now(&self) -> DateTime<Utc> }`. Production uses the system clock and
  tests use a manual one. This is the only concession made to a future replay mode.
- **Money:** `rust_decimal::Decimal` holds every price, quantity, and amount. Floats appear only
  in indicator maths.
- **Stack:** tokio, axum, sqlx (Postgres), serde, schemars, tokio-tungstenite, reqwest, and
  argon2 + totp-rs for auth. The zyris crates are pinned to the same revision `zyris-docker` uses.
  The frontend is React 19, Vite, Tailwind v4, TanStack Query, and lightweight-charts.
- **Template:** `~/zyris-docker` supplies the node bootstrap, the credential handling
  (`ZYRIS_CREDENTIAL` / `ZYRIS_CREDENTIAL_FILE`, with enroll as the fallback), and the
  `attacca_api` client usage (`heal.rs`).

## 3. Market model

### Instruments

The ID is `{VENUE}:{SYMBOL}`. Examples: `KRX:005930`, `US:AAPL`, `UPBIT:KRW-BTC`, `BINANCE:BTCUSDT`.

The master list refreshes daily:
- KRX and US: the KIS master files.
- Upbit: `/v1/market/all`.
- Binance: `exchangeInfo`.

Each instrument stores its name (Korean and English where available), venue, quote currency,
tick rule, lot rule, and status.

### Venue rules

| Rule | KRX | US | Upbit | Binance |
| --- | --- | --- | --- | --- |
| Currency | KRW | USD | KRW | USDT |
| Session | 09:00–15:30 KST, business days | 09:30–16:00 ET, NYSE calendar | 24/7 | 24/7 |
| Tick | KRX price-band table | $0.01 (≥ $1), $0.0001 below | Upbit KRW tick table | `PRICE_FILTER.tickSize` |
| Quantity | integer shares | integer shares | 8 decimals, min order ₩5,000 | `LOT_SIZE.stepSize`, `MIN_NOTIONAL` |
| Price limit | ±30% of previous close | none (LULD ignored) | none | none |
| Fees (default) | 0.015% commission | $0 commission + SEC fee on sells | 0.05% | 0.1% |
| Tax (default) | securities transaction tax on sells | none | none | none |

- **Fees and taxes live in a config table** (`fees.toml`) with an effective date. The tax rates
  change by law, so the defaults are checked against the current law when that table is
  implemented, and whoever changes them updates it.
- **Holidays:** a yearly list of KRX and NYSE holidays in `holidays.toml`.

### FX

- **Balances:** each account keeps separate cash balances in KRW, USD and USDT.
- **Conversion:** `convert_currency` converts between them at the reference rate ± a configurable
  spread (default 0.1%).
- **Rate source:** the USD/KRW rate comes from Frankfurter (ECB, free, updated daily), cached.
- **USDT:** valued at 1 USD for valuation, and converted at the same rule.
- **Account value:** KRW is the valuation currency for account totals.

## 4. Market data

- **`trait MarketFeed`** exposes three calls:
  - `subscribe(ids)` / `unsubscribe(ids)`, which push `BookUpdate` and `Trade` events onto the bus.
  - `snapshot(id) -> Book`, over REST.
  - `status(venue)`.
- **Implementations:**
  - `KisFeed`:
    - Token issuance is rate-limited by KIS, so the token is cached and reused until it expires.
    - WebSocket approval key.
    - KRX and US real-time order book + trade TRs.
    - REST snapshots.
  - `UpbitFeed`: public WS with `orderbook` + `trade`.
  - `BinanceFeed`: `@depth20@100ms` + `@trade`.
- **Subscription budget:** a KIS WebSocket session allows only a limited number of registrations.
  `SubscriptionManager` keeps the active set within a per-feed cap and fills it by priority:
  1. Instruments with open positions or resting orders.
  2. Instruments with active alerts.
  3. Recently queried instruments (LRU).

  An instrument outside the set is served from a REST snapshot. An order on such an instrument
  first takes a snapshot, then subscribes.
- **Staleness:** every book carries `received_at`. If an instrument's book is older than its
  venue's staleness limit (default 5 s during a session), orders on it are rejected with
  `STALE_DATA` and quotes are marked `stale: true`.
- **Reconnect:** exponential backoff with jitter, then every active subscription is re-registered.
  Feed health is exposed at `/api/health` and on the dashboard.
- **Bars:** trades on subscribed instruments are rolled into our own 1-minute bars and stored.
  Longer history comes from the venue APIs and is cached.

## 5. Simulator: shadow order book

There is one shared simulated world. Every account's orders act on the same shadow state for each
instrument.

### State per instrument (in memory)

- `offset: Decimal`. This is the permanent impact, a relative price shift. The shadow price is
  the real price × (1 + offset), rounded to a valid tick. It decays toward 0 with half-life
  `τ_perm` (default 30 min stocks, 10 min crypto).
- `depletion: Map<price_level, qty>`. This is liquidity our fills removed. It decays with
  half-life `τ_res` (default 60 s).
- **Shadow book:** each real level is shifted by `offset` and loses its depletion; a level
  cannot drop below zero quantity.

The state is not persisted. After a restart it starts at zero. That is acceptable because it would
decay within minutes anyway.

### Orders

- **Types:** `market` and `limit`.
- **Time in force:** `day` (stocks, expires at session close), `gtc` (crypto only), and `ioc`.
- **Size:** quantity, or for market buys a notional amount.
- **Market order:** it walks the shadow book and fills level by level, and each fill adds to
  `depletion`. Whatever the visible book cannot fill is cancelled (IOC), and the response reports
  the unfilled quantity. A market order that would move the price more than
  `max_slippage` (default 5%) from the pre-trade mid stops there.
- **Permanent impact** after each fill, from the square-root law:
  `Δoffset = side × η × σ_daily × sqrt(fill_notional / ADV_notional)`.
  - `η` defaults to 0.5 and is configurable per venue.
  - `σ_daily` is the 20-day volatility of daily returns.
  - `ADV` is the 20-day average daily traded value.

  Both are cached daily. When they are missing (a new listing), conservative defaults apply.
- **Limit order:**
  - The marketable part executes like a market order.
  - The rest rests with `queue_ahead` equal to the shadow quantity already at that price.
  - When a real trade prints at our price, it reduces `queue_ahead` first, and any leftover
    trade quantity fills us.
  - When a trade prints through our price, or the shadow opposite side crosses it, we fill at
    our limit, up to the trade or level quantity.
- **Validation:** before acceptance, in this order.
  1. The instrument exists and is tradable.
  2. The session is open. Otherwise `MARKET_CLOSED` with `next_open`.
  3. Data is fresh.
  4. Tick and lot sizes are valid. Otherwise `INVALID_TICK` with the nearest valid prices.
  5. Price limits.
  6. Cash is enough, including estimated fees (buys) or the position is enough (sells). Otherwise
     `INSUFFICIENT_FUNDS` with the amount available.
- **Reservations:** accepted orders reserve cash or shares until they fill or are cancelled.
- **Dry run:** `estimate_order` runs the same walk on a copy of the state. It returns the expected
  average price, slippage in bps, fees, taxes, and the expected permanent impact, and changes
  nothing. This is how the agent learns that splitting a large order pays.

## 6. Ledger and accounting

- **Account** fields: `id`, `name`, `agent_id` (the Attacca agent that trades it and receives its
  alerts), `allowed_venues`, `initial_cash` per currency, `generation`, `broker` (`sim` in v1),
  and `created_at`.
- **Append-only `ledger_entries`:** deposit, fill cash leg, fee, tax, fx, reset. Cash balances are
  sums of these entries. Positions are materialised from fills in the same transaction.
- **Cost basis:** moving average cost, the standard for Korean brokers. Realised PnL on a sell is
  `(price − avg_cost) × qty − fees − taxes`, in the instrument's currency and also converted to
  KRW at the fill-time rate.
- **Equity snapshots:**
  - Every minute (crypto trades around the clock, so there is always a market open).
  - Once a day at 00:00 KST as the daily close.
  - Daily PnL is the change from one daily close to the next.
- **Reset:** `generation` goes up and fresh initial cash is deposited. Old rows are kept and
  filtered by generation, so a strategy's history survives a restart of the experiment.
- **Order reason:** every order stores a `reason` text written by the agent. The dashboard shows it
  next to each fill, so the user can see why the agent traded.

## 7. AI tools: capability `trader` v1

Tool descriptions come from doc comments, as in `zyris-docker/src/monitor.rs`. They state units,
currency, and what to call next.

**Response conventions:**
- Every price carries its currency.
- Every snapshot carries `as_of` and `stale`.
- Lists are capped and paginated.
- Errors are typed codes with the fields needed to recover, as listed in §5.

| Group | Tool | Purpose |
| --- | --- | --- |
| Discovery | `search_instruments(query, venue?)` | Name/symbol search in Korean or English |
| | `market_status(venue?)` | Open/closed, next open/close, feed health |
| | `screen(venue, ranking, filters?, limit)` | Top gainers, losers, volume, value, volume surge. KIS ranking APIs for stocks; computed from all-ticker endpoints for crypto |
| Quotes | `get_quotes(ids[])` | Last, change %, volume, bid/ask, real vs shadow price |
| | `get_orderbook(id, depth?)` | Shadow book with the real book side by side |
| | `get_candles(id, interval, limit, before?)` | 1m/5m/15m/1h/1d/1w OHLCV |
| | `get_indicators(id, interval, names[])` | SMA, EMA, RSI, MACD, Bollinger, ATR, volatility, computed on the server |
| Fundamentals | `get_financials(id)` | Normalised annual and quarterly revenue, operating income, net income, EPS, equity, plus PER and PBR at the current price. DART for KRX, EDGAR for US, not available for crypto |
| | `list_filings(id, since?)` | Disclosures with type, title, date, id |
| | `get_filing(id, filing_id, page?)` | Filing text as plain text, paginated |
| Trading | `estimate_order(account, id, side, type, qty\|notional, limit_price?)` | Dry run (see §5) |
| | `place_order(account, id, side, type, qty\|notional, limit_price?, tif?, reason)` | `reason` is required |
| | `cancel_order(account, order_id)` | |
| | `list_orders(account, status?)` | |
| | `list_fills(account, since?)` | |
| Account | `list_accounts()` | The accounts this node serves |
| | `get_account(account)` | Cash per currency, equity (KRW), day and total PnL, buying power |
| | `get_positions(account)` | Quantity, average cost, price, unrealised PnL, weight |
| | `get_performance(account, period)` | Return, max drawdown, volatility, Sharpe, win rate, turnover |
| | `convert_currency(account, from, to, amount)` | FX at reference rate ± spread |
| Alerts | `create_alert(account, condition, note, once?)` | Conditions: price above/below, % move over a window, volume surge, order filled, session open/close |
| | `list_alerts(account)`, `delete_alert(alert_id)` | |

The agent cannot create, reset, or delete accounts. Only the user does that, in the dashboard.

## 8. Event push to Attacca

- **Evaluation:** the alert watcher subscribes to the bus and checks each active alert's condition
  on every relevant event.
- **Delivery:** when an alert fires, it gets `attacca_api` from the live connection with
  `conn.wait_capability::<AttaccaApiClient>` (the same pattern as `zyris-docker/src/heal.rs`). It
  opens or reuses one session per account against the account's `agent_id`, and then calls
  `send_message`. The message states what fired, the current quote, the account summary, and the
  note the agent attached when it created the alert.
- **Rate limits:** at most 1 push per alert per 5 minutes and at most 20 pushes per account per
  hour. Pushes beyond that are coalesced into one digest message.
- **Failures:** failed pushes are logged in `alert_events` and retried once. A push failure never
  affects trading.
- **Scopes requested:** `agents:read`, `sessions:write`, `sessions:read`.

## 9. The `Broker` seam for real trading

```rust
#[async_trait]
trait Broker: Send + Sync {
    async fn place(&self, account: &Account, req: OrderRequest) -> Result<Order, OrderError>;
    async fn cancel(&self, account: &Account, order_id: OrderId) -> Result<Order, OrderError>;
    async fn sync(&self, account: &Account) -> Result<BrokerState, BrokerError>; // orders, fills, balances
}
```

- **v1:** `SimBroker` is the only implementation. Tools and web handlers go through
  `Broker`, never through `sim` directly, and the account's `broker` column chooses the
  implementation.
- **Later:** a `KisBroker` reuses `KisFeed`'s credentials and adds hard guards:
  - A per-order notional cap.
  - A daily loss limit.
  - A kill switch in the dashboard.
  - A flag the user must enable per account in the dashboard.

  None of that is built in v1.

## 10. Web dashboard

The SPA lives under `web/`. `atrader` serves the built files. Live updates come over SSE
(`/api/stream`): fills, order changes, equity ticks, and feed health.

**Colour convention:** up is red and down is blue by default, the Korean convention. A setting
switches to green/red.

| Page | Content |
| --- | --- |
| Overview | Every account: equity, day PnL, total return, allocation by venue/currency, recent fills |
| Account | Equity curve (1D/1W/1M/All), daily PnL bars, positions table with ± unrealised PnL, open orders, fill history with the agent's `reason`, realised PnL by day and by symbol, performance stats |
| Instrument | Candles with this account's buy/sell markers, real vs shadow price, order book, the agent's orders on it |
| Alerts | Active alerts, fire history, push delivery status |
| Settings | Create, reset, and archive accounts; change password; manage 2FA; active sessions; audit log; feed and zyris connection status |

## 11. Security

- **Network:**
  - `atrader` binds `127.0.0.1` only.
  - `tailscale serve` provides HTTPS on the tailnet name.
  - No port is exposed on the LAN or the internet.
- **User bootstrap:** the single user is created from the CLI with `atrader user create`, which
  prompts for the password. There is no sign-up endpoint.
- **Login:**
  - Password hashing: argon2id.
  - TOTP is mandatory; it is enrolled at first login, and recovery codes are shown once.
  - Login rate limit: 5 failures per 15 minutes per IP and per user, with exponential lockout.
- **Sessions:**
  - Random 256-bit IDs, stored hashed in the DB.
  - Cookie: `HttpOnly; Secure; SameSite=Strict`.
  - Timeouts: 12 h idle, 7 d absolute.
  - The user can revoke sessions from Settings.
- **CSRF:** `SameSite=Strict`, and every mutating request must carry an `X-Requested-With`
  header.
- **Headers:** a strict CSP (`default-src 'self'`), `X-Frame-Options: DENY`, and no CORS.
- **Audit log:** logins, failures, account creates and resets, settings changes, and every order
  (with its source: agent or web).
- **Secrets:** the KIS app key and secret, the DART key, and the zyris credential come from files
  passed by systemd `LoadCredential`. They are never stored in the DB, never returned by the API,
  and never logged.
- **AI boundary:** the zyris node trusts the Attacca server's authentication of the connection.
  Tool arguments are validated like any untrusted input, and the tools can only reach accounts
  whose `agent_id` is set.

## 12. Data model (Postgres)

`users`, `user_sessions`, `recovery_codes`, `audit_log`,
`accounts`, `ledger_entries`, `positions`, `orders`, `fills`, `equity_snapshots`,
`alerts`, `alert_events`, `instruments`, `bars` (id, interval, ts, OHLCV), `daily_stats` (σ, ADV),
`filings_cache`, `financials_cache`.

Migrations are plain SQL under `migrations/`, run at startup through `sqlx::migrate!`.

## 13. Error handling

- **A feed disconnect** marks the affected instruments stale. Orders on them are rejected, and
  resting limit orders stay queued but cannot fill until data is back.
- **A zyris disconnect** triggers reconnect with backoff (the runner from `zyris-docker`).
  Trading and the dashboard keep working.
- **Order processing** runs per instrument on one task (an actor), so the book, reservations
  and fills never race. Every change is journaled, and a writer task persists the journal
  in order, with each fill's ledger write as one DB transaction. A failed write is retried up
  to 10 times; after that the event is logged and skipped. In-memory state is never rolled
  back.
- **Tool errors** return typed codes (`MARKET_CLOSED`, `STALE_DATA`, `INVALID_TICK`,
  `INSUFFICIENT_FUNDS`, `UNKNOWN_INSTRUMENT`, `NOT_FOUND`, `RATE_LIMITED`, `UPSTREAM_ERROR`) with
  recovery fields. They never return raw upstream errors.

## 14. Testing

- **`sim`:** deterministic unit tests for:
  - Walking the book and partial IOC.
  - Depletion decay and offset decay.
  - Queue-position fills.
  - Cancellation at the slippage cap.
  - Tick and lot rounding for every venue.
  - Price limits.
- **`ledger`:** tests for moving average cost, realised PnL with fees and taxes, FX conversion,
  reservations, and reset generations. The ledger tests use `sqlx::test`.
- **Feeds:** parsers are tested against recorded JSON and binary fixtures from each venue. A
  `FakeFeed` drives end-to-end tests: place order → fill → ledger → SSE event.
- **Tools:** schema snapshot tests, so tool descriptions don't drift silently, and one test per
  error code.
- **Web:** auth flow tests (login, TOTP, lockout, CSRF rejection) against the axum router, and a
  small vitest suite for PnL formatting and colour logic.
- **Manual smoke test:** connect to a local Attacca, let an agent buy and sell `UPBIT:KRW-BTC`, and
  check that the dashboard shows the fills and reasons.

## 15. Delivery order

Each phase ends in something that runs.

1. **Core:** instruments, venue rules, `sim`, `ledger`, `FakeFeed`, DB. All covered by tests.
2. **Crypto feeds:** Upbit and Binance, FX, and the subscription manager.
3. **zyris `trader` capability:** trading, account, and quote tools. An agent can trade crypto
   end to end at this point.
4. **KIS feed:** KRX and US.
5. **Research:** candles and indicators, screener, DART, EDGAR.
6. **Alerts** and pushing them to Attacca.
7. **Web:** auth, API, SSE, and the dashboard.
8. **Deploy:** a NixOS module, `tailscale serve`, and systemd credentials.
