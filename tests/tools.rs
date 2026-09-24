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

async fn rig(pool: PgPool) -> (Arc<App>, TraderTools) {
    let clock = ManualClock::new(Utc.with_ymd_and_hms(2026, 9, 23, 1, 0, 0).unwrap());
    let store = Arc::new(Store::new(pool));
    store.create_account("bot", "Bot", Some("agent-1"), &[(Currency::Krw, dec!(1000000000))], Utc::now()).await.unwrap();
    store.create_account("manual", "Manual", None, &[(Currency::Krw, dec!(1000000000))], Utc::now()).await.unwrap();
    let (jtx, jrx) = mpsc::unbounded_channel();
    let broker = Arc::new(SimBroker::new(Arc::new(clock.clone()), Calendar::default()).with_journal(jtx));
    let generations = restore(&store, &broker).await.unwrap();
    let (bus, _) = broadcast::channel(64);
    let mut market = Market::new(broker.clone());
    let _subs = market.add_feed(Arc::new(Fake { clock }), 10);
    market.load_instruments().await.unwrap();
    tokio::spawn(persist(jrx, store.clone(), bus, generations));
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
    assert_eq!(restore(&app.store, &fresh).await.unwrap().len(), 2);
    assert_eq!(fresh.portfolio("bot").unwrap(), before);
    assert_eq!(fresh.order(resting.id).unwrap().status, atrader::broker::OrderStatus::Open);
}

#[sqlx::test]
async fn unknown_instruments_are_not_retriable_and_not_subscribed(pool: PgPool) {
    let (_, t) = rig(pool).await;
    for id in ["UPBIT:KRW-NOPE", "KRX:005930"] {
        let mut o = buy("bot", Some(dec!(1)), "why");
        o.instrument = id.into();
        let e = t.estimate_order(o).await.unwrap_err();
        assert_eq!((code(&e), e.retriable), ("UNKNOWN_INSTRUMENT".to_string(), false), "{id}");
        assert_eq!(code(&t.get_orderbook(id.into(), None).await.unwrap_err()), "UNKNOWN_INSTRUMENT");
    }
    let q = t.get_quotes(vec!["UPBIT:KRW-NOPE".into()]).await.unwrap();
    assert!(q[0].error.as_deref().unwrap_or("").contains("unknown"));
}

#[sqlx::test]
async fn research_tools(pool: PgPool) {
    let (app, t) = rig(pool).await;
    let c = t.get_candles("UPBIT:KRW-BTC".into(), "5m".into(), Some(30)).await.unwrap();
    assert_eq!(c.len(), 30);
    assert!(c[0].start < c[29].start);
    assert_eq!(code(&t.get_candles("UPBIT:KRW-BTC".into(), "2h".into(), None).await.unwrap_err()), "InvalidParams");

    let ind = t.get_indicators("UPBIT:KRW-BTC".into(), "1d".into(), vec!["sma:5".into(), "macd".into()], Some(3)).await.unwrap();
    assert_eq!(ind.iter().map(|l| l.name.as_str()).collect::<Vec<_>>(), vec!["sma_5", "macd", "macd_signal", "macd_hist"]);
    assert_eq!(ind[0].values.len(), 3);
    assert!(ind[0].values[2].value.is_some());
    assert_eq!(code(&t.get_indicators("UPBIT:KRW-BTC".into(), "1d".into(), vec!["magic".into()], None).await.unwrap_err()), "InvalidParams");

    let rows = t.screen("UPBIT".into(), "gainers".into(), Some(5)).await.unwrap();
    assert_eq!(rows[0].id, "UPBIT:KRW-BTC");
    assert_eq!(rows[0].name, "비트코인 (Bitcoin)"); // filled from the instrument list
    assert_eq!(code(&t.screen("KRX".into(), "gainers".into(), None).await.unwrap_err()), "INVALID_REQUEST"); // no KRX feed here

    t.place_order(buy("bot", Some(dec!(0.1)), "entry")).await.unwrap();
    let p = t.get_performance("bot".into(), "all".into()).await.unwrap();
    assert!(p.trades <= 1); // the journal may not have landed yet; no panic either way
    assert_eq!(code(&t.get_performance("bot".into(), "1y".into()).await.unwrap_err()), "InvalidParams");
    let _ = app;
}

#[sqlx::test]
async fn fundamentals_need_their_keys(pool: PgPool) {
    let (_, t) = rig(pool).await;
    let e = t.get_financials("UPBIT:KRW-BTC".into()).await.unwrap_err();
    assert_eq!(code(&e), "INVALID_REQUEST");
    assert!(e.message.contains("stocks"), "{}", e.message);
    assert_eq!(code(&t.list_filings("UPBIT:KRW-BTC".into(), None, None).await.unwrap_err()), "INVALID_REQUEST");
    assert_eq!(code(&t.list_filings("UPBIT:KRW-BTC".into(), Some("yesterday".into()), None).await.unwrap_err()), "InvalidParams");
}

struct Recorder {
    sent: std::sync::Mutex<Vec<(String, String, Option<String>, String)>>,
    fail: std::sync::atomic::AtomicBool,
}

#[async_trait]
impl atrader::alerts::deliver::Notifier for Recorder {
    async fn send(&self, agent_id: &str, account: &str, session: Option<String>, text: &str) -> anyhow::Result<String> {
        if self.fail.load(std::sync::atomic::Ordering::SeqCst) {
            anyhow::bail!("attacca unreachable");
        }
        self.sent.lock().unwrap().push((agent_id.into(), account.into(), session, text.into()));
        Ok("sess-1".into())
    }
}

#[sqlx::test]
async fn fired_alerts_reach_the_agent_once(pool: PgPool) {
    use atrader::alerts::{Alert, Condition, deliver::{AlertCmd, alert_loop}};
    let (app, _t) = rig(pool).await;
    let (bus, _) = broadcast::channel(64);
    let (cmd_tx, cmd_rx) = mpsc::unbounded_channel();
    let rec = Arc::new(Recorder { sent: Default::default(), fail: Default::default() });
    let gens = std::collections::HashMap::from([("bot".to_string(), 1)]);
    tokio::spawn(alert_loop(app.clone(), bus.subscribe(), cmd_rx, rec.clone(), gens));
    let alert = Alert {
        id: 0,
        account: "bot".into(),
        generation: 1,
        condition: Condition::PriceAbove { id: "UPBIT:KRW-BTC".parse().unwrap(), price: dec!(100000000) },
        note: "breakout".into(),
        once: true,
        created_at: Utc::now(),
        last_fired_at: None,
    };
    let id = app.store.create_alert(&alert).await.unwrap();
    cmd_tx.send(AlertCmd::Upsert(Alert { id, ..alert })).unwrap();
    tokio::time::sleep(Duration::from_millis(50)).await;
    let print = |p| atrader::market::BusEvent::Market(MarketEvent::Trade(Trade { instrument: "UPBIT:KRW-BTC".parse().unwrap(), price: p, qty: dec!(1), at: Utc::now() }));
    bus.send(print(dec!(100000000))).unwrap();
    bus.send(print(dec!(100100000))).unwrap();
    for _ in 0..100 {
        if !rec.sent.lock().unwrap().is_empty() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    tokio::time::sleep(Duration::from_millis(100)).await;
    let sent = rec.sent.lock().unwrap().clone();
    assert_eq!(sent.len(), 1, "{sent:?}");
    let (agent, account, session, text) = &sent[0];
    assert_eq!((agent.as_str(), account.as_str(), session.as_deref()), ("agent-1", "bot", None));
    assert!(text.contains("breakout") && text.contains("#") && text.contains("equity"), "{text}");
    assert!(app.store.active_alerts("bot", 1).await.unwrap().is_empty()); // one-shot persisted as off
    assert_eq!(app.store.alert_session("bot").await.unwrap().as_deref(), Some("sess-1"));
}

#[sqlx::test]
async fn failed_deliveries_are_recorded(pool: PgPool) {
    use atrader::alerts::{Alert, Condition, deliver::{AlertCmd, alert_loop}};
    let (app, _t) = rig(pool.clone()).await;
    let (bus, _) = broadcast::channel(64);
    let (cmd_tx, cmd_rx) = mpsc::unbounded_channel();
    let rec = Arc::new(Recorder { sent: Default::default(), fail: std::sync::atomic::AtomicBool::new(true) });
    tokio::spawn(alert_loop(app.clone(), bus.subscribe(), cmd_rx, rec, std::collections::HashMap::from([("bot".to_string(), 1)])));
    let alert = Alert { id: 0, account: "bot".into(), generation: 1, condition: Condition::OrderFilled { id: None }, note: "fills".into(), once: false, created_at: Utc::now(), last_fired_at: None };
    let id = app.store.create_alert(&alert).await.unwrap();
    cmd_tx.send(AlertCmd::Upsert(Alert { id, ..alert })).unwrap();
    tokio::time::sleep(Duration::from_millis(50)).await;
    let fill = atrader::broker::Fill {
        order_id: 1,
        account: "bot".into(),
        instrument: "UPBIT:KRW-BTC".parse().unwrap(),
        side: Side::Buy,
        qty: dec!(0.1),
        notional: dec!(10000000),
        price: dec!(100000000),
        fee: dec!(5000),
        tax: dec!(0),
        realized_pnl: None,
        liquidity: atrader::broker::Liquidity::Taker,
        at: Utc::now(),
    };
    bus.send(atrader::market::BusEvent::Fill(fill)).unwrap();
    let mut row = None;
    for _ in 0..100 {
        row = sqlx::query_as::<_, (bool, Option<String>)>("SELECT delivered, error FROM alert_events").fetch_optional(&pool).await.unwrap();
        if row.is_some() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    let (delivered, error) = row.expect("an event row");
    assert!(!delivered);
    assert!(error.unwrap().contains("unreachable"));
}
