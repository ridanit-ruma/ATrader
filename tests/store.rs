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
    tx.send(Journal::Order(order(1))).unwrap();
    tx.send(Journal::Fill(fill(1, Side::Buy, dec!(10), dec!(700000), dec!(105), dec!(0)))).unwrap();
    let c = Conversion { from: Currency::Krw, to: Currency::Usd, debit: dec!(1365350), credit: dec!(999), rate: dec!(0.00073167) };
    tx.send(Journal::Conversion { account: "a".into(), conversion: c, at: Utc::now() }).unwrap();
    drop(tx);
    persist(rx, store.clone(), bus, std::collections::HashMap::new()).await;

    let cash = store.cash_balances("a", 1).await.unwrap();
    assert_eq!(cash[&Currency::Krw], dec!(2000000) - dec!(700105) - dec!(1365350));
    assert_eq!(cash[&Currency::Usd], dec!(999));
    assert!(matches!(events.recv().await.unwrap(), BusEvent::Order(o) if o.id == 1));
    assert!(matches!(events.recv().await.unwrap(), BusEvent::Fill(f) if f.order_id == 1));
}

#[sqlx::test]
async fn persister_keeps_the_generation_it_started_with(pool: PgPool) {
    use atrader::persist::persist;
    use std::collections::HashMap;
    use std::sync::Arc;
    let store = Arc::new(Store::new(pool));
    store.create_account("a", "Test", None, &[(Currency::Krw, dec!(2000000))], Utc::now()).await.unwrap();
    let started_with = HashMap::from([("a".to_string(), 1)]);
    store.reset_account("a", &[(Currency::Krw, dec!(5))], Utc::now()).await.unwrap(); // reset while serving
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
    let (bus, _) = tokio::sync::broadcast::channel(16);
    tx.send(Journal::Order(order(1))).unwrap();
    tx.send(Journal::Fill(fill(1, Side::Buy, dec!(10), dec!(700000), dec!(105), dec!(0)))).unwrap();
    drop(tx);
    persist(rx, store.clone(), bus, started_with).await;
    assert_eq!(store.cash_balances("a", 1).await.unwrap()[&Currency::Krw], dec!(2000000) - dec!(700105));
    assert_eq!(store.cash_balances("a", 2).await.unwrap()[&Currency::Krw], dec!(5));
}

#[sqlx::test]
async fn persister_skips_integrity_failures_without_stalling(pool: PgPool) {
    use atrader::persist::persist;
    use std::collections::HashMap;
    use std::sync::Arc;
    let store = Arc::new(Store::new(pool));
    store.create_account("a", "Test", None, &[(Currency::Krw, dec!(2000000))], Utc::now()).await.unwrap();
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
    let (bus, _) = tokio::sync::broadcast::channel(16);
    tx.send(Journal::Fill(fill(99, Side::Buy, dec!(1), dec!(1), dec!(0), dec!(0)))).unwrap(); // no order 99
    tx.send(Journal::Order(order(1))).unwrap();
    drop(tx);
    let started = std::time::Instant::now();
    persist(rx, store.clone(), bus, HashMap::new()).await;
    assert!(started.elapsed() < std::time::Duration::from_secs(5), "retried a permanent failure");
    assert_eq!(store.max_order_id().await.unwrap(), 1);
}
