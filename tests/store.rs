use atrader::broker::*;
use atrader::domain::*;
use atrader::sim::Size;
use atrader::store::{AccountRow, Store};
use chrono::Utc;
use rust_decimal_macros::dec;
use sqlx::PgPool;

fn samsung() -> InstrumentId {
    "KRX:005930".parse().unwrap()
}

fn order(id: u64) -> Order {
    Order {
        id,
        account: "a".into(),
        req: OrderRequest {
            instrument: samsung(),
            side: Side::Buy,
            kind: OrderType::Market,
            size: Size::Qty(dec!(10)),
            limit_price: None,
            tif: Tif::Ioc,
            reason: "test".into(),
        },
        status: OrderStatus::Filled,
        filled_qty: dec!(10),
        filled_notional: dec!(700000),
        created_at: Utc::now(),
    }
}

fn fill(order_id: u64, side: Side, qty: rust_decimal::Decimal, notional: rust_decimal::Decimal, fee: rust_decimal::Decimal, tax: rust_decimal::Decimal) -> Fill {
    Fill {
        order_id,
        account: "a".into(),
        instrument: samsung(),
        side,
        qty,
        notional,
        price: notional / qty,
        fee,
        tax,
        realized_pnl: None,
        liquidity: Liquidity::Taker,
        at: Utc::now(),
    }
}

#[sqlx::test]
async fn ledger_and_fill_replay_agree(pool: PgPool) {
    let store = Store::new(pool);
    store.create_account("a", "Test", None, &[(Currency::Krw, dec!(1000000))], Utc::now()).await.unwrap();
    store.save_order(&order(1), 1).await.unwrap();
    store.save_fill(&fill(1, Side::Buy, dec!(10), dec!(700000), dec!(105), dec!(0)), 1).await.unwrap();
    store.save_order(&order(2), 1).await.unwrap();
    store.save_fill(&fill(2, Side::Sell, dec!(4), dec!(300000), dec!(45), dec!(600)), 1).await.unwrap();

    let cash = store.cash_balances("a", 1).await.unwrap();
    assert_eq!(cash[&Currency::Krw], dec!(599250)); // 1,000,000 - 700,105 + 299,355
    let pf = store.load_portfolio("a", 1).await.unwrap();
    assert_eq!(pf.cash(Currency::Krw), cash[&Currency::Krw]);
    assert_eq!(pf.positions[&samsung()].qty, dec!(6));
    assert_eq!(pf.positions[&samsung()].avg_cost, dec!(70000));
    assert_eq!(store.max_order_id().await.unwrap(), 2);
}

#[sqlx::test]
async fn save_order_updates_in_place(pool: PgPool) {
    let store = Store::new(pool);
    store.create_account("a", "Test", None, &[(Currency::Krw, dec!(1000000))], Utc::now()).await.unwrap();
    let mut o = order(7);
    o.status = OrderStatus::Open;
    store.save_order(&o, 1).await.unwrap();
    o.status = OrderStatus::Cancelled;
    store.save_order(&o, 1).await.unwrap();
    assert_eq!(store.max_order_id().await.unwrap(), 7);
}

#[sqlx::test]
async fn reset_starts_a_new_generation(pool: PgPool) {
    let store = Store::new(pool);
    store.create_account("a", "Test", Some("agent-1"), &[(Currency::Krw, dec!(1000000))], Utc::now()).await.unwrap();
    store.save_order(&order(1), 1).await.unwrap();
    store.save_fill(&fill(1, Side::Buy, dec!(10), dec!(700000), dec!(105), dec!(0)), 1).await.unwrap();
    let generation = store.reset_account("a", &[(Currency::Krw, dec!(5000000)), (Currency::Usd, dec!(1000))], Utc::now()).await.unwrap();
    assert_eq!(generation, 2);
    assert_eq!(store.generation("a").await.unwrap(), 2);
    let pf = store.load_portfolio("a", 2).await.unwrap();
    assert_eq!(pf.cash(Currency::Krw), dec!(5000000));
    assert_eq!(pf.cash(Currency::Usd), dec!(1000));
    assert!(pf.positions.is_empty());
    assert_eq!(store.cash_balances("a", 1).await.unwrap()[&Currency::Krw], dec!(299895)); // history kept
}

#[sqlx::test]
async fn conversions_survive_replay(pool: PgPool) {
    let store = Store::new(pool);
    store.create_account("a", "Test", None, &[(Currency::Krw, dec!(2000000))], Utc::now()).await.unwrap();
    let c = Conversion { from: Currency::Krw, to: Currency::Usd, debit: dec!(1365350), credit: dec!(999), rate: dec!(0.00073167) };
    store.save_conversion("a", 1, &c, Utc::now()).await.unwrap();
    let cash = store.cash_balances("a", 1).await.unwrap();
    assert_eq!((cash[&Currency::Krw], cash[&Currency::Usd]), (dec!(634650), dec!(999)));
    let pf = store.load_portfolio("a", 1).await.unwrap();
    assert_eq!((pf.cash(Currency::Krw), pf.cash(Currency::Usd)), (dec!(634650), dec!(999)));
}

fn micros() -> chrono::DateTime<Utc> {
    use chrono::SubsecRound;
    Utc::now().trunc_subsecs(6)
}

#[sqlx::test]
async fn orders_and_fills_round_trip(pool: PgPool) {
    let store = Store::new(pool);
    store.create_account("a", "Test", Some("agent-1"), &[(Currency::Krw, dec!(1000000))], Utc::now()).await.unwrap();
    store.create_account("b", "Manual", None, &[(Currency::Krw, dec!(1))], Utc::now()).await.unwrap();
    assert_eq!(
        store.list_accounts().await.unwrap(),
        vec![
            AccountRow { id: "a".into(), name: "Test".into(), agent_id: Some("agent-1".into()), generation: 1 },
            AccountRow { id: "b".into(), name: "Manual".into(), agent_id: None, generation: 1 },
        ]
    );

    let mut filled = order(1);
    filled.created_at = micros();
    let mut open = order(2);
    open.req.kind = OrderType::Limit;
    open.req.limit_price = Some(dec!(69000));
    open.req.tif = Tif::Day;
    open.status = OrderStatus::Open;
    open.filled_qty = dec!(0);
    open.filled_notional = dec!(0);
    open.created_at = micros();
    store.save_order(&filled, 1).await.unwrap();
    store.save_order(&open, 1).await.unwrap();
    assert_eq!(store.orders("a", 1, true, 10).await.unwrap(), vec![open.clone()]);
    assert_eq!(store.orders("a", 1, false, 10).await.unwrap(), vec![open, filled]);

    let mut f = fill(1, Side::Buy, dec!(10), dec!(700000), dec!(105), dec!(0));
    f.at = micros();
    store.save_fill(&f, 1).await.unwrap();
    assert_eq!(store.fills("a", 1, None, 10).await.unwrap(), vec![f.clone()]);
    assert!(store.fills("a", 1, Some(f.at + chrono::Duration::seconds(1)), 10).await.unwrap().is_empty());
}

#[sqlx::test]
async fn persister_writes_in_order_then_republishes(pool: PgPool) {
    use atrader::market::BusEvent;
    use atrader::persist::persist;
    use std::sync::Arc;
    let store = Arc::new(Store::new(pool));
    store.create_account("a", "Test", None, &[(Currency::Krw, dec!(2000000))], Utc::now()).await.unwrap();
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
    let (bus, mut events) = tokio::sync::broadcast::channel(16);
    tx.send(Stamped { generation: 1, event: Journal::Order(order(1)) }).unwrap();
    tx.send(Stamped { generation: 1, event: Journal::Fill(fill(1, Side::Buy, dec!(10), dec!(700000), dec!(105), dec!(0))) }).unwrap();
    let c = Conversion { from: Currency::Krw, to: Currency::Usd, debit: dec!(1365350), credit: dec!(999), rate: dec!(0.00073167) };
    tx.send(Stamped { generation: 1, event: Journal::Conversion { account: "a".into(), conversion: c, at: Utc::now() } }).unwrap();
    drop(tx);
    persist(rx, store.clone(), bus).await;

    let cash = store.cash_balances("a", 1).await.unwrap();
    assert_eq!(cash[&Currency::Krw], dec!(2000000) - dec!(700105) - dec!(1365350));
    assert_eq!(cash[&Currency::Usd], dec!(999));
    assert!(matches!(events.recv().await.unwrap(), BusEvent::Order(o) if o.id == 1));
    assert!(matches!(events.recv().await.unwrap(), BusEvent::Fill(f) if f.order_id == 1));
}

#[sqlx::test]
async fn persister_keeps_the_generation_it_started_with(pool: PgPool) {
    use atrader::persist::persist;
    use std::sync::Arc;
    let store = Arc::new(Store::new(pool));
    store.create_account("a", "Test", None, &[(Currency::Krw, dec!(2000000))], Utc::now()).await.unwrap();
    store.reset_account("a", &[(Currency::Krw, dec!(5))], Utc::now()).await.unwrap(); // reset while serving
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
    let (bus, _) = tokio::sync::broadcast::channel(16);
    tx.send(Stamped { generation: 1, event: Journal::Order(order(1)) }).unwrap();
    tx.send(Stamped { generation: 1, event: Journal::Fill(fill(1, Side::Buy, dec!(10), dec!(700000), dec!(105), dec!(0))) }).unwrap();
    drop(tx);
    persist(rx, store.clone(), bus).await;
    assert_eq!(store.cash_balances("a", 1).await.unwrap()[&Currency::Krw], dec!(2000000) - dec!(700105));
    assert_eq!(store.cash_balances("a", 2).await.unwrap()[&Currency::Krw], dec!(5));
}

#[sqlx::test]
async fn persister_skips_integrity_failures_without_stalling(pool: PgPool) {
    use atrader::persist::persist;
    use std::sync::Arc;
    let store = Arc::new(Store::new(pool));
    store.create_account("a", "Test", None, &[(Currency::Krw, dec!(2000000))], Utc::now()).await.unwrap();
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
    let (bus, _) = tokio::sync::broadcast::channel(16);
    tx.send(Stamped { generation: 1, event: Journal::Fill(fill(99, Side::Buy, dec!(1), dec!(1), dec!(0), dec!(0))) }).unwrap(); // no order 99
    tx.send(Stamped { generation: 1, event: Journal::Order(order(1)) }).unwrap();
    drop(tx);
    let started = std::time::Instant::now();
    persist(rx, store.clone(), bus).await;
    assert!(started.elapsed() < std::time::Duration::from_secs(5), "retried a permanent failure");
    assert_eq!(store.max_order_id().await.unwrap(), 1);
}

#[sqlx::test]
async fn bars_and_snapshots_round_trip(pool: PgPool) {
    use atrader::candles::Candle;
    use atrader::performance::{Snapshot, SnapshotKind};
    use chrono::TimeZone;
    let store = Store::new(pool);
    let id: InstrumentId = "KRX:005930".parse().unwrap();
    let at = |m| Utc.with_ymd_and_hms(2026, 9, 23, 1, m, 0).unwrap();
    let c = |m, p| Candle { start: at(m), open: p, high: p, low: p, close: p, volume: dec!(1), value: p };
    store.save_bars(&[(id.clone(), c(1, dec!(100))), (id.clone(), c(0, dec!(99)))]).await.unwrap();
    store.save_bars(&[(id.clone(), c(1, dec!(101)))]).await.unwrap(); // upsert
    let bars = store.bars(&id, at(0)).await.unwrap();
    assert_eq!(bars.iter().map(|b| b.close).collect::<Vec<_>>(), vec![dec!(99), dec!(101)]);

    store.create_account("a", "Test", None, &[(Currency::Krw, dec!(1))], Utc::now()).await.unwrap();
    let s = Snapshot { account: "a".into(), generation: 1, at: at(5), kind: SnapshotKind::Daily, equity_krw: dec!(10), cash_krw: dec!(4), positions_krw: dec!(6) };
    store.save_snapshot(&s).await.unwrap();
    assert_eq!(store.snapshots("a", 1, None, false).await.unwrap(), vec![s.clone()]);
    assert!(store.snapshots("a", 1, Some(at(6)), false).await.unwrap().is_empty());
}

#[sqlx::test]
async fn late_bar_fragments_merge_instead_of_overwriting(pool: PgPool) {
    use atrader::candles::Candle;
    use chrono::TimeZone;
    let store = Store::new(pool);
    let id: InstrumentId = "KRX:005930".parse().unwrap();
    let start = Utc.with_ymd_and_hms(2026, 9, 23, 1, 0, 0).unwrap();
    let full = Candle { start, open: dec!(100), high: dec!(105), low: dec!(99), close: dec!(104), volume: dec!(10), value: dec!(1020) };
    let late = Candle { start, open: dec!(103), high: dec!(103), low: dec!(103), close: dec!(103), volume: dec!(1), value: dec!(103) };
    store.save_bars(&[(id.clone(), full)]).await.unwrap();
    store.save_bars(&[(id.clone(), late)]).await.unwrap();
    let b = store.bars(&id, start).await.unwrap();
    assert_eq!((b[0].open, b[0].high, b[0].low, b[0].close), (dec!(100), dec!(105), dec!(99), dec!(103)));
    assert_eq!((b[0].volume, b[0].value), (dec!(11), dec!(1123)));
}

#[sqlx::test]
async fn daily_snapshots_can_be_read_alone(pool: PgPool) {
    use atrader::performance::{Snapshot, SnapshotKind};
    let store = Store::new(pool);
    store.create_account("a", "Test", None, &[(Currency::Krw, dec!(1))], Utc::now()).await.unwrap();
    for (kind, secs) in [(SnapshotKind::Minute, 0), (SnapshotKind::Daily, 1), (SnapshotKind::Minute, 2)] {
        let s = Snapshot { account: "a".into(), generation: 1, at: Utc::now() + chrono::Duration::seconds(secs), kind, equity_krw: dec!(1), cash_krw: dec!(1), positions_krw: dec!(0) };
        store.save_snapshot(&s).await.unwrap();
    }
    assert_eq!(store.snapshots("a", 1, None, true).await.unwrap().len(), 1);
    assert_eq!(store.snapshots("a", 1, None, false).await.unwrap().len(), 3);
}

#[sqlx::test]
async fn alerts_round_trip_per_account_and_generation(pool: PgPool) {
    use atrader::alerts::{Alert, Condition};
    let store = Store::new(pool);
    store.create_account("a", "A", Some("ag"), &[(Currency::Krw, dec!(1))], Utc::now()).await.unwrap();
    store.create_account("b", "B", Some("ag"), &[(Currency::Krw, dec!(1))], Utc::now()).await.unwrap();
    let btc: InstrumentId = "UPBIT:KRW-BTC".parse().unwrap();
    let alert = |account: &str, condition| Alert {
        id: 0,
        account: account.into(),
        generation: 1,
        condition,
        note: "watch".into(),
        once: true,
        created_at: Utc::now(),
        last_fired_at: None,
    };
    let id = store.create_alert(&alert("a", Condition::PriceAbove { id: btc.clone(), price: dec!(100) })).await.unwrap();
    store.create_alert(&alert("a", Condition::Move { id: btc.clone(), pct: dec!(3), window_minutes: 30 })).await.unwrap();
    store.create_alert(&alert("a", Condition::SessionOpen { venue: Venue::Krx })).await.unwrap();
    store.create_alert(&alert("b", Condition::OrderFilled { id: None })).await.unwrap();
    let a = store.active_alerts("a", 1).await.unwrap();
    assert_eq!(a.len(), 3);
    assert_eq!(a[0].condition, Condition::PriceAbove { id: btc.clone(), price: dec!(100) });
    assert_eq!(a[1].condition, Condition::Move { id: btc.clone(), pct: dec!(3), window_minutes: 30 });
    assert_eq!(a[2].condition, Condition::SessionOpen { venue: Venue::Krx });

    assert!(!store.deactivate_alert("b", id).await.unwrap()); // not b's
    assert!(store.deactivate_alert("a", id).await.unwrap());
    assert!(!store.deactivate_alert("a", id).await.unwrap()); // already off
    assert_eq!(store.active_alerts("a", 1).await.unwrap().len(), 2);

    store.reset_account("a", &[(Currency::Krw, dec!(1))], Utc::now()).await.unwrap();
    let gens = std::collections::HashMap::from([("a".to_string(), 2), ("b".to_string(), 1)]);
    assert_eq!(store.all_active_alerts(&gens).await.unwrap().len(), 1); // a's gen-1 alerts are gone

    let ev = store.record_alert_event(id, "a", Utc::now(), "fired", false, Some("offline")).await.unwrap();
    store.set_event_delivered(ev, true, None).await.unwrap();
    assert_eq!(store.alert_session("a").await.unwrap(), None);
    store.set_alert_session("a", "sess-1").await.unwrap();
    assert_eq!(store.alert_session("a").await.unwrap().as_deref(), Some("sess-1"));
}
