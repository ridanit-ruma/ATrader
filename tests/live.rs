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
