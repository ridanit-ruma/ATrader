//! Market data feeds. Each venue streams books and trades for a subscription set that
//! `run_feed` keeps alive across disconnects.

pub mod binance;
pub mod kis;
pub mod upbit;
/// Feeds kept out of the public repository (`src/feed/private/`, git-ignored), compiled in with
/// `--features private-feeds`. They stand in for KIS when it has no keys.
#[cfg(feature = "private-feeds")]
pub mod private;

use std::sync::Arc;
use std::time::{Duration as StdDuration, Instant, SystemTime, UNIX_EPOCH};

use async_trait::async_trait;
use tokio::sync::{mpsc, watch};

use crate::domain::{Book, InstrumentId, Trade, Venue};
use crate::sim::DailyStats;
use crate::venue::Instrument;
use crate::candles::{Candle, Interval};
use crate::screen::{Ranking, ScreenRow};

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
    /// OHLCV candles, oldest first; the last may still be forming.
    async fn candles(&self, _id: &InstrumentId, interval: Interval, _limit: usize) -> anyhow::Result<Vec<Candle>> {
        anyhow::bail!("unsupported: {} has no {} candles", self.venue().tag(), interval.code())
    }

    /// Venue-wide ranking.
    async fn screen(&self, _ranking: Ranking, _limit: usize) -> anyhow::Result<Vec<ScreenRow>> {
        anyhow::bail!("unsupported: {} has no ranking data", self.venue().tag())
    }

    /// Stream books and trades for `ids` into `tx`. Returns `Ok` only when `tx` is closed; any
    /// disconnect or idle timeout is an error so the runner reconnects.
    async fn stream(&self, ids: &[InstrumentId], tx: &mpsc::Sender<MarketEvent>) -> anyhow::Result<()>;
}

/// No data for this long means the socket is dead.
pub const IDLE_TIMEOUT: StdDuration = StdDuration::from_secs(30);

/// The next item of `s`, or an error once nothing has arrived for `IDLE_TIMEOUT`.
pub async fn next_or_idle<S: futures_util::Stream + Unpin>(s: &mut S, venue: &str) -> anyhow::Result<S::Item> {
    use futures_util::StreamExt;
    tokio::time::timeout(IDLE_TIMEOUT, s.next())
        .await
        .map_err(|_| anyhow::anyhow!("{venue}: no data for {IDLE_TIMEOUT:?}"))?
        .ok_or_else(|| anyhow::anyhow!("{venue} websocket closed"))
}

/// How long the subscription set must stay unchanged before the stream restarts with it.
pub const SETTLE: StdDuration = StdDuration::from_secs(1);

/// Keep `feed` streaming whatever `subs` currently holds. A changed set restarts the stream once
/// it has settled for `SETTLE`, so a burst of changes costs one reconnect. Drops reconnect with
/// capped exponential backoff, which a set change cuts short.
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
        let ended = tokio::select! {
            r = feed.stream(&ids, &tx) => Some(r),
            changed = subs.changed() => {
                if changed.is_err() {
                    return;
                }
                None
            }
        };
        let Some(r) = ended else {
            if !settle(&mut subs).await {
                return;
            }
            continue;
        };
        if tx.is_closed() {
            return;
        }
        if let Err(e) = r {
            tracing::warn!(venue = feed.venue().tag(), error = %e, "feed stream ended");
        }
        if started.elapsed() > StdDuration::from_secs(60) {
            backoff = StdDuration::from_secs(1);
        }
        let wait = backoff + jitter(backoff);
        backoff = (backoff * 2).min(StdDuration::from_secs(60));
        tokio::select! {
            _ = tokio::time::sleep(wait) => {}
            changed = subs.changed() => {
                if changed.is_err() || !settle(&mut subs).await {
                    return;
                }
            }
        }
    }
}

/// Wait until `subs` has been quiet for `SETTLE`. False once the sender is gone.
async fn settle(subs: &mut watch::Receiver<Vec<InstrumentId>>) -> bool {
    loop {
        tokio::select! {
            changed = subs.changed() => {
                if changed.is_err() {
                    return false;
                }
            }
            _ = tokio::time::sleep(SETTLE) => return true,
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

    /// Records the ids of every connection, sends one trade, then either hangs or fails.
    struct Recorder {
        calls: std::sync::Mutex<Vec<(tokio::time::Instant, Vec<InstrumentId>)>>,
        fail: bool,
    }

    #[async_trait]
    impl MarketFeed for Recorder {
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
            self.calls.lock().unwrap().push((tokio::time::Instant::now(), ids.to_vec()));
            if self.fail {
                anyhow::bail!("refused");
            }
            let t = Trade { instrument: ids[0].clone(), price: Decimal::ONE, qty: Decimal::ONE, at: Utc::now() };
            tx.send(MarketEvent::Trade(t)).await?;
            std::future::pending::<()>().await;
            Ok(())
        }
    }

    fn id(s: &str) -> InstrumentId {
        format!("UPBIT:{s}").parse().unwrap()
    }

    #[tokio::test(start_paused = true)]
    async fn rapid_set_changes_reconnect_once_with_the_final_set() {
        let feed = Arc::new(Recorder { calls: Default::default(), fail: false });
        let (subs_tx, subs_rx) = watch::channel(vec![id("A")]);
        let (tx, mut rx) = mpsc::channel(8);
        tokio::spawn(run_feed(feed.clone(), subs_rx, tx));
        rx.recv().await.unwrap();
        subs_tx.send(vec![id("A"), id("B")]).unwrap();
        tokio::time::sleep(StdDuration::from_millis(100)).await;
        subs_tx.send(vec![id("A"), id("B"), id("C")]).unwrap();
        rx.recv().await.unwrap();
        let calls = feed.calls.lock().unwrap();
        assert_eq!(calls.len(), 2);
        assert_eq!(calls[1].1, vec![id("A"), id("B"), id("C")]);
    }

    #[tokio::test(start_paused = true)]
    async fn a_set_change_cuts_the_backoff_short() {
        let feed = Arc::new(Recorder { calls: Default::default(), fail: true });
        let (subs_tx, subs_rx) = watch::channel(vec![id("A")]);
        let (tx, _rx) = mpsc::channel(8);
        tokio::spawn(run_feed(feed.clone(), subs_rx, tx));
        tokio::time::sleep(StdDuration::from_secs(20)).await; // several failures: backoff is now >= 8 s
        let changed_at = tokio::time::Instant::now();
        subs_tx.send(vec![id("B")]).unwrap();
        tokio::time::sleep(StdDuration::from_secs(3)).await;
        let calls = feed.calls.lock().unwrap();
        let (at, ids) = calls.last().unwrap();
        assert_eq!(ids, &vec![id("B")]);
        assert!(*at - changed_at < StdDuration::from_secs(3));
    }

    #[tokio::test(start_paused = true)]
    async fn a_silent_stream_times_out() {
        let mut silent = futures_util::stream::pending::<u8>();
        let started = tokio::time::Instant::now();
        assert!(next_or_idle(&mut silent, "test").await.is_err());
        assert!(started.elapsed() >= IDLE_TIMEOUT);
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
