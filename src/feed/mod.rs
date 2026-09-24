//! Market data feeds. Each venue streams books and trades for a subscription set that
//! `run_feed` keeps alive across disconnects.

pub mod binance;
pub mod upbit;

use std::sync::Arc;
use std::time::{Duration as StdDuration, Instant, SystemTime, UNIX_EPOCH};

use async_trait::async_trait;
use tokio::sync::{mpsc, watch};

use crate::domain::{Book, InstrumentId, Trade, Venue};
use crate::sim::DailyStats;
use crate::venue::Instrument;

#[derive(Debug, Clone, PartialEq)]
pub enum MarketEvent {
    Book(Book),
    Trade(Trade),
}

#[async_trait]
pub trait MarketFeed: Send + Sync {
    fn venue(&self) -> Venue;
    async fn instruments(&self) -> anyhow::Result<Vec<Instrument>>;
    async fn snapshot(&self, id: &InstrumentId) -> anyhow::Result<Book>;
    async fn daily_stats(&self, id: &InstrumentId) -> anyhow::Result<DailyStats>;
    /// Stream books and trades for `ids` into `tx`. Returns `Ok` only when `tx` is closed; any
    /// disconnect or idle timeout is an error so the runner reconnects.
    async fn stream(&self, ids: &[InstrumentId], tx: &mpsc::Sender<MarketEvent>) -> anyhow::Result<()>;
}

/// No data for this long means the socket is dead.
pub const IDLE_TIMEOUT: StdDuration = StdDuration::from_secs(30);

/// Keep `feed` streaming whatever `subs` currently holds. A changed set restarts the stream.
/// Drops reconnect with capped exponential backoff.
// ponytail: resubscribing = reconnecting; send in-band SUBSCRIBE messages if churn gets high.
pub async fn run_feed(feed: Arc<dyn MarketFeed>, mut subs: watch::Receiver<Vec<InstrumentId>>, tx: mpsc::Sender<MarketEvent>) {
    let mut backoff = StdDuration::from_secs(1);
    loop {
        let ids = subs.borrow_and_update().clone();
        if ids.is_empty() {
            if subs.changed().await.is_err() {
                return;
            }
            continue;
        }
        let started = Instant::now();
        tokio::select! {
            r = feed.stream(&ids, &tx) => {
                if tx.is_closed() {
                    return;
                }
                if let Err(e) = r {
                    tracing::warn!(venue = feed.venue().tag(), error = %e, "feed stream ended");
                }
                if started.elapsed() > StdDuration::from_secs(60) {
                    backoff = StdDuration::from_secs(1);
                }
                tokio::time::sleep(backoff + jitter(backoff)).await;
                backoff = (backoff * 2).min(StdDuration::from_secs(60));
            }
            changed = subs.changed() => {
                if changed.is_err() {
                    return;
                }
            }
        }
    }
}

/// Up to +25% of `base`, so reconnecting clients do not stampede together.
// ponytail: clock nanoseconds as the random source; enough to spread reconnects.
fn jitter(base: StdDuration) -> StdDuration {
    let n = SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.subsec_nanos());
    base.mul_f64(f64::from(n % 1000) / 4000.0)
}
#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;
    use rust_decimal::Decimal;
    use std::sync::atomic::{AtomicUsize, Ordering};

    /// Sends one trade per connection, then drops.
    struct Flaky {
        calls: AtomicUsize,
    }

    #[async_trait]
    impl MarketFeed for Flaky {
        fn venue(&self) -> Venue {
            Venue::Upbit
        }
        async fn instruments(&self) -> anyhow::Result<Vec<Instrument>> {
            Ok(vec![])
        }
        async fn snapshot(&self, _: &InstrumentId) -> anyhow::Result<Book> {
            anyhow::bail!("unused")
        }
        async fn daily_stats(&self, _: &InstrumentId) -> anyhow::Result<DailyStats> {
            anyhow::bail!("unused")
        }
        async fn stream(&self, ids: &[InstrumentId], tx: &mpsc::Sender<MarketEvent>) -> anyhow::Result<()> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            let t = Trade { instrument: ids[0].clone(), price: Decimal::ONE, qty: Decimal::ONE, at: Utc::now() };
            tx.send(MarketEvent::Trade(t)).await?;
            anyhow::bail!("dropped")
        }
    }

    fn btc() -> InstrumentId {
        "UPBIT:KRW-BTC".parse().unwrap()
    }

    #[tokio::test(start_paused = true)]
    async fn reconnects_after_a_drop_and_stops_when_closed() {
        let feed = Arc::new(Flaky { calls: AtomicUsize::new(0) });
        let (subs_tx, subs_rx) = watch::channel(vec![btc()]);
        let (tx, mut rx) = mpsc::channel(8);
        let task = tokio::spawn(run_feed(feed.clone(), subs_rx, tx));
        rx.recv().await.unwrap();
        rx.recv().await.unwrap();
        assert!(feed.calls.load(Ordering::SeqCst) >= 2);
        drop(subs_tx);
        drop(rx);
        task.await.unwrap();
    }

    #[tokio::test(start_paused = true)]
    async fn idles_until_something_is_subscribed() {
        let feed = Arc::new(Flaky { calls: AtomicUsize::new(0) });
        let (subs_tx, subs_rx) = watch::channel(vec![]);
        let (tx, mut rx) = mpsc::channel(8);
        tokio::spawn(run_feed(feed.clone(), subs_rx, tx));
        tokio::time::sleep(StdDuration::from_secs(5)).await;
        assert_eq!(feed.calls.load(Ordering::SeqCst), 0);
        subs_tx.send(vec![btc()]).unwrap();
        assert!(rx.recv().await.is_some());
    }
}
