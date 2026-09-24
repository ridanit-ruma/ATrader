//! Order entry and execution. `SimBroker` is the paper-trading implementation of `Broker`.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use chrono::{DateTime, Duration, Utc};
use rust_decimal::{Decimal, RoundingStrategy};
use rust_decimal_macros::dec;

use crate::domain::{Book, Clock, Currency, InstrumentId, Level, Side, Trade, Venue};
use crate::ledger::Portfolio;
use crate::sim::{self, DailyStats, Resting, ShadowState, SimParams, Size, Slice};
use crate::venue::{Calendar, FeeSchedule, Instrument, TickRule, price_band};

pub type AccountId = String;
pub type OrderId = u64;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OrderType {
    Market,
    Limit,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tif {
    /// Stocks: expires at session close.
    Day,
    /// Crypto: rests until filled or cancelled.
    Gtc,
    /// Fill what is possible now, cancel the rest.
    Ioc,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OrderStatus {
    Open,
    Filled,
    Cancelled,
    Expired,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Liquidity {
    Taker,
    Maker,
}

#[derive(Debug, Clone, PartialEq)]
pub struct OrderRequest {
    pub instrument: InstrumentId,
    pub side: Side,
    pub kind: OrderType,
    pub size: Size,
    pub limit_price: Option<Decimal>,
    pub tif: Tif,
    /// Why the agent placed this order; shown next to its fills.
    pub reason: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Order {
    pub id: OrderId,
    pub account: AccountId,
    pub req: OrderRequest,
    pub status: OrderStatus,
    pub filled_qty: Decimal,
    pub filled_notional: Decimal,
    pub created_at: DateTime<Utc>,
}

impl Order {
    pub fn avg_price(&self) -> Option<Decimal> {
        (!self.filled_qty.is_zero()).then(|| (self.filled_notional / self.filled_qty).round_dp(8))
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Fill {
    pub order_id: OrderId,
    pub account: AccountId,
    pub instrument: InstrumentId,
    pub side: Side,
    pub qty: Decimal,
    pub notional: Decimal,
    /// Volume-weighted average price of this execution.
    pub price: Decimal,
    pub fee: Decimal,
    pub tax: Decimal,
    pub realized_pnl: Option<Decimal>,
    pub liquidity: Liquidity,
    pub at: DateTime<Utc>,
}

/// What `place` would do right now, without doing it.
#[derive(Debug, Clone, PartialEq)]
pub struct Estimate {
    pub filled_qty: Decimal,
    pub avg_price: Option<Decimal>,
    pub notional: Decimal,
    pub fee: Decimal,
    pub tax: Decimal,
    /// Quantity that would rest as a limit order.
    pub rest_qty: Decimal,
    /// Distance of the average fill price from the pre-trade shadow mid.
    pub slippage_bps: Decimal,
    /// Permanent price shift this execution would leave behind.
    pub impact_bps: f64,
}

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

#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum OrderError {
    #[error("unknown account")]
    UnknownAccount,
    #[error("unknown instrument")]
    UnknownInstrument,
    #[error("instrument is not tradable")]
    NotTradable,
    #[error("market closed; next open {next_open:?}")]
    MarketClosed { next_open: Option<DateTime<Utc>> },
    #[error("market data is stale ({age_secs:?} s old)")]
    StaleData { age_secs: Option<i64> },
    #[error("no liquidity on the book")]
    NoLiquidity,
    #[error("price is not on a valid tick; nearest {lower} / {upper}")]
    InvalidTick { lower: Decimal, upper: Decimal },
    #[error("invalid quantity: step {step}, min qty {min_qty}, min notional {min_notional}")]
    InvalidQty { step: Decimal, min_qty: Decimal, min_notional: Decimal },
    #[error("price outside the daily limit {lower}..{upper}")]
    PriceLimit { lower: Decimal, upper: Decimal },
    #[error("insufficient funds: need {required}, available {available}")]
    InsufficientFunds { required: Decimal, available: Decimal },
    #[error("insufficient position: available {available}")]
    InsufficientPosition { available: Decimal },
    #[error("order not found")]
    NotFound,
    #[error("invalid request: {0}")]
    InvalidRequest(String),
}

// ponytail: place/cancel only; `sync` (spec §9) arrives with KisBroker, the first broker with remote state.
#[async_trait]
pub trait Broker: Send + Sync {
    async fn place(&self, account: &str, req: OrderRequest) -> Result<(Order, Vec<Fill>), OrderError>;
    async fn cancel(&self, account: &str, order_id: OrderId) -> Result<Order, OrderError>;
}

/// Everything the simulator knows. One lock guards all of it.
#[derive(Default)]
struct World {
    instruments: HashMap<InstrumentId, Instrument>,
    books: HashMap<InstrumentId, Book>,
    shadows: HashMap<InstrumentId, ShadowState>,
    stats: HashMap<InstrumentId, DailyStats>,
    accounts: HashMap<AccountId, Portfolio>,
    orders: HashMap<OrderId, Order>,
    resting: HashMap<InstrumentId, Vec<Resting>>,
    next_order_id: OrderId,
}

pub struct SimBroker {
    clock: Arc<dyn Clock>,
    calendar: Calendar,
    staleness: Duration,
    // ponytail: one global lock over the simulated world; per-instrument actors (spec §13) if order rate needs it.
    world: Mutex<World>,
}

/// The result of validating an order against the current shadow book.
struct Plan {
    slices: Vec<Slice>,
    qty: Decimal,
    notional: Decimal,
    fee: Decimal,
    tax: Decimal,
    rest_qty: Decimal,
    queue_ahead: Decimal,
    mid: Decimal,
}

/// Cash reserved per unit of a resting buy: limit price plus commission.
fn buy_reserve_per_unit(limit: Decimal, venue: Venue) -> Decimal {
    limit * (dec!(1) + FeeSchedule::default_for(venue).commission_bps / dec!(10000))
}

/// Above any real order, far below `Decimal` overflow once multiplied together.
const MAX_INPUT: Decimal = dec!(1000000000000000);

fn check_shape(req: &OrderRequest) -> Result<(), OrderError> {
    let bad = |m: &str| -> Result<(), OrderError> { Err(OrderError::InvalidRequest(m.into())) };
    let size = match req.size {
        Size::Qty(q) | Size::Notional(q) => q,
    };
    if size.abs() > MAX_INPUT || req.limit_price.is_some_and(|p| p.abs() > MAX_INPUT) {
        return bad("size or price is out of range");
    }
    match (req.kind, req.limit_price, req.size) {
        (OrderType::Limit, None, _) => return bad("limit orders need limit_price"),
        (OrderType::Limit, _, Size::Notional(_)) => return bad("limit orders are sized by qty"),
        (OrderType::Market, Some(_), _) => return bad("market orders take no limit_price"),
        (_, _, Size::Notional(_)) if req.side == Side::Sell => return bad("sell orders are sized by qty"),
        _ => {}
    }
    let crypto = !req.instrument.venue.has_session();
    match (req.kind, req.tif, crypto) {
        (OrderType::Limit, Tif::Gtc, false) => bad("gtc is only for crypto; use day"),
        (OrderType::Limit, Tif::Day, true) => bad("day is only for stocks; use gtc"),
        _ => Ok(()),
    }
}

fn is_complete(o: &Order) -> bool {
    match o.req.size {
        Size::Qty(q) => o.filled_qty >= q,
        Size::Notional(_) => o.filled_qty > Decimal::ZERO,
    }
}

/// Book one execution into the account and the order.
fn record_fill(w: &mut World, order: &mut Order, qty: Decimal, notional: Decimal, liquidity: Liquidity, now: DateTime<Utc>) -> Fill {
    let id = order.req.instrument.clone();
    let venue = id.venue;
    let (fee, tax) = FeeSchedule::default_for(venue).cost(order.req.side, notional, venue.currency().decimals());
    let pf = w.accounts.get_mut(&order.account).expect("order's account exists");
    let realized_pnl = pf.apply_fill(&id, order.req.side, qty, notional, fee, tax);
    order.filled_qty += qty;
    order.filled_notional += notional;
    Fill {
        order_id: order.id,
        account: order.account.clone(),
        instrument: id,
        side: order.req.side,
        qty,
        notional,
        price: (notional / qty).round_dp(8),
        fee,
        tax,
        realized_pnl,
        liquidity,
        at: now,
    }
}

fn shadow_consume(w: &mut World, id: &InstrumentId, taker: Side, slices: &[Slice]) {
    w.shadows.entry(id.clone()).or_default().consume(taker, slices);
}

/// Execute `slices` as a taker: deplete the book, push the price, book the fill.
fn take(w: &mut World, order: &mut Order, slices: &[Slice], now: DateTime<Utc>) -> Fill {
    let id = order.req.instrument.clone();
    let params = SimParams::default_for(id.venue);
    let stats = w.stats.get(&id).copied().unwrap_or(DailyStats { sigma: params.default_sigma, adv_notional: params.default_adv });
    let (qty, notional) = sim::totals(slices);
    let shadow = w.shadows.entry(id).or_default();
    shadow.consume(order.req.side, slices);
    shadow.offset += sim::impact(order.req.side, notional, stats, &params);
    record_fill(w, order, qty, notional, Liquidity::Taker, now)
}

/// Give back the reservation held for `qty` of a resting order.
fn release(w: &mut World, order: &Order, qty: Decimal) {
    let pf = w.accounts.get_mut(&order.account).expect("order's account exists");
    match order.req.side {
        Side::Buy => {
            let venue = order.req.instrument.venue;
            let per_unit = buy_reserve_per_unit(order.req.limit_price.unwrap_or_default(), venue);
            pf.release_cash(venue.currency(), per_unit * qty);
        }
        Side::Sell => pf.release_qty(&order.req.instrument, qty),
    }
}

/// Take a resting order off the book with `status`, returning its reservation.
fn close_order(w: &mut World, order_id: OrderId, status: OrderStatus) -> Order {
    let mut order = w.orders.remove(&order_id).expect("tracked order");
    let mut remaining = Decimal::ZERO;
    if let Some(rest) = w.resting.get_mut(&order.req.instrument) {
        if let Some(pos) = rest.iter().position(|r| r.order_id == order_id) {
            remaining = rest.remove(pos).remaining;
        }
    }
    release(w, &order, remaining);
    order.status = status;
    w.orders.insert(order_id, order.clone());
    order
}

impl SimBroker {
    pub fn new(clock: Arc<dyn Clock>, calendar: Calendar) -> Self {
        SimBroker {
            clock,
            calendar,
            staleness: Duration::seconds(5),
            world: Mutex::new(World { next_order_id: 1, ..Default::default() }),
        }
    }

    /// Continue order numbering after the highest id already persisted.
    pub fn set_next_order_id(&self, id: OrderId) {
        self.world.lock().unwrap().next_order_id = id;
    }

    /// Instruments with a non-positive tick or lot step are refused (they would divide by zero).
    pub fn add_instrument(&self, instrument: Instrument) {
        let tick_ok = !matches!(instrument.tick, TickRule::Fixed(t) if t <= Decimal::ZERO);
        if !tick_ok || instrument.lot.step <= Decimal::ZERO {
            tracing::warn!(instrument = %instrument.id, "refusing instrument with zero tick or lot step");
            return;
        }
        self.world.lock().unwrap().instruments.insert(instrument.id.clone(), instrument);
    }

    pub fn set_stats(&self, id: InstrumentId, stats: DailyStats) {
        self.world.lock().unwrap().stats.insert(id, stats);
    }

    pub fn open_account(&self, id: &str, cash: &[(Currency, Decimal)]) {
        self.world.lock().unwrap().accounts.insert(id.to_string(), Portfolio::new(cash));
    }

    pub fn portfolio(&self, id: &str) -> Option<Portfolio> {
        self.world.lock().unwrap().accounts.get(id).cloned()
    }

    pub fn order(&self, id: OrderId) -> Option<Order> {
        self.world.lock().unwrap().orders.get(&id).cloned()
    }

    /// The instrument's permanent-impact offset, decayed to now.
    pub fn shadow_offset(&self, id: &InstrumentId) -> f64 {
        let now = self.clock.now();
        let mut w = self.world.lock().unwrap();
        let Some(shadow) = w.shadows.get_mut(id) else { return 0.0 };
        shadow.decay(now, &SimParams::default_for(id.venue));
        shadow.offset
    }

    /// A new real order book arrived. Resting orders it crosses fill at their limit (spec §5).
    /// Levels with a non-positive price or quantity are dropped.
    pub fn on_book(&self, mut book: Book) -> Vec<Fill> {
        let now = self.clock.now();
        let mut guard = self.world.lock().unwrap();
        let w = &mut *guard;
        let sane = |l: &Level| l.price > Decimal::ZERO && l.qty > Decimal::ZERO;
        book.bids.retain(sane);
        book.asks.retain(sane);
        let id = book.instrument.clone();
        w.books.insert(id.clone(), book);
        if !self.calendar.is_open(id.venue, now) {
            return Vec::new();
        }
        let Some(inst) = w.instruments.get(&id) else { return Vec::new() };
        let (tick, step) = (inst.tick.clone(), inst.lot.step);
        let params = SimParams::default_for(id.venue);
        let mut rest = w.resting.remove(&id).unwrap_or_default();
        let mut fills = Vec::new();
        for r in rest.iter_mut() {
            let book = &w.books[&id];
            let shadow = w.shadows.entry(id.clone()).or_default();
            shadow.decay(now, &params);
            let levels = match r.side {
                Side::Buy => shadow.shadow_side(&book.asks, Side::Sell, &tick),
                Side::Sell => shadow.shadow_side(&book.bids, Side::Buy, &tick),
            };
            let slices = sim::walk(&levels, r.side, Size::Qty(r.remaining), r.price, step);
            if slices.is_empty() {
                continue;
            }
            let (qty, _) = sim::totals(&slices);
            r.remaining -= qty;
            // The crossing liquidity is used up, but we fill at our own limit and move no price.
            shadow_consume(w, &id, r.side, &slices);
            let mut order = w.orders.remove(&r.order_id).expect("resting order is tracked");
            release(w, &order, qty);
            fills.push(record_fill(w, &mut order, qty, qty * r.price, Liquidity::Maker, now));
            if r.remaining.is_zero() {
                order.status = OrderStatus::Filled;
            }
            w.orders.insert(r.order_id, order);
        }
        rest.retain(|r| r.remaining > Decimal::ZERO);
        if !rest.is_empty() {
            w.resting.insert(id, rest);
        }
        fills
    }

    /// A real trade printed. Resting orders at or through its (shifted) price fill as makers,
    /// best price first, sharing the trade's size.
    pub fn on_trade(&self, trade: Trade) -> Vec<Fill> {
        let now = self.clock.now();
        let mut guard = self.world.lock().unwrap();
        let w = &mut *guard;
        let id = trade.instrument.clone();
        if trade.price <= Decimal::ZERO || trade.qty <= Decimal::ZERO || !self.calendar.is_open(id.venue, now) {
            return Vec::new();
        }
        let Some(tick) = w.instruments.get(&id).map(|i| i.tick.clone()) else { return Vec::new() };
        let shadow = w.shadows.entry(id.clone()).or_default();
        shadow.decay(now, &SimParams::default_for(id.venue));
        let price = shadow.shift(trade.price, &tick);

        let mut rest = w.resting.remove(&id).unwrap_or_default();
        // Buys highest first, then sells lowest first.
        rest.sort_by_key(|r| (r.side == Side::Sell, if r.side == Side::Buy { -r.price } else { r.price }));
        let (mut buy_avail, mut sell_avail) = (trade.qty, trade.qty);
        let mut fills = Vec::new();
        for r in rest.iter_mut() {
            let avail = if r.side == Side::Buy { &mut buy_avail } else { &mut sell_avail };
            let q = sim::passive_fill(r, price, avail);
            if q.is_zero() {
                continue;
            }
            let mut order = w.orders.remove(&r.order_id).expect("resting order is tracked");
            release(w, &order, q);
            fills.push(record_fill(w, &mut order, q, q * r.price, Liquidity::Maker, now));
            if r.remaining.is_zero() {
                order.status = OrderStatus::Filled;
            }
            w.orders.insert(r.order_id, order);
        }
        rest.retain(|r| r.remaining > Decimal::ZERO);
        if !rest.is_empty() {
            w.resting.insert(id, rest);
        }
        fills
    }

    pub fn cancel_sync(&self, account: &str, order_id: OrderId) -> Result<Order, OrderError> {
        let mut guard = self.world.lock().unwrap();
        let w = &mut *guard;
        let order = w.orders.get(&order_id).filter(|o| o.account == account).ok_or(OrderError::NotFound)?;
        if order.status != OrderStatus::Open {
            return Ok(order.clone());
        }
        Ok(close_order(w, order_id, OrderStatus::Cancelled))
    }

    /// Expire open DAY orders whose venue is closed. Call on a timer.
    pub fn expire_day_orders(&self) -> Vec<Order> {
        let now = self.clock.now();
        let mut guard = self.world.lock().unwrap();
        let w = &mut *guard;
        let ids: Vec<OrderId> = w
            .orders
            .values()
            .filter(|o| o.status == OrderStatus::Open && o.req.tif == Tif::Day)
            .filter(|o| !self.calendar.is_open(o.req.instrument.venue, now))
            .map(|o| o.id)
            .collect();
        ids.into_iter().map(|id| close_order(w, id, OrderStatus::Expired)).collect()
    }

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
        let valid = from != to
            && amount > Decimal::ZERO
            && amount <= MAX_INPUT
            && amount.normalize().scale() <= from.decimals()
            && usd_krw > Decimal::ZERO
            && spread >= Decimal::ZERO
            && spread < Decimal::ONE;
        if !valid {
            return Err(OrderError::InvalidRequest(format!(
                "convert needs two different currencies, a positive amount in whole {} units of {} and a positive rate",
                from.code(),
                from.decimals()
            )));
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
        if credit <= Decimal::ZERO {
            return Err(OrderError::InvalidRequest(format!("{amount} {} is too small to convert", from.code())));
        }
        *pf.cash.entry(from).or_default() -= amount;
        *pf.cash.entry(to).or_default() += credit;
        Ok(Conversion { from, to, debit: amount, credit, rate: (krw_per(from) * net / krw_per(to)).round_dp(8) })
    }

    pub fn estimate(&self, account: &str, req: &OrderRequest) -> Result<Estimate, OrderError> {
        let now = self.clock.now();
        let mut guard = self.world.lock().unwrap();
        let w = &mut *guard;
        let plan = self.prepare(w, account, req, now)?;
        let params = SimParams::default_for(req.instrument.venue);
        let stats = w.stats.get(&req.instrument).copied().unwrap_or(DailyStats { sigma: params.default_sigma, adv_notional: params.default_adv });
        let avg_price = (!plan.qty.is_zero()).then(|| (plan.notional / plan.qty).round_dp(8));
        let slippage_bps = avg_price.map_or(Decimal::ZERO, |p| ((p - plan.mid) / plan.mid * dec!(10000)).abs().round_dp(2));
        Ok(Estimate {
            filled_qty: plan.qty,
            avg_price,
            notional: plan.notional,
            fee: plan.fee,
            tax: plan.tax,
            rest_qty: plan.rest_qty,
            slippage_bps,
            impact_bps: sim::impact(req.side, plan.notional, stats, &params).abs() * 10000.0,
        })
    }

    pub fn place_sync(&self, account: &str, req: OrderRequest) -> Result<(Order, Vec<Fill>), OrderError> {
        let now = self.clock.now();
        let mut guard = self.world.lock().unwrap();
        let w = &mut *guard;
        let plan = self.prepare(w, account, &req, now)?;
        let id = w.next_order_id;
        w.next_order_id += 1;
        let mut order = Order {
            id,
            account: account.to_string(),
            req,
            status: OrderStatus::Open,
            filled_qty: Decimal::ZERO,
            filled_notional: Decimal::ZERO,
            created_at: now,
        };
        let mut fills = Vec::new();
        if !plan.slices.is_empty() {
            fills.push(take(w, &mut order, &plan.slices, now));
        }
        if plan.rest_qty > Decimal::ZERO {
            let price = order.req.limit_price.expect("only limit orders rest");
            let venue = order.req.instrument.venue;
            let pf = w.accounts.get_mut(account).expect("checked in prepare");
            match order.req.side {
                Side::Buy => pf.reserve_cash(venue.currency(), buy_reserve_per_unit(price, venue) * plan.rest_qty),
                Side::Sell => pf.reserve_qty(&order.req.instrument, plan.rest_qty),
            }
            w.resting.entry(order.req.instrument.clone()).or_default().push(Resting {
                order_id: id,
                side: order.req.side,
                price,
                remaining: plan.rest_qty,
                queue_ahead: plan.queue_ahead,
            });
        } else {
            order.status = if is_complete(&order) { OrderStatus::Filled } else { OrderStatus::Cancelled };
        }
        w.orders.insert(id, order.clone());
        Ok((order, fills))
    }

    /// Validate `req` and work out what executing it now would do. Only side effect: decaying the
    /// instrument's shadow state to `now`.
    fn prepare(&self, w: &mut World, account: &str, req: &OrderRequest, now: DateTime<Utc>) -> Result<Plan, OrderError> {
        let pf = w.accounts.get(account).ok_or(OrderError::UnknownAccount)?;
        let inst = w.instruments.get(&req.instrument).ok_or(OrderError::UnknownInstrument)?;
        if !inst.tradable {
            return Err(OrderError::NotTradable);
        }
        check_shape(req)?;
        let venue = req.instrument.venue;
        if !self.calendar.is_open(venue, now) {
            return Err(OrderError::MarketClosed { next_open: self.calendar.next_open(venue, now) });
        }
        let book = w.books.get(&req.instrument).ok_or(OrderError::StaleData { age_secs: None })?;
        let age = now - book.received_at;
        if age > self.staleness {
            return Err(OrderError::StaleData { age_secs: Some(age.num_seconds()) });
        }
        if let Some(p) = req.limit_price {
            if !inst.tick.is_valid(p) {
                return Err(OrderError::InvalidTick { lower: inst.tick.floor(p), upper: inst.tick.ceil(p) });
            }
            if let Some((lower, upper)) = price_band(venue, &inst.tick, book.prev_close) {
                if p < lower || p > upper {
                    return Err(OrderError::PriceLimit { lower, upper });
                }
            }
        }

        let params = SimParams::default_for(venue);
        let shadow = w.shadows.entry(req.instrument.clone()).or_default();
        shadow.decay(now, &params);
        let bids = shadow.shadow_side(&book.bids, Side::Buy, &inst.tick);
        let asks = shadow.shadow_side(&book.asks, Side::Sell, &inst.tick);
        let (Some(best_bid), Some(best_ask)) = (bids.first(), asks.first()) else {
            return Err(OrderError::NoLiquidity);
        };
        let mid = (best_bid.price + best_ask.price) / dec!(2);

        let lot = &inst.lot;
        let bad_qty = || OrderError::InvalidQty { step: lot.step, min_qty: lot.min_qty, min_notional: lot.min_notional };
        match req.size {
            Size::Qty(q) if !lot.is_valid(q, req.limit_price.unwrap_or(mid)) => return Err(bad_qty()),
            Size::Notional(n) if n <= Decimal::ZERO || n < lot.min_notional => return Err(bad_qty()),
            _ => {}
        }

        let fees = FeeSchedule::default_for(venue);
        let (opposite, own, band) = match req.side {
            Side::Buy => (&asks, &bids, dec!(1) + params.max_slippage),
            Side::Sell => (&bids, &asks, dec!(1) - params.max_slippage),
        };
        let limit = req.limit_price.unwrap_or((mid * band).round_dp(8));
        // Leave room for commission inside a notional budget.
        let walk_size = match req.size {
            Size::Notional(n) => Size::Notional(n * dec!(10000) / (dec!(10000) + fees.commission_bps)),
            s => s,
        };
        let slices = sim::walk(opposite, req.side, walk_size, limit, lot.step);
        let (qty, notional) = sim::totals(&slices);
        if matches!(req.size, Size::Notional(_)) && qty > Decimal::ZERO && !lot.is_valid(qty, notional / qty) {
            return Err(bad_qty());
        }
        let (fee, tax) = fees.cost(req.side, notional, venue.currency().decimals());
        let rest_qty = match (req.kind, req.tif, req.size) {
            (OrderType::Limit, Tif::Day | Tif::Gtc, Size::Qty(q)) => q - qty,
            _ => Decimal::ZERO,
        };
        let queue_ahead = own.iter().filter(|l| Some(l.price) == req.limit_price).map(|l| l.qty).sum();

        match (req.side, req.size) {
            (Side::Buy, _) => {
                let required = notional + fee + tax + buy_reserve_per_unit(limit, venue) * rest_qty;
                let available = pf.available_cash(venue.currency());
                if required > available {
                    return Err(OrderError::InsufficientFunds { required, available });
                }
            }
            (Side::Sell, Size::Qty(wanted)) => {
                let available = pf.available_qty(&req.instrument);
                if wanted > available {
                    return Err(OrderError::InsufficientPosition { available });
                }
            }
            (Side::Sell, Size::Notional(_)) => unreachable!("rejected by check_shape"),
        }

        Ok(Plan { slices, qty, notional, fee, tax, rest_qty, queue_ahead, mid })
    }
}

#[async_trait]
impl Broker for SimBroker {
    async fn place(&self, account: &str, req: OrderRequest) -> Result<(Order, Vec<Fill>), OrderError> {
        self.place_sync(account, req)
    }

    async fn cancel(&self, account: &str, order_id: OrderId) -> Result<Order, OrderError> {
        self.cancel_sync(account, order_id)
    }
}
