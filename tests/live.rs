//! Network smoke tests against the real venues. Run: `cargo test --test live -- --ignored`.

use std::sync::Arc;
use std::time::Duration;

use atrader::domain::{InstrumentId, SystemClock};
use atrader::feed::binance::BinanceFeed;
use atrader::feed::upbit::UpbitFeed;
use atrader::feed::{MarketEvent, MarketFeed};
use atrader::fx::FxCache;
use tokio::sync::mpsc;

async fn check(feed: Arc<dyn MarketFeed>, id: InstrumentId) {
    let instruments = feed.instruments().await.unwrap();
    let inst = instruments.iter().find(|i| i.id == id).expect("instrument listed").clone();
    let book = feed.snapshot(&id).await.unwrap();
    assert!(!book.asks.is_empty() && !book.bids.is_empty());
    assert!(inst.tick.is_valid(book.asks[0].price), "best ask {} off tick", book.asks[0].price);
    assert!(feed.daily_stats(&id).await.unwrap().sigma > 0.0);

    let (tx, mut rx) = mpsc::channel(256);
    let streaming = feed.clone();
    let ids = vec![id.clone()];
    tokio::spawn(async move { streaming.stream(&ids, &tx).await });
    let (mut books, mut trades) = (0, 0);
    let deadline = tokio::time::Instant::now() + Duration::from_secs(20);
    while (books == 0 || trades == 0) && tokio::time::Instant::now() < deadline {
        match tokio::time::timeout(Duration::from_secs(20), rx.recv()).await {
            Ok(Some(MarketEvent::Book(b))) => {
                assert_eq!(b.instrument, id);
                books += 1;
            }
            Ok(Some(MarketEvent::Trade(_))) => trades += 1,
            _ => break,
        }
    }
    assert!(books > 0, "no books streamed");
    assert!(trades > 0, "no trades streamed");
}

#[tokio::test]
#[ignore]
async fn upbit_live() {
    check(Arc::new(UpbitFeed::new(Arc::new(SystemClock))), "UPBIT:KRW-BTC".parse().unwrap()).await;
}

#[tokio::test]
#[ignore]
async fn binance_live() {
    check(Arc::new(BinanceFeed::new(Arc::new(SystemClock))), "BINANCE:BTCUSDT".parse().unwrap()).await;
}

#[tokio::test]
#[ignore]
async fn fx_live() {
    let rate = FxCache::new().usd_krw().await.unwrap();
    assert!(rate > rust_decimal::Decimal::from(500) && rate < rust_decimal::Decimal::from(5000), "rate {rate}");
}

fn kis() -> Option<Arc<atrader::feed::kis::KisClient>> {
    atrader::feed::kis::KisConfig::from_env().map(|c| Arc::new(atrader::feed::kis::KisClient::new(c)))
}

#[tokio::test]
#[ignore]
async fn kis_krx_live() {
    let Some(client) = kis() else { return eprintln!("KIS keys not set; skipped") };
    let cal = atrader::venue::Calendar::from_toml(include_str!("../holidays.toml")).unwrap();
    let feed = Arc::new(atrader::feed::kis::KisKrxFeed::new(client, Arc::new(SystemClock), cal));
    let id: InstrumentId = "KRX:005930".parse().unwrap();
    assert!(feed.instruments().await.unwrap().iter().any(|i| i.id == id));
    let book = feed.snapshot(&id).await.unwrap();
    assert!(book.prev_close.is_some() && !book.asks.is_empty(), "{book:?}");
    assert!(feed.daily_stats(&id).await.unwrap().sigma > 0.0);
}

#[tokio::test]
#[ignore]
async fn kis_us_live() {
    let Some(client) = kis() else { return eprintln!("KIS keys not set; skipped") };
    let cal = atrader::venue::Calendar::from_toml(include_str!("../holidays.toml")).unwrap();
    let feed = Arc::new(atrader::feed::kis::KisUsFeed::new(client, Arc::new(SystemClock), cal));
    let id: InstrumentId = "US:AAPL".parse().unwrap();
    assert!(feed.instruments().await.unwrap().iter().any(|i| i.id == id));
    let book = feed.snapshot(&id).await.unwrap();
    assert!(!book.bids.is_empty() || !book.asks.is_empty(), "{book:?}");
    assert!(feed.daily_stats(&id).await.unwrap().sigma > 0.0);
}

#[tokio::test]
#[ignore]
async fn crypto_candles_and_screens_live() {
    use atrader::candles::Interval;
    use atrader::screen::Ranking;
    let upbit = UpbitFeed::new(Arc::new(SystemClock));
    let c = upbit.candles(&"UPBIT:KRW-BTC".parse().unwrap(), Interval::M5, 10).await.unwrap();
    assert_eq!(c.len(), 10);
    assert!(c[0].start < c[9].start);
    assert_eq!(upbit.screen(Ranking::Value, 5).await.unwrap().len(), 5);
    let binance = BinanceFeed::new(Arc::new(SystemClock));
    assert_eq!(binance.candles(&"BINANCE:BTCUSDT".parse().unwrap(), Interval::D1, 3).await.unwrap().len(), 3);
    assert_eq!(binance.screen(Ranking::Gainers, 5).await.unwrap().len(), 5);
}

#[tokio::test]
#[ignore]
async fn edgar_live() {
    let c = atrader::fundamentals::edgar::EdgarClient::new("ATrader test contact@example.com".into());
    let f = c.fundamentals("AAPL").await.unwrap();
    assert!(f.annual[0].revenue.unwrap() > rust_decimal::Decimal::from(100_000_000_000u64));
    let filings = c.filings("AAPL", None, 5).await.unwrap();
    assert_eq!(filings.len(), 5);
    let q = c.filings("AAPL", None, 50).await.unwrap().into_iter().find(|f| f.form == "10-Q").unwrap();
    let text = c.filing_text("AAPL", &q.id).await.unwrap();
    assert!(text.len() > 10_000 && text.contains("Apple"), "{}", &text[..200.min(text.len())]);
}
