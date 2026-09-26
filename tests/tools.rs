use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use atrader::app::restore;
use atrader::broker::SimBroker;
use atrader::domain::*;
use atrader::feed::MarketEvent;
use atrader::tools::*;
use atrader::venue::Calendar;
use chrono::{TimeZone, Utc};
use rust_decimal::Decimal;
use rust_decimal_macros::dec;
use sqlx::PgPool;
use tokio::sync::{broadcast, mpsc};
use zyris::ErrorCode;

mod common;
use common::*;


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
    assert_eq!(restore(&app.store, &fresh).await.unwrap(), 2);
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
    tokio::spawn(alert_loop(app.clone(), bus.subscribe(), cmd_rx, rec.clone()));
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
    tokio::spawn(alert_loop(app.clone(), bus.subscribe(), cmd_rx, rec));
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

fn alert_input(kind: &str) -> AlertInput {
    AlertInput { kind: kind.into(), instrument: Some("UPBIT:KRW-BTC".into()), venue: None, threshold: Some(dec!(100000000)), window_minutes: None, note: "watch".into(), once: None }
}

#[sqlx::test]
async fn alert_tools_validate_and_scope_to_the_account(pool: PgPool) {
    let (_, t) = rig(pool).await;
    let a = t.create_alert("bot".into(), alert_input("price_above")).await.unwrap();
    assert!(a.once && a.id > 0);
    assert_eq!(t.list_alerts("bot".into()).await.unwrap().len(), 1);

    let mut m = alert_input("move");
    assert_eq!(code(&t.create_alert("bot".into(), m.clone()).await.unwrap_err()), "InvalidParams"); // no window
    m.window_minutes = Some(0);
    assert_eq!(code(&t.create_alert("bot".into(), m.clone()).await.unwrap_err()), "InvalidParams");
    m.window_minutes = Some(30);
    m.threshold = Some(dec!(3));
    t.create_alert("bot".into(), m).await.unwrap();

    let mut no_threshold = alert_input("price_below");
    no_threshold.threshold = None;
    assert_eq!(code(&t.create_alert("bot".into(), no_threshold).await.unwrap_err()), "InvalidParams");
    let mut unknown = alert_input("price_above");
    unknown.instrument = Some("UPBIT:KRW-NOPE".into());
    assert_eq!(code(&t.create_alert("bot".into(), unknown).await.unwrap_err()), "UNKNOWN_INSTRUMENT");
    let session = AlertInput { kind: "session_open".into(), instrument: None, venue: Some("NASDAQ".into()), threshold: None, window_minutes: None, note: "x".into(), once: None };
    assert_eq!(code(&t.create_alert("bot".into(), session).await.unwrap_err()), "InvalidParams");
    assert_eq!(code(&t.create_alert("bot".into(), alert_input("teleport")).await.unwrap_err()), "InvalidParams");
    assert_eq!(code(&t.create_alert("manual".into(), alert_input("price_above")).await.unwrap_err()), "UNKNOWN_ACCOUNT");

    assert_eq!(code(&t.delete_alert("bot".into(), 999_999).await.unwrap_err()), "NOT_FOUND");
    let gone = t.delete_alert("bot".into(), a.id).await.unwrap();
    assert!(!gone.active);
    assert_eq!(t.list_alerts("bot".into()).await.unwrap().len(), 1);
}

#[sqlx::test]
async fn simultaneous_alerts_share_one_new_session(pool: PgPool) {
    use atrader::alerts::{Alert, Condition, deliver::{AlertCmd, alert_loop}};
    let (app, _t) = rig(pool).await;
    let (bus, _) = broadcast::channel(64);
    let (cmd_tx, cmd_rx) = mpsc::unbounded_channel();
    let rec = Arc::new(Recorder { sent: Default::default(), fail: Default::default() });
    tokio::spawn(alert_loop(app.clone(), bus.subscribe(), cmd_rx, rec.clone()));
    for price in [dec!(99000000), dec!(99500000)] {
        let a = Alert { id: 0, account: "bot".into(), generation: 1, condition: Condition::PriceAbove { id: "UPBIT:KRW-BTC".parse().unwrap(), price }, note: "n".into(), once: true, created_at: Utc::now(), last_fired_at: None };
        let id = app.store.create_alert(&a).await.unwrap();
        cmd_tx.send(AlertCmd::Upsert(Alert { id, ..a })).unwrap();
    }
    tokio::time::sleep(Duration::from_millis(50)).await;
    bus.send(atrader::market::BusEvent::Market(MarketEvent::Trade(Trade { instrument: "UPBIT:KRW-BTC".parse().unwrap(), price: dec!(100000000), qty: dec!(1), at: Utc::now() }))).unwrap();
    for _ in 0..100 {
        if rec.sent.lock().unwrap().len() == 2 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    let sent = rec.sent.lock().unwrap().clone();
    assert_eq!(sent.len(), 2);
    assert_eq!(sent.iter().filter(|s| s.2.is_none()).count(), 1, "both deliveries created a session: {sent:?}");
}

#[sqlx::test]
async fn accounts_are_created_and_reset_live(pool: PgPool) {
    let (app, t) = rig(pool).await;
    app.create_account("fresh", "Fresh", Some("agent-2"), &[(Currency::Krw, dec!(1000000))]).await.unwrap();
    assert!(t.list_accounts().await.unwrap().iter().any(|a| a.id == "fresh"));
    t.place_order(buy("fresh", Some(dec!(0.001)), "first")).await.unwrap();
    let generation = app.reset_account("fresh", &[(Currency::Krw, dec!(2000000))]).await.unwrap();
    assert_eq!(generation, 2);
    let acct = t.get_account("fresh".into()).await.unwrap();
    assert_eq!(acct.equity_krw, dec!(2000000));
    t.place_order(buy("fresh", Some(dec!(0.001)), "second")).await.unwrap();
    for _ in 0..100 {
        if !app.store.fills("fresh", 2, None, 10).await.unwrap().is_empty() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert_eq!(app.store.fills("fresh", 1, None, 10).await.unwrap().len(), 1);
    assert_eq!(app.store.fills("fresh", 2, None, 10).await.unwrap().len(), 1);
}

/// Every tool schema must be plain enough for any LLM tool API: no references, no type unions,
/// no nullable anyOf, no non-standard formats, and every object lists its properties.
#[test]
fn announced_schemas_are_portable() {
    fn check(v: &serde_json::Value, tool: &str, path: &str) {
        match v {
            serde_json::Value::Object(m) => {
                for bad in ["$ref", "$defs", "$schema", "anyOf", "oneOf", "allOf", "title"] {
                    assert!(!m.contains_key(bad), "{tool}{path}: has {bad}");
                }
                if let Some(t) = m.get("type") {
                    assert!(t.is_string(), "{tool}{path}: type union {t}");
                    if t == "object" {
                        assert!(m.contains_key("properties"), "{tool}{path}: object without properties");
                    }
                    if t == "number" || t == "integer" {
                        assert!(!m.contains_key("pattern"), "{tool}{path}: pattern on a number");
                    }
                }
                // DeepSeek accepts only email/hostname/ipv4/ipv6/uuid string formats; carry none.
                assert!(!m.contains_key("format"), "{tool}{path}: format {:?}", m.get("format"));
                assert_ne!(m.get("default"), Some(&serde_json::Value::Null), "{tool}{path}: default null");
                for (k, x) in m {
                    match (k.as_str(), x) {
                        // Field names, not keywords: check each field's schema.
                        ("properties", serde_json::Value::Object(fields)) => {
                            fields.iter().for_each(|(f, s)| check(s, tool, &format!("{path}/properties/{f}")))
                        }
                        _ => check(x, tool, &format!("{path}/{k}")),
                    }
                }
            }
            serde_json::Value::Array(a) => a.iter().for_each(|x| check(x, tool, path)),
            _ => {}
        }
    }
    let d = atrader::tools::portable(atrader::tools::trader_capability());
    assert!(d.tools.len() > 20);
    for t in &d.tools {
        check(&t.request_schema, &t.name, "");
        check(t.response_schema.as_ref().unwrap(), &t.name, "(response)");
    }
    let order = d.tools.iter().find(|t| t.name == "place_order").unwrap();
    let qty = &order.request_schema["properties"]["order"]["properties"]["qty"];
    assert_eq!(qty["type"], "number", "decimals are numbers: {qty}");
    let side = &order.request_schema["properties"]["order"]["properties"]["side"];
    assert_eq!(side["enum"], serde_json::json!(["buy", "sell"]), "enums are inlined: {side}");
    let fills = d.tools.iter().find(|t| t.name == "list_fills").unwrap();
    let since = fills.request_schema["properties"]["since"]["description"].as_str().unwrap_or_default();
    assert!(since.contains("RFC 3339"), "a dropped date-time format is spelled out: {since}");
    // What the schema now asks for (plain numbers) must decode exactly.
    let o: OrderInput = serde_json::from_value(serde_json::json!({"account": "bot", "instrument": "UPBIT:KRW-BTC", "side": "buy", "kind": "limit", "qty": 0.1, "limit_price": 99999000, "reason": "r"})).unwrap();
    assert_eq!((o.qty, o.limit_price), (Some(dec!(0.1)), Some(dec!(99999000))));
}

#[sqlx::test]
async fn giving_an_account_to_an_agent_shows_it_and_forgets_the_alert_session(pool: PgPool) {
    let (app, t) = rig(pool).await;
    assert!(!t.list_accounts().await.unwrap().iter().any(|a| a.id == "manual"));
    app.store.set_alert_session("manual", "old-session").await.unwrap();
    app.set_account_agent("manual", Some("agent-2")).await.unwrap();
    assert!(t.list_accounts().await.unwrap().iter().any(|a| a.id == "manual"));
    assert_eq!(app.store.alert_session("manual").await.unwrap(), None, "the old agent's session is not reused");
    assert!(app.set_account_agent("nope", None).await.is_err());
}
