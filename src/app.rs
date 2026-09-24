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
    pub dart: Option<crate::fundamentals::dart::DartClient>,
    pub edgar: Option<crate::fundamentals::edgar::EdgarClient>,
    pub alerts: Option<tokio::sync::mpsc::UnboundedSender<crate::alerts::deliver::AlertCmd>>,
    accounts: RwLock<HashMap<String, AccountRow>>,
}

impl App {
    pub async fn new(broker: Arc<SimBroker>, store: Arc<Store>, market: Market, fx: FxCache) -> anyhow::Result<Self> {
        let app = App { broker, store, market, fx, fx_spread: dec!(0.001), dart: None, edgar: None, alerts: None, accounts: RwLock::new(HashMap::new()) };
        app.reload_accounts().await?;
        Ok(app)
    }

    pub fn with_fundamentals(mut self, dart: Option<crate::fundamentals::dart::DartClient>, edgar: Option<crate::fundamentals::edgar::EdgarClient>) -> Self {
        self.dart = dart;
        self.edgar = edgar;
        self
    }

    pub fn with_alerts(mut self, tx: tokio::sync::mpsc::UnboundedSender<crate::alerts::deliver::AlertCmd>) -> Self {
        self.alerts = Some(tx);
        self
    }

    /// Create an account and make it tradable immediately.
    pub async fn create_account(&self, id: &str, name: &str, agent: Option<&str>, cash: &[(Currency, Decimal)]) -> anyhow::Result<()> {
        self.store.create_account(id, name, agent, cash, self.broker.now()).await?;
        self.broker.restore_account(id, crate::ledger::Portfolio::new(cash), 1);
        self.reload_accounts().await
    }

    /// Start an account over while serving. Returns the new generation.
    pub async fn reset_account(&self, id: &str, cash: &[(Currency, Decimal)]) -> anyhow::Result<i32> {
        let generation = self.store.reset_account(id, cash, self.broker.now()).await?;
        self.broker.reset_account(id, cash, generation);
        if let Some(tx) = &self.alerts {
            let _ = tx.send(crate::alerts::deliver::AlertCmd::DropAccount(id.to_string()));
        }
        self.reload_accounts().await?;
        Ok(generation)
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
        self.account(id).filter(|r| r.agent_id.is_some()).ok_or_else(|| order_error(OrderError::UnknownAccount))
    }

    /// Any account, agent-traded or not.
    pub fn account(&self, id: &str) -> Option<AccountRow> {
        self.accounts.read().unwrap().get(id).cloned()
    }
}

/// KRW per unit of `c`; USDT counts as USD.
pub fn krw_per(c: Currency, usd_krw: Decimal) -> Decimal {
    if c == Currency::Krw { Decimal::ONE } else { usd_krw }
}

/// Load every account's portfolio and open orders from the store into `broker`. Returns how many
/// accounts were restored.
pub async fn restore(store: &Store, broker: &SimBroker) -> anyhow::Result<usize> {
    let accounts = store.list_accounts().await?;
    for a in &accounts {
        broker.restore_account(&a.id, store.load_portfolio(&a.id, a.generation).await?, a.generation);
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
pub async fn snapshot_loop(app: Arc<App>) {
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
        for (account, generation) in &app.broker.generations() {
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::{Book, Clock, Level, ManualClock};
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
