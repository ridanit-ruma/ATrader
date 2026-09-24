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

/// Load every account's portfolio and open orders from the store into `broker`. Returns the
/// generation each account was restored at; the journal writer must keep using these.
pub async fn restore(store: &Store, broker: &SimBroker) -> anyhow::Result<HashMap<String, i32>> {
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
    Ok(accounts.into_iter().map(|a| (a.id, a.generation)).collect())
}
