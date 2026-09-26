//! Shared test rig: a fake Upbit feed and an App wired to a test database.

use std::sync::Arc;

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
use atrader::tools::TraderTools;
use atrader::venue::{Calendar, Instrument, LotRule, TickRule};
use chrono::{TimeZone, Utc};
use rust_decimal::Decimal;
use rust_decimal_macros::dec;
use sqlx::PgPool;
use tokio::sync::{broadcast, mpsc};

pub struct Fake {
    pub clock: ManualClock,
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
}

/// Accounts `bot` ("Bot") and `manual` ("Manual"), each with ₩1bn.
pub async fn rig(pool: PgPool) -> (Arc<App>, TraderTools) {
    rig_with(pool, &["bot", "manual"]).await
}

pub async fn rig_with(pool: PgPool, accounts: &[&str]) -> (Arc<App>, TraderTools) {
    let clock = ManualClock::new(Utc.with_ymd_and_hms(2026, 9, 23, 1, 0, 0).unwrap());
    let store = Arc::new(Store::new(pool));
    for id in accounts {
        let name = format!("{}{}", id[..1].to_uppercase(), &id[1..]);
        store.create_account(id, &name, &[(Currency::Krw, dec!(1000000000))], Utc::now()).await.unwrap();
    }
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
