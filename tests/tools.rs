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
