//! Wires feeds to the broker: an event pump, and a facade that keeps instruments loaded,
//! subscribed and fresh.

use std::collections::HashMap;
use std::sync::Arc;

use anyhow::anyhow;
use chrono::Duration;
use tokio::sync::{broadcast, mpsc, watch};

use crate::broker::{Fill, Order, SimBroker};
use crate::domain::{InstrumentId, Venue};
use crate::feed::{MarketEvent, MarketFeed};
use crate::subs::Subscriptions;

/// Everything downstream consumers (SSE, alerts) listen to. Orders and fills arrive here only
/// after they are persisted (see persist.rs).
#[derive(Debug, Clone)]
pub enum BusEvent {
    Market(MarketEvent),
    Order(Order),
    Fill(Fill),
}

/// Apply each market event to the broker, then broadcast it. Fills it causes travel through the
/// broker's journal.
pub async fn pump(mut rx: mpsc::Receiver<MarketEvent>, broker: Arc<SimBroker>, bus: broadcast::Sender<BusEvent>) {
    while let Some(ev) = rx.recv().await {
        match &ev {
            MarketEvent::Book(b) => {
                broker.on_book(b.clone());
            }
            MarketEvent::Trade(t) => {
                broker.on_trade(t.clone());
            }
        }
        let _ = bus.send(BusEvent::Market(ev));
    }
}

struct VenueFeed {
    feed: Arc<dyn MarketFeed>,
    subs: Subscriptions,
}

pub struct Market {
    broker: Arc<SimBroker>,
    venues: HashMap<Venue, VenueFeed>,
}

impl Market {
    pub fn new(broker: Arc<SimBroker>) -> Self {
        Market { broker, venues: HashMap::new() }
    }

    /// Register a feed. Hand the returned receiver to `feed::run_feed`.
    pub fn add_feed(&mut self, feed: Arc<dyn MarketFeed>, cap: usize) -> watch::Receiver<Vec<InstrumentId>> {
        let (subs, rx) = Subscriptions::new(cap);
        self.venues.insert(feed.venue(), VenueFeed { feed, subs });
        rx
    }

    /// Load every feed's instrument list into the broker. Returns how many were offered.
    pub async fn load_instruments(&self) -> anyhow::Result<usize> {
        let mut n = 0;
        for vf in self.venues.values() {
            for i in vf.feed.instruments().await? {
                self.broker.add_instrument(i);
                n += 1;
            }
        }
        Ok(n)
    }

    /// Make sure the broker has daily stats and a book at most 2 s old for `id`, and keep it
    /// subscribed. Fills caused by a fetched snapshot go out on the bus.
    pub async fn ensure_fresh(&self, id: &InstrumentId) -> anyhow::Result<()> {
        let vf = self.venues.get(&id.venue).ok_or_else(|| anyhow!("no feed for venue {}", id.venue.tag()))?;
        vf.subs.touch(id);
        if !self.broker.has_stats(id) {
            // On failure the broker keeps using its defaults and the next call retries.
            // ponytail: stats load once per process; refresh daily when runs last longer than a day.
            match vf.feed.daily_stats(id).await {
                Ok(stats) => self.broker.set_stats(id.clone(), stats),
                Err(e) => tracing::warn!(instrument = %id, error = %e, "daily stats unavailable; using defaults"),
            }
        }
        let fresh = self.broker.book_age(id).is_some_and(|age| age <= Duration::seconds(2));
        if !fresh {
            let book = vf.feed.snapshot(id).await?;
            self.broker.on_book(book);
        }
        Ok(())
    }

    /// Pin every instrument with a position or resting order to its venue's subscription set.
    /// Call after fills and on a timer.
    pub fn refresh_pins(&self) {
        let active = self.broker.active_instruments();
        for (venue, vf) in &self.venues {
            vf.subs.pin(active.iter().filter(|i| i.venue == *venue).cloned().collect());
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use crate::broker::{OrderRequest, OrderType, Tif};
    use crate::domain::{Book, Clock, Currency, Level, ManualClock, Side, Trade};
    use crate::sim::{DailyStats, Size};
    use crate::venue::{Calendar, Instrument, LotRule, TickRule};
    use async_trait::async_trait;
    use chrono::{TimeZone, Utc};
    use rust_decimal_macros::dec;
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn btc() -> InstrumentId {
        "UPBIT:KRW-BTC".parse().unwrap()
    }

    fn book(clock: &ManualClock) -> Book {
        Book {
            instrument: btc(),
            bids: vec![Level { price: dec!(99999000), qty: dec!(1) }],
            asks: vec![Level { price: dec!(100000000), qty: dec!(1) }],
            prev_close: None,
            received_at: clock.now(),
        }
    }

    struct Fake {
        clock: ManualClock,
        snaps: AtomicUsize,
        stats: AtomicUsize,
        fail_first_stats: bool,
    }

    #[async_trait]
    impl MarketFeed for Fake {
        fn venue(&self) -> Venue {
            Venue::Upbit
        }
        async fn instruments(&self) -> anyhow::Result<Vec<Instrument>> {
            Ok(vec![Instrument {
                id: btc(),
                name: "Bitcoin".into(),
                tick: TickRule::Upbit,
                lot: LotRule { step: dec!(0.00000001), min_qty: dec!(0.00000001), min_notional: dec!(5000) },
                tradable: true,
            }])
        }
        async fn snapshot(&self, _: &InstrumentId) -> anyhow::Result<Book> {
            self.snaps.fetch_add(1, Ordering::SeqCst);
            Ok(book(&self.clock))
        }
        async fn daily_stats(&self, _: &InstrumentId) -> anyhow::Result<DailyStats> {
            if self.stats.fetch_add(1, Ordering::SeqCst) == 0 && self.fail_first_stats {
                anyhow::bail!("transient");
            }
            Ok(DailyStats { sigma: 0.02, adv_notional: dec!(50000000000) })
        }
        async fn stream(&self, _: &[InstrumentId], _: &mpsc::Sender<MarketEvent>) -> anyhow::Result<()> {
            anyhow::bail!("unused")
        }
    }

    struct Rig {
        clock: ManualClock,
        broker: Arc<SimBroker>,
        feed: Arc<Fake>,
        market: Market,
        subs: watch::Receiver<Vec<InstrumentId>>,
        bus: broadcast::Sender<BusEvent>,
    }

    async fn rig() -> Rig {
        rig_with(false).await
    }

    async fn rig_with(fail_first_stats: bool) -> Rig {
        let clock = ManualClock::new(Utc.with_ymd_and_hms(2026, 9, 23, 1, 0, 0).unwrap());
        let broker = Arc::new(SimBroker::new(Arc::new(clock.clone()), Calendar::default()));
        broker.open_account("a", &[(Currency::Krw, dec!(1000000000))]);
        let (bus, _) = broadcast::channel(64);
        let mut market = Market::new(broker.clone());
        let feed = Arc::new(Fake { clock: clock.clone(), snaps: AtomicUsize::new(0), stats: AtomicUsize::new(0), fail_first_stats });
        let subs = market.add_feed(feed.clone(), 10);
        assert_eq!(market.load_instruments().await.unwrap(), 1);
        Rig { clock, broker, feed, market, subs, bus }
    }

    #[tokio::test]
    async fn ensure_fresh_snapshots_only_when_stale_and_subscribes() {
        let r = rig().await;
        r.market.ensure_fresh(&btc()).await.unwrap();
        assert_eq!(r.feed.snaps.load(Ordering::SeqCst), 1);
        assert_eq!(r.subs.borrow().clone(), vec![btc()]);
        assert!(r.broker.has_stats(&btc()));
        r.market.ensure_fresh(&btc()).await.unwrap();
        assert_eq!(r.feed.snaps.load(Ordering::SeqCst), 1);
        r.clock.advance(chrono::Duration::seconds(3));
        r.market.ensure_fresh(&btc()).await.unwrap();
        assert_eq!(r.feed.snaps.load(Ordering::SeqCst), 2);
        assert_eq!(r.feed.stats.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn a_failed_stats_fetch_is_retried_later() {
        let r = rig_with(true).await;
        r.market.ensure_fresh(&btc()).await.unwrap();
        assert!(!r.broker.has_stats(&btc()));
        r.market.ensure_fresh(&btc()).await.unwrap();
        assert!(r.broker.has_stats(&btc()));
        assert_eq!(r.feed.stats.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn unknown_venue_is_an_error() {
        let r = rig().await;
        assert!(r.market.ensure_fresh(&"BINANCE:BTCUSDT".parse().unwrap()).await.is_err());
    }

    #[tokio::test]
    async fn refresh_pins_keeps_positions_subscribed() {
        let r = rig().await;
        r.market.ensure_fresh(&btc()).await.unwrap();
        let buy = OrderRequest {
            instrument: btc(),
            side: Side::Buy,
            kind: OrderType::Market,
            size: Size::Qty(dec!(0.1)),
            limit_price: None,
            tif: Tif::Ioc,
            reason: "test".into(),
        };
        r.broker.place_sync("a", buy).unwrap();
        r.market.refresh_pins();
        assert!(r.subs.borrow().contains(&btc()));
    }

    #[tokio::test]
    async fn pump_applies_events_and_broadcasts_market_data() {
        let r = rig().await;
        let mut events = r.bus.subscribe();
        let (tx, rx) = mpsc::channel(8);
        tokio::spawn(pump(rx, r.broker.clone(), r.bus.clone()));
        tx.send(MarketEvent::Book(book(&r.clock))).await.unwrap();
        assert!(matches!(events.recv().await.unwrap(), BusEvent::Market(MarketEvent::Book(_))));
        let bid = OrderRequest {
            instrument: btc(),
            side: Side::Buy,
            kind: OrderType::Limit,
            size: Size::Qty(dec!(0.1)),
            limit_price: Some(dec!(99998000)),
            tif: Tif::Gtc,
            reason: "test".into(),
        };
        let (order, _) = r.broker.place_sync("a", bid).unwrap();
        let t = Trade { instrument: btc(), price: dec!(99990000), qty: dec!(1), at: r.clock.now() };
        tx.send(MarketEvent::Trade(t)).await.unwrap();
        assert!(matches!(events.recv().await.unwrap(), BusEvent::Market(MarketEvent::Trade(_))));
        assert_eq!(r.broker.order(order.id).unwrap().status, crate::broker::OrderStatus::Filled);
    }
}
