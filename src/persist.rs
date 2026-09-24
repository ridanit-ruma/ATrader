//! Writes the broker's journal to Postgres in order, then republishes each event on the bus.

use std::sync::Arc;
use std::time::Duration;

use tokio::sync::{broadcast, mpsc};

use crate::broker::Journal;
use crate::market::BusEvent;
use crate::store::Store;

const ATTEMPTS: u32 = 10;

pub async fn persist(mut rx: mpsc::UnboundedReceiver<Journal>, store: Arc<Store>, bus: broadcast::Sender<BusEvent>) {
    while let Some(j) = rx.recv().await {
        let mut delay = Duration::from_millis(200);
        for attempt in 1..=ATTEMPTS {
            match write(&store, &j).await {
                Ok(()) => break,
                // ponytail: after ATTEMPTS the event is logged and skipped rather than blocking every later write.
                Err(e) if attempt == ATTEMPTS => tracing::error!(error = %e, event = ?j, "journal write failed; event dropped"),
                Err(e) => {
                    tracing::warn!(error = %e, attempt, "journal write failed; retrying");
                    tokio::time::sleep(delay).await;
                    delay = (delay * 2).min(Duration::from_secs(30));
                }
            }
        }
        let event = match j {
            Journal::Order(o) => BusEvent::Order(o),
            Journal::Fill(f) => BusEvent::Fill(f),
            Journal::Conversion { .. } => continue,
        };
        let _ = bus.send(event);
    }
}

async fn write(store: &Store, j: &Journal) -> anyhow::Result<()> {
    match j {
        Journal::Order(o) => store.save_order(o, store.generation(&o.account).await?).await?,
        Journal::Fill(f) => {
            store.save_fill(f, store.generation(&f.account).await?).await?;
        }
        Journal::Conversion { account, conversion, at } => {
            store.save_conversion(account, store.generation(account).await?, conversion, *at).await?
        }
    }
    Ok(())
}
