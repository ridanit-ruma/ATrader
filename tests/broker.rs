use std::sync::Arc;

use atrader::broker::*;
use atrader::domain::*;
use atrader::sim::Size;
use atrader::venue::*;
use chrono::{Duration, TimeZone, Utc};
use rust_decimal::Decimal;
use rust_decimal_macros::dec;

fn btc() -> InstrumentId {
    "UPBIT:KRW-BTC".parse().unwrap()
}

fn samsung() -> InstrumentId {
    "KRX:005930".parse().unwrap()
}

fn levels(xs: &[(Decimal, Decimal)]) -> Vec<Level> {
    xs.iter().map(|&(price, qty)| Level { price, qty }).collect()
}

fn book(clock: &ManualClock, id: InstrumentId, bids: &[(Decimal, Decimal)], asks: &[(Decimal, Decimal)]) -> Book {
    Book { instrument: id, bids: levels(bids), asks: levels(asks), prev_close: Some(dec!(70000)), received_at: clock.now() }
}

fn btc_book(clock: &ManualClock) -> Book {
    book(clock, btc(), &[(dec!(99999000), dec!(1))], &[(dec!(100000000), dec!(0.5)), (dec!(100001000), dec!(1))])
}

/// Wednesday 2026-09-23 10:00 KST: KRX is open; crypto always is.
fn setup() -> (SimBroker, ManualClock) {
    let clock = ManualClock::new(Utc.with_ymd_and_hms(2026, 9, 23, 1, 0, 0).unwrap());
    let b = SimBroker::new(Arc::new(clock.clone()), Calendar::default());
    b.add_instrument(Instrument {
        id: btc(),
        name: "Bitcoin".into(),
        tick: TickRule::Fixed(dec!(1000)),
        lot: LotRule { step: dec!(0.00000001), min_qty: dec!(0.00000001), min_notional: dec!(5000) },
        tradable: true,
    });
    b.add_instrument(Instrument { id: samsung(), name: "Samsung Electronics".into(), tick: TickRule::Krx, lot: whole_shares(), tradable: true });
    b.open_account("a", &[(Currency::Krw, dec!(1000000000))]);
    b.on_book(btc_book(&clock));
    b.on_book(book(&clock, samsung(), &[(dec!(70000), dec!(100))], &[(dec!(70100), dec!(100))]));
    (b, clock)
}

fn req(id: InstrumentId, side: Side, kind: OrderType, size: Size, limit_price: Option<Decimal>, tif: Tif) -> OrderRequest {
    OrderRequest { instrument: id, side, kind, size, limit_price, tif, reason: "test".into() }
}

fn market_buy(qty: Decimal) -> OrderRequest {
    req(btc(), Side::Buy, OrderType::Market, Size::Qty(qty), None, Tif::Ioc)
}

#[test]
fn market_buy_walks_the_book_and_moves_the_price() {
    let (b, _) = setup();
    let (order, fills) = b.place_sync("a", market_buy(dec!(0.7))).unwrap();
    assert_eq!(order.status, OrderStatus::Filled);
    assert_eq!(fills.len(), 1);
    assert_eq!(fills[0].qty, dec!(0.7));
    assert_eq!(fills[0].notional, dec!(70000200)); // 0.5 @ 100,000,000 + 0.2 @ 100,001,000
    assert_eq!(fills[0].fee, dec!(35000)); // 5 bps, rounded to whole won
    assert_eq!(fills[0].liquidity, Liquidity::Taker);
    assert!(b.shadow_offset(&btc()) > 0.0);
    let pf = b.portfolio("a").unwrap();
    assert_eq!(pf.cash(Currency::Krw), dec!(1000000000) - dec!(70000200) - dec!(35000));
    assert_eq!(pf.positions[&btc()].qty, dec!(0.7));
}

#[test]
fn back_to_back_buys_pay_more() {
    let (b, _) = setup();
    b.place_sync("a", market_buy(dec!(0.7))).unwrap();
    let (_, fills) = b.place_sync("a", market_buy(dec!(0.3))).unwrap();
    // The first level is depleted and the offset has lifted the rest of the book.
    assert!(fills[0].price > dec!(100001000));
}

#[test]
fn impact_decays_with_time() {
    let (b, clock) = setup();
    b.place_sync("a", market_buy(dec!(0.7))).unwrap();
    let before = b.shadow_offset(&btc());
    clock.advance(Duration::seconds(600)); // crypto tau_perm
    assert!((b.shadow_offset(&btc()) - before / 2.0).abs() < 1e-12);
}

#[test]
fn notional_buy_keeps_fees_inside_the_budget() {
    let (b, _) = setup();
    let r = req(btc(), Side::Buy, OrderType::Market, Size::Notional(dec!(10000000)), None, Tif::Ioc);
    let (order, fills) = b.place_sync("a", r).unwrap();
    assert_eq!(order.status, OrderStatus::Filled);
    assert_eq!(fills[0].qty, dec!(0.09995002));
    assert!(fills[0].notional + fills[0].fee <= dec!(10000000));
}

#[test]
fn malformed_sizes_are_rejected() {
    let (b, _) = setup();
    for qty in [dec!(0), dec!(-1), dec!(0.000000001), dec!(0.00001)] {
        assert!(matches!(b.place_sync("a", market_buy(qty)), Err(OrderError::InvalidQty { .. })), "qty {qty}");
    }
    assert!(b.portfolio("a").unwrap().positions.is_empty());
}

#[test]
fn empty_side_is_no_liquidity() {
    let (b, clock) = setup();
    b.on_book(book(&clock, btc(), &[(dec!(99999000), dec!(1))], &[]));
    assert_eq!(b.place_sync("a", market_buy(dec!(0.1))), Err(OrderError::NoLiquidity));
}

#[test]
fn stale_book_is_rejected() {
    let (b, clock) = setup();
    clock.advance(Duration::seconds(6));
    assert_eq!(b.place_sync("a", market_buy(dec!(0.1))), Err(OrderError::StaleData { age_secs: Some(6) }));
}

#[test]
fn insufficient_funds() {
    let (b, _) = setup();
    b.open_account("poor", &[(Currency::Krw, dec!(1000000))]);
    assert!(matches!(b.place_sync("poor", market_buy(dec!(0.5))), Err(OrderError::InsufficientFunds { .. })));
}

#[test]
fn krx_rejects_when_closed_and_names_next_open() {
    let (b, clock) = setup();
    clock.set(Utc.with_ymd_and_hms(2026, 9, 26, 1, 0, 0).unwrap()); // Saturday
    let r = req(samsung(), Side::Buy, OrderType::Market, Size::Qty(dec!(1)), None, Tif::Ioc);
    assert_eq!(
        b.place_sync("a", r),
        Err(OrderError::MarketClosed { next_open: Some(Utc.with_ymd_and_hms(2026, 9, 28, 0, 0, 0).unwrap()) })
    );
}

#[test]
fn krx_tick_and_price_limit() {
    let (b, _) = setup();
    let limit = |p| req(samsung(), Side::Buy, OrderType::Limit, Size::Qty(dec!(1)), Some(p), Tif::Day);
    assert_eq!(b.place_sync("a", limit(dec!(70050))), Err(OrderError::InvalidTick { lower: dec!(70000), upper: dec!(70100) }));
    assert_eq!(b.place_sync("a", limit(dec!(95000))), Err(OrderError::PriceLimit { lower: dec!(49000), upper: dec!(91000) }));
}

#[test]
fn order_shape_is_checked() {
    let (b, _) = setup();
    let crypto_day = req(btc(), Side::Buy, OrderType::Limit, Size::Qty(dec!(0.1)), Some(dec!(99000000)), Tif::Day);
    assert!(matches!(b.place_sync("a", crypto_day), Err(OrderError::InvalidRequest(_))));
    let limit_without_price = req(btc(), Side::Buy, OrderType::Limit, Size::Qty(dec!(0.1)), None, Tif::Gtc);
    assert!(matches!(b.place_sync("a", limit_without_price), Err(OrderError::InvalidRequest(_))));
}

#[test]
fn estimate_changes_nothing() {
    let (b, _) = setup();
    let before = b.portfolio("a").unwrap();
    let e = b.estimate("a", &market_buy(dec!(0.7))).unwrap();
    assert_eq!(e.filled_qty, dec!(0.7));
    assert_eq!(e.notional, dec!(70000200));
    assert!(e.slippage_bps > dec!(0));
    assert!(e.impact_bps > 0.0);
    assert_eq!(b.portfolio("a").unwrap(), before);
    assert_eq!(b.shadow_offset(&btc()), 0.0);
}

#[tokio::test]
async fn works_through_the_broker_trait() {
    let (b, _) = setup();
    let broker: Arc<dyn Broker> = Arc::new(b);
    let (order, _) = broker.place("a", market_buy(dec!(0.1))).await.unwrap();
    assert_eq!(order.status, OrderStatus::Filled);
}

fn limit(id: InstrumentId, side: Side, qty: Decimal, price: Decimal, tif: Tif) -> OrderRequest {
    req(id, side, OrderType::Limit, Size::Qty(qty), Some(price), tif)
}

fn trade(clock: &ManualClock, id: InstrumentId, price: Decimal, qty: Decimal) -> Trade {
    Trade { instrument: id, price, qty, at: clock.now() }
}

#[test]
fn resting_buy_reserves_cash_and_fills_on_trades() {
    let (b, clock) = setup();
    let (order, fills) = b.place_sync("a", limit(btc(), Side::Buy, dec!(0.1), dec!(99998000), Tif::Gtc)).unwrap();
    assert_eq!((order.status, fills.len()), (OrderStatus::Open, 0));
    let reserved = dec!(0.1) * dec!(99998000) * dec!(1.0005);
    assert_eq!(b.portfolio("a").unwrap().available_cash(Currency::Krw), dec!(1000000000) - reserved);

    let f = b.on_trade(trade(&clock, btc(), dec!(99998000), dec!(0.05)));
    assert_eq!((f[0].qty, f[0].liquidity), (dec!(0.05), Liquidity::Maker));
    let f = b.on_trade(trade(&clock, btc(), dec!(99990000), dec!(1))); // traded through
    assert_eq!(f[0].qty, dec!(0.05));
    assert_eq!(b.order(order.id).unwrap().status, OrderStatus::Filled);
    let pf = b.portfolio("a").unwrap();
    assert_eq!(pf.available_cash(Currency::Krw), pf.cash(Currency::Krw)); // nothing left reserved
}

#[test]
fn queue_ahead_is_served_first() {
    let (b, clock) = setup();
    b.on_book(book(&clock, btc(), &[(dec!(99998000), dec!(0.3))], &[(dec!(100000000), dec!(0.5))]));
    let (order, _) = b.place_sync("a", limit(btc(), Side::Buy, dec!(0.1), dec!(99998000), Tif::Gtc)).unwrap();
    assert!(b.on_trade(trade(&clock, btc(), dec!(99998000), dec!(0.2))).is_empty());
    let f = b.on_trade(trade(&clock, btc(), dec!(99998000), dec!(0.2)));
    assert_eq!(f[0].qty, dec!(0.1));
    assert_eq!(b.order(order.id).unwrap().status, OrderStatus::Filled);
}

#[test]
fn cancel_releases_the_reservation() {
    let (b, _) = setup();
    let (order, _) = b.place_sync("a", limit(btc(), Side::Buy, dec!(0.1), dec!(99998000), Tif::Gtc)).unwrap();
    let cancelled = b.cancel_sync("a", order.id).unwrap();
    assert_eq!(cancelled.status, OrderStatus::Cancelled);
    assert_eq!(b.portfolio("a").unwrap().available_cash(Currency::Krw), dec!(1000000000));
    assert_eq!(b.cancel_sync("someone-else", order.id), Err(OrderError::NotFound));
}

#[test]
fn cannot_sell_shares_already_promised() {
    let (b, _) = setup();
    b.place_sync("a", market_buy(dec!(0.5))).unwrap();
    b.place_sync("a", limit(btc(), Side::Sell, dec!(0.4), dec!(200000000), Tif::Gtc)).unwrap();
    assert_eq!(
        b.place_sync("a", limit(btc(), Side::Sell, dec!(0.2), dec!(200000000), Tif::Gtc)),
        Err(OrderError::InsufficientPosition { available: dec!(0.1) })
    );
}

#[test]
fn day_orders_expire_after_the_close() {
    let (b, clock) = setup();
    let (order, _) = b.place_sync("a", limit(samsung(), Side::Buy, dec!(10), dec!(69900), Tif::Day)).unwrap();
    assert_eq!(order.status, OrderStatus::Open);
    assert!(b.expire_day_orders().is_empty());
    clock.set(Utc.with_ymd_and_hms(2026, 9, 23, 6, 31, 0).unwrap()); // 15:31 KST
    let expired = b.expire_day_orders();
    assert_eq!(expired.len(), 1);
    assert_eq!(expired[0].status, OrderStatus::Expired);
    assert_eq!(b.portfolio("a").unwrap().available_cash(Currency::Krw), dec!(1000000000));
}

#[tokio::test]
async fn cancel_works_through_the_broker_trait() {
    let (b, _) = setup();
    let broker: Arc<dyn Broker> = Arc::new(b);
    let (order, _) = broker.place("a", limit(btc(), Side::Buy, dec!(0.1), dec!(99998000), Tif::Gtc)).await.unwrap();
    assert_eq!(broker.cancel("a", order.id).await.unwrap().status, OrderStatus::Cancelled);
}

// --- Final review fixes ---

#[test]
fn absurd_sizes_are_rejected_without_poisoning_the_broker() {
    let (b, _) = setup();
    assert!(b.place_sync("a", market_buy(dec!(79228162514264337593543950335))).is_err());
    let huge_notional = req(btc(), Side::Buy, OrderType::Market, Size::Notional(dec!(10000000000000000000000000000)), None, Tif::Ioc);
    assert!(b.place_sync("a", huge_notional).is_err());
    let huge_limit = limit(btc(), Side::Buy, dec!(1), dec!(7000000000000000000000000000), Tif::Gtc);
    assert!(b.place_sync("a", huge_limit).is_err());
    assert_eq!(b.place_sync("a", market_buy(dec!(0.1))).unwrap().0.status, OrderStatus::Filled);
}

#[test]
fn zero_price_levels_and_bad_prints_are_ignored() {
    let (b, clock) = setup();
    b.on_book(book(&clock, btc(), &[(dec!(0), dec!(1))], &[(dec!(0), dec!(1))]));
    let r = req(btc(), Side::Buy, OrderType::Market, Size::Notional(dec!(10000)), None, Tif::Ioc);
    assert_eq!(b.place_sync("a", r), Err(OrderError::NoLiquidity));
    assert!(b.estimate("a", &market_buy(dec!(0.1))).is_err());
    b.on_book(btc_book(&clock));
    b.place_sync("a", limit(btc(), Side::Buy, dec!(0.1), dec!(99998000), Tif::Gtc)).unwrap();
    assert!(b.on_trade(trade(&clock, btc(), dec!(99990000), dec!(-1))).is_empty());
    assert!(b.on_trade(trade(&clock, btc(), dec!(0), dec!(1))).is_empty());
}

#[test]
fn instruments_with_zero_tick_or_step_are_refused() {
    let (b, _) = setup();
    let bad: InstrumentId = "UPBIT:KRW-BAD".parse().unwrap();
    b.add_instrument(Instrument {
        id: bad.clone(),
        name: "Bad".into(),
        tick: TickRule::Fixed(dec!(0)),
        lot: LotRule { step: dec!(0), min_qty: dec!(0), min_notional: dec!(0) },
        tradable: true,
    });
    let r = req(bad, Side::Buy, OrderType::Market, Size::Qty(dec!(1)), None, Tif::Ioc);
    assert_eq!(b.place_sync("a", r), Err(OrderError::UnknownInstrument));
}

#[test]
fn notional_buy_below_minimum_after_fees_is_rejected() {
    let (b, _) = setup();
    let r = req(btc(), Side::Buy, OrderType::Market, Size::Notional(dec!(5000)), None, Tif::Ioc);
    assert!(matches!(b.place_sync("a", r), Err(OrderError::InvalidQty { .. })));
    assert!(b.portfolio("a").unwrap().positions.is_empty());
}

#[test]
fn resting_buy_fees_never_overdraw_cash() {
    let (b, clock) = setup();
    b.open_account("tight", &[(Currency::Krw, dec!(500075))]); // exactly 10 x 50,000 + 1.5 bps
    b.place_sync("tight", limit(samsung(), Side::Buy, dec!(10), dec!(50000), Tif::Day)).unwrap();
    for _ in 0..10 {
        b.on_trade(trade(&clock, samsung(), dec!(50000), dec!(1)));
    }
    let pf = b.portfolio("tight").unwrap();
    assert_eq!(pf.positions[&samsung()].qty, dec!(10));
    assert!(pf.cash(Currency::Krw) >= dec!(0), "cash {}", pf.cash(Currency::Krw));
}

#[test]
fn crossed_resting_order_fills_at_its_limit_as_maker() {
    let (b, clock) = setup();
    let (order, _) = b.place_sync("a", limit(btc(), Side::Buy, dec!(0.1), dec!(99998000), Tif::Gtc)).unwrap();
    let f = b.on_book(book(&clock, btc(), &[(dec!(99990000), dec!(1))], &[(dec!(99997000), dec!(0.5))]));
    assert_eq!((f[0].qty, f[0].price, f[0].liquidity), (dec!(0.1), dec!(99998000), Liquidity::Maker));
    assert_eq!(b.order(order.id).unwrap().status, OrderStatus::Filled);
    assert_eq!(b.shadow_offset(&btc()), 0.0); // makers leave no permanent impact
}

#[test]
fn closed_market_books_do_not_fill_day_orders() {
    let (b, clock) = setup();
    let (order, _) = b.place_sync("a", limit(samsung(), Side::Buy, dec!(10), dec!(69900), Tif::Day)).unwrap();
    clock.set(Utc.with_ymd_and_hms(2026, 9, 23, 6, 35, 0).unwrap()); // 15:35 KST
    assert!(b.on_book(book(&clock, samsung(), &[(dec!(69700), dec!(100))], &[(dec!(69800), dec!(100))])).is_empty());
    assert!(b.on_trade(trade(&clock, samsung(), dec!(69800), dec!(100))).is_empty());
    assert_eq!(b.order(order.id).unwrap().status, OrderStatus::Open);
}

#[test]
fn book_age_and_active_instruments() {
    let (b, clock) = setup();
    clock.advance(Duration::seconds(3));
    assert_eq!(b.book_age(&btc()), Some(Duration::seconds(3)));
    assert_eq!(b.book_age(&"UPBIT:KRW-XRP".parse().unwrap()), None);
    clock.set(Utc.with_ymd_and_hms(2026, 9, 23, 1, 0, 0).unwrap());
    b.place_sync("a", market_buy(dec!(0.1))).unwrap();
    b.place_sync("a", limit(samsung(), Side::Buy, dec!(1), dec!(69900), Tif::Day)).unwrap();
    assert_eq!(b.active_instruments(), vec![samsung(), btc()]);
    assert!(!b.has_stats(&btc()));
    b.set_stats(btc(), atrader::sim::DailyStats { sigma: 0.02, adv_notional: dec!(1) });
    assert!(b.has_stats(&btc()));
}

#[test]
fn convert_moves_cash_at_rate_minus_spread() {
    let (b, _) = setup();
    let c = b.convert_sync("a", Currency::Krw, Currency::Usd, dec!(1365350), dec!(1365.35), dec!(0.001)).unwrap();
    assert_eq!((c.debit, c.credit), (dec!(1365350), dec!(999)));
    let pf = b.portfolio("a").unwrap();
    assert_eq!(pf.cash(Currency::Krw), dec!(1000000000) - dec!(1365350));
    assert_eq!(pf.cash(Currency::Usd), dec!(999));
    let back = b.convert_sync("a", Currency::Usd, Currency::Krw, dec!(100), dec!(1365.35), dec!(0.001)).unwrap();
    assert_eq!(back.credit, dec!(136398)); // 136,398.465 truncated
}

#[test]
fn bad_conversions_change_nothing() {
    let (b, _) = setup();
    let before = b.portfolio("a").unwrap();
    assert!(matches!(b.convert_sync("a", Currency::Krw, Currency::Krw, dec!(1), dec!(1365), dec!(0)), Err(OrderError::InvalidRequest(_))));
    assert!(matches!(b.convert_sync("a", Currency::Krw, Currency::Usd, dec!(0), dec!(1365), dec!(0)), Err(OrderError::InvalidRequest(_))));
    assert!(matches!(b.convert_sync("a", Currency::Usd, Currency::Krw, dec!(1), dec!(1365), dec!(0)), Err(OrderError::InsufficientFunds { .. })));
    assert_eq!(b.convert_sync("nobody", Currency::Krw, Currency::Usd, dec!(1), dec!(1365), dec!(0)), Err(OrderError::UnknownAccount));
    assert_eq!(b.portfolio("a").unwrap(), before);
}

#[test]
fn conversions_that_lose_money_are_rejected() {
    let (b, _) = setup();
    let before = b.portfolio("a").unwrap();
    let r = |from, to, amount, spread| b.convert_sync("a", from, to, amount, dec!(1365.35), spread);
    assert!(matches!(r(Currency::Krw, Currency::Usd, dec!(13), dec!(0.001)), Err(OrderError::InvalidRequest(_)))); // credit rounds to 0
    assert!(matches!(r(Currency::Krw, Currency::Usd, dec!(0.5), dec!(0)), Err(OrderError::InvalidRequest(_)))); // below 1 won
    assert!(matches!(r(Currency::Krw, Currency::Usd, dec!(1365350), dec!(-0.1)), Err(OrderError::InvalidRequest(_))));
    assert!(matches!(r(Currency::Krw, Currency::Usd, dec!(1365350), dec!(1)), Err(OrderError::InvalidRequest(_))));
    assert_eq!(b.portfolio("a").unwrap(), before);
}
