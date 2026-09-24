//! The `trader` zyris capability: what an Attacca agent can do with ATrader.

pub mod dto;

use std::sync::Arc;

use chrono::{DateTime, Utc};
use rust_decimal::Decimal;
use serde_json::json;
use zyris::{ErrorCode, Payload};

pub use dto::*;

use crate::app::{App, krw_per};
use crate::broker::{OrderError, OrderRequest, OrderType, Tif};
use crate::domain::{Currency, InstrumentId, Level, Venue};
use crate::sim::Size;

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
        let row = self.app.agent_account(&order.account)?;
        let req = to_request(&order)?;
        self.known(&order.instrument)?;
        self.app.market.ensure_fresh(&req.instrument).await.map_err(|e| upstream(format!("{e:#}")))?;
        self.app.broker.estimate(&row.id, &req).map(EstimateView::from).map_err(order_error)
    }

    async fn place_order(&self, order: OrderInput) -> zyris::Result<PlaceResult> {
        let row = self.app.agent_account(&order.account)?;
        let req = to_request(&order)?;
        self.known(&order.instrument)?;
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
}
