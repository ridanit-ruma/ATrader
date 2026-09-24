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

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct PeriodView {
    /// `FY2025` or `2026Q2`.
    pub period: String,
    pub end: chrono::NaiveDate,
    pub revenue: Option<Decimal>,
    pub operating_income: Option<Decimal>,
    pub net_income: Option<Decimal>,
    pub eps: Option<Decimal>,
    /// Total equity at period end.
    pub equity: Option<Decimal>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct FinancialsView {
    pub id: String,
    /// `DART` (KRX) or `SEC EDGAR` (US).
    pub source: String,
    /// Currency of every amount.
    pub currency: String,
    /// Newest first, up to 3 fiscal years.
    pub annual: Vec<PeriodView>,
    /// The latest quarter's own figures (not year-to-date).
    pub latest_quarter: Option<PeriodView>,
    pub shares_outstanding: Option<Decimal>,
    /// `reported` (EDGAR diluted EPS) or `computed` (DART: net income / shares outstanding).
    pub eps_basis: String,
    /// Current simulated mid price used for the ratios.
    pub price: Option<Decimal>,
    /// Price / latest annual EPS; absent for losses.
    pub per: Option<Decimal>,
    /// Price / (latest annual equity / shares).
    pub pbr: Option<Decimal>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct FilingView {
    /// Pass to `get_filing`.
    pub filing_id: String,
    pub title: String,
    pub form: String,
    pub date: chrono::NaiveDate,
    pub url: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct FilingText {
    pub filing_id: String,
    pub page: u32,
    pub pages: u32,
    /// Plain text, up to 20,000 characters.
    pub text: String,
}

/// An alert to create. Kinds and what they need:
/// `price_above` / `price_below`: instrument + threshold (price);
/// `move`: instrument + threshold (percent) + window_minutes (1-240);
/// `volume_surge`: instrument + threshold (multiple of the average pace, >= 1) + window_minutes;
/// `order_filled`: optional instrument; `session_open` / `session_close`: venue (KRX or US).
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct AlertInput {
    pub kind: String,
    #[serde(default)]
    pub instrument: Option<String>,
    #[serde(default)]
    pub venue: Option<String>,
    #[serde(default)]
    pub threshold: Option<Decimal>,
    #[serde(default)]
    pub window_minutes: Option<u32>,
    /// What you want to remember when it fires — your plan for this moment.
    pub note: String,
    /// Fire once and turn off (default true); false re-arms after a 5-minute cooldown.
    #[serde(default)]
    pub once: Option<bool>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct AlertView {
    pub id: i64,
    pub kind: String,
    pub instrument: Option<String>,
    pub venue: Option<String>,
    pub threshold: Option<Decimal>,
    pub window_minutes: Option<u32>,
    pub note: String,
    pub once: bool,
    pub active: bool,
    pub created_at: DateTime<Utc>,
    pub last_fired_at: Option<DateTime<Utc>>,
}

impl AlertView {
    pub fn from_alert(a: &crate::alerts::Alert, active: bool) -> Self {
        let c = &a.condition;
        AlertView {
            id: a.id,
            kind: c.kind().into(),
            instrument: c.instrument().map(|i| i.to_string()),
            venue: c.venue().map(|v| v.tag().into()),
            threshold: c.threshold(),
            window_minutes: c.window_minutes(),
            note: a.note.clone(),
            once: a.once,
            active,
            created_at: a.created_at,
            last_fired_at: a.last_fired_at,
        }
    }
}
