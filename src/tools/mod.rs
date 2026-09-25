//! The `trader` zyris capability: what an Attacca agent can do with ATrader.

pub mod dto;

use std::sync::Arc;

use chrono::{DateTime, Utc};
use rust_decimal::Decimal;
use serde_json::json;
use zyris::{ErrorCode, Payload};

pub use dto::*;
pub mod schema;
pub use schema::{Portable, portable};

use crate::app::{App, value_account};
use crate::broker::{OrderError, OrderRequest, OrderType, Tif};
use crate::domain::{Currency, InstrumentId, Level, Venue};
use crate::sim::Size;
use crate::candles::{Candle, Interval, resample};
use crate::indicators::{IndicatorSpec, compute};
use crate::performance::performance;
use crate::screen::Ranking;

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

    /// OHLCV candles, oldest first (the last may still be forming). `interval`: 1m, 5m, 15m, 1h,
    /// 1d, 1w. `limit` default 100, at most 200; KRX/US daily and weekly candles return at most
    /// 100. KRX/US minute candles exist only for periods when ATrader was streaming that stock.
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

    /// Company financials: up to 3 fiscal years and the latest quarter (revenue, operating and
    /// net income, EPS, equity), shares outstanding, and PER/PBR at the current price. KRX
    /// (DART) and US (SEC EDGAR) stocks only.
    async fn get_financials(&self, id: String) -> zyris::Result<FinancialsView>;

    /// Recent disclosures and filings, newest first. `since` is YYYY-MM-DD (default: 90 days
    /// ago); `limit` default 20, at most 100.
    async fn list_filings(&self, id: String, since: Option<String>, limit: Option<u32>) -> zyris::Result<Vec<FilingView>>;

    /// A filing's text, 20,000 characters per page (`page` from 1; the answer says how many
    /// pages exist).
    async fn get_filing(&self, id: String, filing_id: String, page: Option<u32>) -> zyris::Result<FilingText>;

    /// Get woken up when something happens: a price level, a % move, a volume surge, one of
    /// your orders filling, or a stock market opening/closing. When it fires, this account's
    /// Attacca session receives a message with your note. At most 50 active alerts per account.
    async fn create_alert(&self, account: String, alert: AlertInput) -> zyris::Result<AlertView>;

    /// Active alerts of an account.
    async fn list_alerts(&self, account: String) -> zyris::Result<Vec<AlertView>>;

    /// Turn an alert off.
    async fn delete_alert(&self, account: String, alert_id: i64) -> zyris::Result<AlertView>;
}

pub struct TraderTools {
    app: Arc<App>,
    /// The dashboard sees every account; the agent only those with an `agent_id`.
    any_account: bool,
}

impl TraderTools {
    fn alert_condition(&self, a: &AlertInput) -> zyris::Result<crate::alerts::Condition> {
        use crate::alerts::Condition;
        let inst = || -> zyris::Result<InstrumentId> { self.known(a.instrument.as_deref().ok_or_else(|| bad(format!("{} needs an instrument", a.kind)))?) };
        let threshold = |what: &str| a.threshold.filter(|t| *t > Decimal::ZERO).ok_or_else(|| bad(format!("{} needs a positive threshold ({what})", a.kind)));
        let window = || a.window_minutes.filter(|w| (1..=240).contains(w)).ok_or_else(|| bad(format!("{} needs window_minutes between 1 and 240", a.kind)));
        let venue = || -> zyris::Result<Venue> {
            match a.venue.as_deref().map(parse_venue).transpose()? {
                Some(v) if v.has_session() => Ok(v),
                _ => Err(bad(format!("{} needs venue KRX or US", a.kind))),
            }
        };
        Ok(match a.kind.trim() {
            "price_above" => Condition::PriceAbove { id: inst()?, price: threshold("price")? },
            "price_below" => Condition::PriceBelow { id: inst()?, price: threshold("price")? },
            "move" => Condition::Move { id: inst()?, pct: threshold("percent")?, window_minutes: window()? },
            "volume_surge" => {
                let f = threshold("multiple of the average pace")?;
                if f < Decimal::ONE {
                    return Err(bad("volume_surge threshold must be at least 1"));
                }
                Condition::VolumeSurge { id: inst()?, factor: f, window_minutes: window()? }
            }
            "order_filled" => Condition::OrderFilled { id: a.instrument.as_deref().map(|i| self.known(i)).transpose()? },
            "session_open" => Condition::SessionOpen { venue: venue()? },
            "session_close" => Condition::SessionClose { venue: venue()? },
            other => return Err(bad(format!("unknown alert kind {other:?}"))),
        })
    }

    pub fn new(app: Arc<App>) -> Self {
        TraderTools { app, any_account: false }
    }

    pub fn for_dashboard(app: Arc<App>) -> Self {
        TraderTools { app, any_account: true }
    }

    fn account(&self, id: &str) -> zyris::Result<crate::store::AccountRow> {
        if self.any_account {
            self.app.account(id).ok_or_else(|| order_error(OrderError::UnknownAccount))
        } else {
            self.app.agent_account(id)
        }
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

fn feed_error(e: anyhow::Error) -> zyris::Error {
    let msg = format!("{e:#}");
    match msg.strip_prefix("unsupported: ") {
        Some(rest) => order_error(OrderError::InvalidRequest(rest.to_string())),
        None => upstream(msg),
    }
}

fn not_enabled(what: &str) -> zyris::Error {
    order_error(OrderError::InvalidRequest(what.to_string()))
}

fn period_view(p: &crate::fundamentals::PeriodFinancials) -> PeriodView {
    PeriodView { period: p.label.clone(), end: p.end, revenue: p.revenue, operating_income: p.operating_income, net_income: p.net_income, eps: p.eps, equity: p.equity }
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

impl TraderTools {
    async fn candles(&self, id: &InstrumentId, interval: Interval, limit: usize) -> zyris::Result<Vec<Candle>> {
        if interval.is_intraday() && id.venue.has_session() {
            let since = crate::candles::lookback_start(self.app.broker.now(), interval, limit);
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

    /// Parse an id and require a loaded instrument, so typos fail fast and are never subscribed.
    fn known(&self, raw: &str) -> Result<InstrumentId, zyris::Error> {
        let id = parse_id(raw)?;
        if self.app.broker.instrument(&id).is_none() {
            return Err(coded(
                "UNKNOWN_INSTRUMENT",
                format!("no instrument {id}; find ids with search_instruments"),
                json!({}),
            ));
        }
        Ok(id)
    }

    async fn valuation(&self, account: &str) -> zyris::Result<(AccountSummary, Vec<PositionView>)> {
        let row = self.account(account)?;
        let pf = self.app.broker.portfolio(&row.id).ok_or_else(|| order_error(OrderError::UnknownAccount))?;
        let usd_krw = self.app.fx.usd_krw().await.map_err(|e| upstream(format!("{e:#}")))?;
        for (id, p) in &pf.positions {
            if p.qty.is_zero() {
                continue;
            }
            if let Err(e) = self.app.market.ensure_fresh(id).await {
                tracing::debug!(instrument = %id, error = %e, "valuing without fresh data");
            }
        }
        let v = value_account(&self.app.broker, &pf, usd_krw);
        let mut lines = v.lines;
        lines.sort_by(|a, b| b.value_krw.cmp(&a.value_krw));
        let positions = lines
            .into_iter()
            .map(|l| {
                let cur = l.id.venue.currency();
                let cost = l.qty * l.avg_cost;
                PositionView {
                    instrument: l.id.to_string(),
                    name: self.app.broker.instrument(&l.id).map(|i| i.name).unwrap_or_default(),
                    currency: cur.code().into(),
                    qty: l.qty,
                    avg_cost: l.avg_cost.round_dp(8),
                    price: l.price,
                    market_value: l.value,
                    unrealized_pnl: (l.value - cost).round_dp(cur.decimals()),
                    unrealized_pct: if cost.is_zero() { Decimal::ZERO } else { ((l.value - cost) / cost * Decimal::ONE_HUNDRED).round_dp(2) },
                    weight_pct: if v.equity_krw.is_zero() { Decimal::ZERO } else { (l.value_krw / v.equity_krw * Decimal::ONE_HUNDRED).round_dp(2) },
                }
            })
            .collect();
        let cash = [Currency::Krw, Currency::Usd, Currency::Usdt]
            .into_iter()
            .filter_map(|c| pf.cash.get(&c).map(|b| CashView { currency: c.code().into(), balance: *b, available: pf.available_cash(c) }))
            .collect();
        let summary = AccountSummary {
            id: row.id,
            name: row.name,
            cash,
            positions_value_krw: v.positions_krw.round_dp(0),
            equity_krw: v.equity_krw.round_dp(0),
            usd_krw,
            as_of: self.app.broker.now(),
        };
        Ok((summary, positions))
    }

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
            if self.app.broker.instrument(id).is_none() {
                out.push(Quote {
                    id: id.to_string(),
                    currency: id.venue.currency().code().into(),
                    stale: true,
                    error: Some("unknown instrument; find ids with search_instruments".into()),
                    ..Default::default()
                });
                continue;
            }
            let error = self.app.market.ensure_fresh(id).await.err().map(|e| format!("{e:#}"));
            out.push(self.quote(id, error));
        }
        Ok(out)
    }

    async fn get_orderbook(&self, id: String, depth: Option<u32>) -> zyris::Result<OrderBook> {
        let id = self.known(&id)?;
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

    async fn estimate_order(&self, order: OrderInput) -> zyris::Result<EstimateView> {
        let row = self.account(&order.account)?;
        let req = to_request(&order)?;
        self.known(&order.instrument)?;
        self.app.market.ensure_fresh(&req.instrument).await.map_err(|e| upstream(format!("{e:#}")))?;
        self.app.broker.estimate(&row.id, &req).map(EstimateView::from).map_err(order_error)
    }

    async fn place_order(&self, order: OrderInput) -> zyris::Result<PlaceResult> {
        let row = self.account(&order.account)?;
        let req = to_request(&order)?;
        self.known(&order.instrument)?;
        self.app.market.ensure_fresh(&req.instrument).await.map_err(|e| upstream(format!("{e:#}")))?;
        let (placed, fills) = self.app.broker.place_sync(&row.id, req).map_err(order_error)?;
        self.app.market.refresh_pins();
        Ok(PlaceResult { order: OrderView::from(&placed), fills: fills.iter().map(FillView::from).collect() })
    }

    async fn cancel_order(&self, account: String, order_id: u64) -> zyris::Result<OrderView> {
        let row = self.account(&account)?;
        let order = self.app.broker.cancel_sync(&row.id, order_id).map_err(order_error)?;
        self.app.market.refresh_pins();
        Ok(OrderView::from(&order))
    }

    async fn list_orders(&self, account: String, open_only: Option<bool>, limit: Option<u32>) -> zyris::Result<Vec<OrderView>> {
        let row = self.account(&account)?;
        let orders = self
            .app
            .store
            .orders(&row.id, row.generation, open_only.unwrap_or(false), clamp_limit(limit))
            .await
            .map_err(upstream)?;
        Ok(orders.iter().map(OrderView::from).collect())
    }

    async fn list_fills(&self, account: String, since: Option<DateTime<Utc>>, limit: Option<u32>) -> zyris::Result<Vec<FillView>> {
        let row = self.account(&account)?;
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
        let row = self.account(&account)?;
        let (from, to) = (parse_currency(&from)?, parse_currency(&to)?);
        let usd_krw = self.app.fx.usd_krw().await.map_err(|e| upstream(format!("{e:#}")))?;
        let c = self.app.broker.convert_sync(&row.id, from, to, amount, usd_krw, self.app.fx_spread).map_err(order_error)?;
        Ok(ConversionView::from(&c))
    }

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
        let row = self.account(&account)?;
        let days = match period.as_str() {
            "1d" => Some(1),
            "1w" => Some(7),
            "1m" => Some(30),
            "3m" => Some(90),
            "all" => None,
            _ => return Err(bad("period must be 1d, 1w, 1m, 3m or all")),
        };
        let since = days.map(|d| self.app.broker.now() - chrono::Duration::days(d));
        // Minute rows only matter for a single day; longer periods use the daily closes.
        let snaps = self.app.store.snapshots(&row.id, row.generation, since, days.is_some_and(|d| d > 1)).await.map_err(upstream)?;
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

    async fn get_financials(&self, id: String) -> zyris::Result<FinancialsView> {
        let id = self.known(&id)?;
        let today = self.app.broker.now().date_naive();
        let (source, f) = match id.venue {
            Venue::Krx => {
                let dart = self.app.dart.as_ref().ok_or_else(|| not_enabled("KRX financials need DART_API_KEY on the server"))?;
                ("DART", dart.fundamentals(&id.symbol, today).await.map_err(|e| upstream(format!("{e:#}")))?)
            }
            Venue::Us => {
                let edgar = self.app.edgar.as_ref().ok_or_else(|| not_enabled("US financials need EDGAR_USER_AGENT on the server"))?;
                ("SEC EDGAR", edgar.fundamentals(&id.symbol).await.map_err(|e| upstream(format!("{e:#}")))?)
            }
            _ => return Err(not_enabled("financials exist for KRX and US stocks only")),
        };
        let _ = self.app.market.ensure_fresh(&id).await;
        let price = self.app.broker.book_view(&id, 1).and_then(|v| mid(&v.shadow_bids, &v.shadow_asks));
        let r = price.map(|p| crate::fundamentals::ratios(p, &f));
        Ok(FinancialsView {
            id: id.to_string(),
            source: source.into(),
            currency: f.currency.code().into(),
            annual: f.annual.iter().map(period_view).collect(),
            latest_quarter: f.latest_quarter.as_ref().map(period_view),
            shares_outstanding: f.shares_outstanding,
            eps_basis: if f.eps_computed { "computed" } else { "reported" }.into(),
            price,
            per: r.and_then(|r| r.per),
            pbr: r.and_then(|r| r.pbr),
        })
    }

    async fn list_filings(&self, id: String, since: Option<String>, limit: Option<u32>) -> zyris::Result<Vec<FilingView>> {
        let id = self.known(&id)?;
        let since = match since {
            Some(s) => chrono::NaiveDate::parse_from_str(s.trim(), "%Y-%m-%d").map_err(|_| bad("since must be YYYY-MM-DD"))?,
            None => self.app.broker.now().date_naive() - chrono::Duration::days(90),
        };
        let limit = limit.unwrap_or(20).clamp(1, 100) as usize;
        let filings = match id.venue {
            Venue::Krx => {
                let dart = self.app.dart.as_ref().ok_or_else(|| not_enabled("KRX filings need DART_API_KEY on the server"))?;
                dart.filings(&id.symbol, since, limit).await
            }
            Venue::Us => {
                let edgar = self.app.edgar.as_ref().ok_or_else(|| not_enabled("US filings need EDGAR_USER_AGENT on the server"))?;
                edgar.filings(&id.symbol, Some(since), limit).await
            }
            _ => return Err(not_enabled("filings exist for KRX and US stocks only")),
        }
        .map_err(|e| upstream(format!("{e:#}")))?;
        Ok(filings.into_iter().map(|f| FilingView { filing_id: f.id, title: f.title, form: f.form, date: f.date, url: f.url }).collect())
    }

    async fn get_filing(&self, id: String, filing_id: String, page: Option<u32>) -> zyris::Result<FilingText> {
        let id = self.known(&id)?;
        let text = match id.venue {
            Venue::Krx => self.app.dart.as_ref().ok_or_else(|| not_enabled("KRX filings need DART_API_KEY on the server"))?.filing_text(filing_id.trim()).await,
            Venue::Us => self.app.edgar.as_ref().ok_or_else(|| not_enabled("US filings need EDGAR_USER_AGENT on the server"))?.filing_text(&id.symbol, filing_id.trim()).await,
            _ => return Err(not_enabled("filings exist for KRX and US stocks only")),
        }
        .map_err(|e| upstream(format!("{e:#}")))?;
        let page = page.unwrap_or(1);
        let (chunk, pages) = crate::fundamentals::page_text(&text, page as usize).map_err(bad)?;
        Ok(FilingText { filing_id, page, pages: pages as u32, text: chunk })
    }

    async fn create_alert(&self, account: String, alert: AlertInput) -> zyris::Result<AlertView> {
        let row = self.account(&account)?;
        let condition = self.alert_condition(&alert)?;
        if alert.note.trim().is_empty() {
            return Err(bad("note is required: say what you want to do when it fires"));
        }
        let active = self.app.store.active_alerts(&row.id, row.generation).await.map_err(upstream)?;
        if active.len() >= 50 {
            return Err(order_error(OrderError::InvalidRequest("50 active alerts is the limit; delete some first".into())));
        }
        let mut a = crate::alerts::Alert {
            id: 0,
            account: row.id.clone(),
            generation: row.generation,
            condition,
            note: alert.note.trim().to_string(),
            once: alert.once.unwrap_or(true),
            created_at: self.app.broker.now(),
            last_fired_at: None,
        };
        a.id = self.app.store.create_alert(&a).await.map_err(upstream)?;
        if let Some(tx) = &self.app.alerts {
            let _ = tx.send(crate::alerts::deliver::AlertCmd::Upsert(a.clone()));
        }
        Ok(AlertView::from_alert(&a, true))
    }

    async fn list_alerts(&self, account: String) -> zyris::Result<Vec<AlertView>> {
        let row = self.account(&account)?;
        let alerts = self.app.store.active_alerts(&row.id, row.generation).await.map_err(upstream)?;
        Ok(alerts.iter().map(|a| AlertView::from_alert(a, true)).collect())
    }

    async fn delete_alert(&self, account: String, alert_id: i64) -> zyris::Result<AlertView> {
        let row = self.account(&account)?;
        let active = self.app.store.active_alerts(&row.id, row.generation).await.map_err(upstream)?;
        let a = active.into_iter().find(|a| a.id == alert_id).ok_or_else(|| order_error(OrderError::NotFound))?;
        self.app.store.deactivate_alert(&row.id, alert_id).await.map_err(upstream)?;
        if let Some(tx) = &self.app.alerts {
            let _ = tx.send(crate::alerts::deliver::AlertCmd::Remove(alert_id));
        }
        Ok(AlertView::from_alert(&a, false))
    }
}
