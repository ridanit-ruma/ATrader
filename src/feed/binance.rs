//! Binance USDT spot feed.

use std::sync::Arc;

use anyhow::{Context, anyhow};
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use rust_decimal::Decimal;
use serde::Deserialize;
use serde_json::Value;
use tokio::sync::mpsc;
use tokio_tungstenite::tungstenite::Message;

use super::{MarketEvent, MarketFeed, next_or_idle};
use crate::domain::{Book, Clock, InstrumentId, Level, Trade, Venue};
use crate::sim::DailyStats;
use crate::stats::daily_stats;
use crate::venue::{Instrument, LotRule, TickRule};

const REST: &str = "https://api.binance.com/api/v3";
const WS: &str = "wss://stream.binance.com:9443/stream?streams=";

#[derive(Deserialize)]
struct RawDepth {
    bids: Vec<(Decimal, Decimal)>,
    asks: Vec<(Decimal, Decimal)>,
}

#[derive(Deserialize)]
struct RawTrade {
    s: String,
    p: Decimal,
    q: Decimal,
    #[serde(rename = "T")]
    time: i64,
}

#[derive(Deserialize)]
struct Envelope {
    stream: String,
    data: Value,
}

#[derive(Deserialize)]
struct RawInfo {
    symbols: Vec<RawSymbol>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawSymbol {
    symbol: String,
    status: String,
    base_asset: String,
    quote_asset: String,
    filters: Vec<Value>,
}

fn id(symbol: &str) -> InstrumentId {
    InstrumentId { venue: Venue::Binance, symbol: symbol.to_uppercase() }
}

fn to_book(symbol: &str, d: RawDepth, now: DateTime<Utc>) -> Book {
    let levels = |v: Vec<(Decimal, Decimal)>| v.into_iter().map(|(price, qty)| Level { price, qty }).collect();
    Book { instrument: id(symbol), bids: levels(d.bids), asks: levels(d.asks), prev_close: None, received_at: now }
}

/// Parse one combined-stream frame. Streams other than depth and trade yield `None`.
pub fn parse_ws(bytes: &[u8], now: DateTime<Utc>) -> anyhow::Result<Option<MarketEvent>> {
    let env: Envelope = serde_json::from_slice(bytes)?;
    let (symbol, kind) = env.stream.split_once('@').ok_or_else(|| anyhow!("bad stream name {}", env.stream))?;
    if kind.starts_with("depth") {
        Ok(Some(MarketEvent::Book(to_book(symbol, serde_json::from_value(env.data)?, now))))
    } else if kind == "trade" {
        let t: RawTrade = serde_json::from_value(env.data)?;
        Ok(Some(MarketEvent::Trade(Trade {
            instrument: id(&t.s),
            price: t.p,
            qty: t.q,
            at: DateTime::from_timestamp_millis(t.time).unwrap_or(now),
        })))
    } else {
        Ok(None)
    }
}

pub fn parse_depth(symbol: &str, bytes: &[u8], now: DateTime<Utc>) -> anyhow::Result<Book> {
    Ok(to_book(symbol, serde_json::from_slice(bytes)?, now))
}

pub fn parse_exchange_info(bytes: &[u8]) -> anyhow::Result<Vec<Instrument>> {
    let info: RawInfo = serde_json::from_slice(bytes)?;
    Ok(info
        .symbols
        .into_iter()
        .filter(|s| s.status == "TRADING" && s.quote_asset == "USDT")
        .filter_map(|s| {
            let filter = |kind: &str, key: &str| -> Option<Decimal> {
                let f = s.filters.iter().find(|f| f["filterType"] == kind)?;
                f.get(key)?.as_str()?.parse::<Decimal>().ok().map(|d| d.normalize())
            };
            Some(Instrument {
                id: id(&s.symbol),
                name: format!("{}/{}", s.base_asset, s.quote_asset),
                tick: TickRule::Fixed(filter("PRICE_FILTER", "tickSize")?),
                lot: LotRule {
                    step: filter("LOT_SIZE", "stepSize")?,
                    min_qty: filter("LOT_SIZE", "minQty")?,
                    min_notional: filter("NOTIONAL", "minNotional")
                        .or_else(|| filter("MIN_NOTIONAL", "minNotional"))
                        .unwrap_or_default(),
                },
                tradable: true,
            })
        })
        .collect())
}

/// Daily klines, oldest first: index 4 is the close, index 7 the quote-asset volume.
pub fn parse_klines(bytes: &[u8]) -> anyhow::Result<DailyStats> {
    let rows: Vec<Vec<Value>> = serde_json::from_slice(bytes)?;
    let field = |r: &Vec<Value>, i: usize| -> anyhow::Result<Decimal> {
        r.get(i).and_then(Value::as_str).ok_or_else(|| anyhow!("kline field {i} missing"))?.parse().map_err(Into::into)
    };
    let closes = rows.iter().map(|r| field(r, 4)).collect::<anyhow::Result<Vec<_>>>()?;
    let values = rows.iter().map(|r| field(r, 7)).collect::<anyhow::Result<Vec<_>>>()?;
    daily_stats(&closes, &values).ok_or_else(|| anyhow!("not enough daily history"))
}

pub fn stream_url(ids: &[InstrumentId]) -> String {
    let streams: Vec<String> = ids
        .iter()
        .map(|i| {
            let s = i.symbol.to_lowercase();
            format!("{s}@depth20@100ms/{s}@trade")
        })
        .collect();
    format!("{WS}{}", streams.join("/"))
}

pub struct BinanceFeed {
    http: reqwest::Client,
    clock: Arc<dyn Clock>,
}

impl BinanceFeed {
    pub fn new(clock: Arc<dyn Clock>) -> Self {
        crate::init_tls();
        BinanceFeed { http: reqwest::Client::new(), clock }
    }

    async fn get(&self, path: &str) -> anyhow::Result<Vec<u8>> {
        let r = self.http.get(format!("{REST}{path}")).send().await?.error_for_status()?;
        Ok(r.bytes().await?.to_vec())
    }
}

#[async_trait]
impl MarketFeed for BinanceFeed {
    fn venue(&self) -> Venue {
        Venue::Binance
    }

    async fn instruments(&self) -> anyhow::Result<Vec<Instrument>> {
        parse_exchange_info(&self.get("/exchangeInfo?permissions=SPOT").await?)
    }

    async fn snapshot(&self, id: &InstrumentId) -> anyhow::Result<Book> {
        let body = self.get(&format!("/depth?symbol={}&limit=20", id.symbol)).await?;
        parse_depth(&id.symbol, &body, self.clock.now()).with_context(|| format!("depth for {id}"))
    }

    async fn daily_stats(&self, id: &InstrumentId) -> anyhow::Result<DailyStats> {
        parse_klines(&self.get(&format!("/klines?symbol={}&interval=1d&limit=21", id.symbol)).await?)
    }

    async fn stream(&self, ids: &[InstrumentId], tx: &mpsc::Sender<MarketEvent>) -> anyhow::Result<()> {
        let (mut ws, _) = tokio_tungstenite::connect_async(stream_url(ids)).await?;
        loop {
            let msg = next_or_idle(&mut ws, "binance").await??;
            let bytes = match msg {
                Message::Text(t) => t.as_bytes().to_vec(),
                Message::Binary(b) => b.to_vec(),
                Message::Close(_) => return Err(anyhow!("binance websocket closed")),
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

    fn now() -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 9, 24, 0, 30, 0).unwrap()
    }

    #[test]
    fn parses_depth_frames() {
        let f = r#"{"stream":"btcusdt@depth20@100ms","data":{"lastUpdateId":1,"bids":[["83532.15000000","1.53981000"]],"asks":[["83532.16000000","4.45844000"],["83532.17000000","0.00039000"]]}}"#;
        let Some(MarketEvent::Book(b)) = parse_ws(f.as_bytes(), now()).unwrap() else { panic!("not a book") };
        assert_eq!(b.instrument.to_string(), "BINANCE:BTCUSDT");
        assert_eq!(b.bids[0], Level { price: dec!(83532.15), qty: dec!(1.53981) });
        assert_eq!(b.asks.len(), 2);
    }

    #[test]
    fn parses_trade_frames() {
        let f = r#"{"stream":"btcusdt@trade","data":{"e":"trade","E":1,"s":"BTCUSDT","t":5,"p":"83532.16000000","q":"0.00100000","T":1790247036700,"m":true,"M":true}}"#;
        let Some(MarketEvent::Trade(t)) = parse_ws(f.as_bytes(), now()).unwrap() else { panic!("not a trade") };
        assert_eq!((t.price, t.qty), (dec!(83532.16), dec!(0.001)));
        assert_eq!(t.at.timestamp_millis(), 1790247036700);
    }

    #[test]
    fn exchange_info_keeps_trading_usdt_symbols() {
        let body = r#"{"symbols":[
          {"symbol":"BTCUSDT","status":"TRADING","baseAsset":"BTC","quoteAsset":"USDT","filters":[
            {"filterType":"PRICE_FILTER","minPrice":"0.01000000","maxPrice":"1000000.00000000","tickSize":"0.01000000"},
            {"filterType":"LOT_SIZE","minQty":"0.00001000","maxQty":"9000.00000000","stepSize":"0.00001000"},
            {"filterType":"NOTIONAL","minNotional":"5.00000000","applyMinToMarket":true}]},
          {"symbol":"ETHBTC","status":"TRADING","baseAsset":"ETH","quoteAsset":"BTC","filters":[]},
          {"symbol":"OLDUSDT","status":"BREAK","baseAsset":"OLD","quoteAsset":"USDT","filters":[]}]}"#;
        let list = parse_exchange_info(body.as_bytes()).unwrap();
        assert_eq!(list.len(), 1);
        let i = &list[0];
        assert_eq!((i.id.to_string(), i.name.as_str()), ("BINANCE:BTCUSDT".to_string(), "BTC/USDT"));
        assert_eq!(i.tick, TickRule::Fixed(dec!(0.01)));
        assert_eq!((i.lot.step, i.lot.min_qty, i.lot.min_notional), (dec!(0.00001), dec!(0.00001), dec!(5)));
    }

    #[test]
    fn klines_become_stats() {
        let body = r#"[[0,"1","1","1","100","1",0,"10",1,"1","1","0"],[0,"1","1","1","110","1",0,"20",1,"1","1","0"],[0,"1","1","1","99","1",0,"30",1,"1","1","0"]]"#;
        let s = parse_klines(body.as_bytes()).unwrap();
        assert!((s.sigma - 0.141895).abs() < 1e-5);
        assert_eq!(s.adv_notional, dec!(20));
    }

    #[test]
    fn stream_url_combines_depth_and_trade() {
        let ids: Vec<InstrumentId> = vec!["BINANCE:BTCUSDT".parse().unwrap(), "BINANCE:ETHUSDT".parse().unwrap()];
        assert_eq!(
            stream_url(&ids),
            "wss://stream.binance.com:9443/stream?streams=btcusdt@depth20@100ms/btcusdt@trade/ethusdt@depth20@100ms/ethusdt@trade"
        );
    }
}
