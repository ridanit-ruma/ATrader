//! Upbit KRW market feed.

use std::sync::Arc;

use anyhow::{Context, anyhow};
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use futures_util::SinkExt;
use rust_decimal::Decimal;
use rust_decimal_macros::dec;
use serde::Deserialize;
use serde_json::{Value, json};
use tokio::sync::mpsc;
use tokio_tungstenite::tungstenite::Message;

use super::{MarketEvent, MarketFeed, next_or_idle};
use crate::domain::{Book, Clock, InstrumentId, Level, Trade, Venue};
use crate::sim::DailyStats;
use crate::stats::daily_stats;
use crate::venue::{Instrument, LotRule, TickRule};

const REST: &str = "https://api.upbit.com/v1";
const WS: &str = "wss://api.upbit.com/websocket/v1";

#[derive(Deserialize)]
struct Unit {
    ask_price: Decimal,
    bid_price: Decimal,
    ask_size: Decimal,
    bid_size: Decimal,
}

#[derive(Deserialize)]
struct RawBook {
    #[serde(alias = "market")]
    code: String,
    orderbook_units: Vec<Unit>,
}

#[derive(Deserialize)]
struct RawTrade {
    code: String,
    trade_price: Decimal,
    trade_volume: Decimal,
    trade_timestamp: i64,
}

#[derive(Deserialize)]
struct RawMarket {
    market: String,
    korean_name: String,
    english_name: String,
}

#[derive(Deserialize)]
struct RawCandle {
    trade_price: Decimal,
    candle_acc_trade_price: Decimal,
}

fn id(code: &str) -> InstrumentId {
    InstrumentId { venue: Venue::Upbit, symbol: code.to_string() }
}

fn to_book(raw: RawBook, now: DateTime<Utc>) -> Book {
    Book {
        instrument: id(&raw.code),
        bids: raw.orderbook_units.iter().map(|u| Level { price: u.bid_price, qty: u.bid_size }).collect(),
        asks: raw.orderbook_units.iter().map(|u| Level { price: u.ask_price, qty: u.ask_size }).collect(),
        prev_close: None,
        received_at: now,
    }
}

/// Parse one WebSocket frame. Frames that are neither books nor trades yield `None`.
pub fn parse_ws(bytes: &[u8], now: DateTime<Utc>) -> anyhow::Result<Option<MarketEvent>> {
    let v: Value = serde_json::from_slice(bytes)?;
    match v.get("type").and_then(Value::as_str) {
        Some("orderbook") => Ok(Some(MarketEvent::Book(to_book(serde_json::from_value(v)?, now)))),
        Some("trade") => {
            let t: RawTrade = serde_json::from_value(v)?;
            Ok(Some(MarketEvent::Trade(Trade {
                instrument: id(&t.code),
                price: t.trade_price,
                qty: t.trade_volume,
                at: DateTime::from_timestamp_millis(t.trade_timestamp).unwrap_or(now),
            })))
        }
        _ => Ok(None),
    }
}

pub fn parse_book_rest(bytes: &[u8], now: DateTime<Utc>) -> anyhow::Result<Book> {
    let raw: Vec<RawBook> = serde_json::from_slice(bytes)?;
    let first = raw.into_iter().next().ok_or_else(|| anyhow!("empty orderbook response"))?;
    Ok(to_book(first, now))
}

pub fn parse_markets(bytes: &[u8]) -> anyhow::Result<Vec<Instrument>> {
    let raw: Vec<RawMarket> = serde_json::from_slice(bytes)?;
    Ok(raw
        .into_iter()
        .filter(|m| m.market.starts_with("KRW-"))
        .map(|m| Instrument {
            id: id(&m.market),
            name: format!("{} ({})", m.korean_name, m.english_name),
            tick: TickRule::Upbit,
            lot: LotRule { step: dec!(0.00000001), min_qty: dec!(0.00000001), min_notional: dec!(5000) },
            tradable: true,
        })
        .collect())
}

/// Daily candles arrive newest first.
pub fn parse_candles(bytes: &[u8]) -> anyhow::Result<DailyStats> {
    let mut raw: Vec<RawCandle> = serde_json::from_slice(bytes)?;
    raw.reverse();
    let closes: Vec<Decimal> = raw.iter().map(|c| c.trade_price).collect();
    let values: Vec<Decimal> = raw.iter().map(|c| c.candle_acc_trade_price).collect();
    daily_stats(&closes, &values).ok_or_else(|| anyhow!("not enough daily history"))
}

pub fn subscribe_message(ids: &[InstrumentId]) -> String {
    let codes: Vec<&str> = ids.iter().map(|i| i.symbol.as_str()).collect();
    json!([{"ticket": "atrader"}, {"type": "orderbook", "codes": codes}, {"type": "trade", "codes": codes}]).to_string()
}

pub struct UpbitFeed {
    http: reqwest::Client,
    clock: Arc<dyn Clock>,
}

impl UpbitFeed {
    pub fn new(clock: Arc<dyn Clock>) -> Self {
        crate::init_tls();
        UpbitFeed { http: reqwest::Client::new(), clock }
    }

    async fn get(&self, path: &str) -> anyhow::Result<Vec<u8>> {
        let r = self.http.get(format!("{REST}{path}")).send().await?.error_for_status()?;
        Ok(r.bytes().await?.to_vec())
    }
}

#[async_trait]
impl MarketFeed for UpbitFeed {
    fn venue(&self) -> Venue {
        Venue::Upbit
    }

    async fn instruments(&self) -> anyhow::Result<Vec<Instrument>> {
        parse_markets(&self.get("/market/all").await?)
    }

    async fn snapshot(&self, id: &InstrumentId) -> anyhow::Result<Book> {
        let body = self.get(&format!("/orderbook?markets={}", id.symbol)).await?;
        parse_book_rest(&body, self.clock.now()).with_context(|| format!("orderbook for {id}"))
    }

    async fn daily_stats(&self, id: &InstrumentId) -> anyhow::Result<DailyStats> {
        parse_candles(&self.get(&format!("/candles/days?market={}&count=21", id.symbol)).await?)
    }

    async fn stream(&self, ids: &[InstrumentId], tx: &mpsc::Sender<MarketEvent>) -> anyhow::Result<()> {
        let (mut ws, _) = tokio_tungstenite::connect_async(WS).await?;
        ws.send(Message::text(subscribe_message(ids))).await?;
        loop {
            let msg = next_or_idle(&mut ws, "upbit").await??;
            let bytes = match msg {
                Message::Binary(b) => b.to_vec(),
                Message::Text(t) => t.as_bytes().to_vec(),
                Message::Close(_) => return Err(anyhow!("upbit websocket closed")),
                _ => continue,
            };
            if let Some(ev) = parse_ws(&bytes, self.clock.now())? {
                if tx.send(ev).await.is_err() {
                    return Ok(());
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;
    use rust_decimal_macros::dec;

    const BOOK_WS: &str = r#"{"type":"orderbook","code":"KRW-BTC","timestamp":1790247036776,"total_ask_size":0.87,"total_bid_size":2.47,"orderbook_units":[{"ask_price":114850000,"bid_price":114833000,"ask_size":0.11621747,"bid_size":0.00173353},{"ask_price":114851000,"bid_price":114831000,"ask_size":0.16136247,"bid_size":0.00137989}],"stream_type":"REALTIME"}"#;
    const TRADE_WS: &str = r#"{"type":"trade","code":"KRW-BTC","timestamp":1790247036800,"trade_date":"2026-09-24","trade_time":"09:30:36","trade_timestamp":1790247036700,"trade_price":114850000,"trade_volume":0.0021,"ask_bid":"BID","prev_closing_price":115908000,"change":"FALL","change_price":1058000,"sequential_id":1,"stream_type":"REALTIME"}"#;

    fn now() -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 9, 24, 0, 30, 0).unwrap()
    }

    #[test]
    fn parses_orderbook_frames_exactly() {
        let Some(MarketEvent::Book(b)) = parse_ws(BOOK_WS.as_bytes(), now()).unwrap() else { panic!("not a book") };
        assert_eq!(b.instrument.to_string(), "UPBIT:KRW-BTC");
        assert_eq!(b.bids[0], Level { price: dec!(114833000), qty: dec!(0.00173353) });
        assert_eq!(b.asks[1], Level { price: dec!(114851000), qty: dec!(0.16136247) });
        assert_eq!(b.received_at, now());
    }

    #[test]
    fn keeps_every_digit_of_long_numbers() {
        let f = r#"{"type":"orderbook","code":"KRW-SHIB","orderbook_units":[{"ask_price":0.01234,"bid_price":0.01233,"ask_size":409234523571.02874,"bid_size":99999999.99999999}]}"#;
        let Some(MarketEvent::Book(b)) = parse_ws(f.as_bytes(), now()).unwrap() else { panic!("not a book") };
        assert_eq!(b.asks[0].qty, dec!(409234523571.02874));
        assert_eq!(b.bids[0].qty, dec!(99999999.99999999));
    }

    #[test]
    fn parses_trade_frames() {
        let Some(MarketEvent::Trade(t)) = parse_ws(TRADE_WS.as_bytes(), now()).unwrap() else { panic!("not a trade") };
        assert_eq!((t.price, t.qty), (dec!(114850000), dec!(0.0021)));
        assert_eq!(t.at.timestamp_millis(), 1790247036700);
    }

    #[test]
    fn ignores_other_frames() {
        assert_eq!(parse_ws(br#"{"type":"ticker","code":"KRW-BTC"}"#, now()).unwrap(), None);
        assert_eq!(parse_ws(br#"{"status":"UP"}"#, now()).unwrap(), None);
    }

    #[test]
    fn parses_rest_orderbook() {
        let body = format!("[{}]", BOOK_WS.replace(r#""code""#, r#""market""#));
        let b = parse_book_rest(body.as_bytes(), now()).unwrap();
        assert_eq!(b.asks.len(), 2);
    }

    #[test]
    fn loads_only_krw_markets() {
        let body = r#"[{"market":"BTC-FIL","korean_name":"파일코인","english_name":"Filecoin"},{"market":"KRW-BTC","korean_name":"비트코인","english_name":"Bitcoin"}]"#;
        let list = parse_markets(body.as_bytes()).unwrap();
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].id.to_string(), "UPBIT:KRW-BTC");
        assert_eq!(list[0].name, "비트코인 (Bitcoin)");
        assert_eq!(list[0].tick, TickRule::Upbit);
        assert_eq!(list[0].lot.min_notional, dec!(5000));
    }

    #[test]
    fn candles_newest_first_become_stats() {
        let body = r#"[{"trade_price":99,"candle_acc_trade_price":30},{"trade_price":110,"candle_acc_trade_price":20},{"trade_price":100,"candle_acc_trade_price":10}]"#;
        let s = parse_candles(body.as_bytes()).unwrap();
        assert!((s.sigma - 0.141895).abs() < 1e-5);
        assert_eq!(s.adv_notional, dec!(20));
    }

    #[test]
    fn subscribe_message_lists_codes_for_both_types() {
        let ids: Vec<InstrumentId> = vec!["UPBIT:KRW-BTC".parse().unwrap(), "UPBIT:KRW-ETH".parse().unwrap()];
        let v: Value = serde_json::from_str(&subscribe_message(&ids)).unwrap();
        assert_eq!(v[1]["type"], "orderbook");
        assert_eq!(v[1]["codes"], serde_json::json!(["KRW-BTC", "KRW-ETH"]));
        assert_eq!(v[2]["type"], "trade");
    }
}
