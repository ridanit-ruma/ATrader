use atrader::broker::*;
use atrader::domain::*;
use atrader::sim::Size;
use atrader::store::Store;
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
