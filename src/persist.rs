//! Writes the broker's journal to Postgres in order, then republishes each event on the bus.

use std::sync::Arc;
use std::time::Duration;

use tokio::sync::{broadcast, mpsc};

use crate::broker::{Journal, Stamped};
use crate::market::BusEvent;
use crate::store::Store;

const ATTEMPTS: u32 = 10;

/// Each entry is written to the generation the broker stamped it with, so a reset while events
/// are queued cannot move them into the new generation.
pub async fn persist(mut rx: mpsc::UnboundedReceiver<Stamped>, store: Arc<Store>, bus: broadcast::Sender<BusEvent>) {
    while let Some(Stamped { generation, event: j }) = rx.recv().await {
        let mut delay = Duration::from_millis(200);
        for attempt in 1..=ATTEMPTS {
            match write(&store, generation, &j).await {
                Ok(()) => break,
                Err(e) if is_permanent(&e) => {
                    tracing::error!(error = %e, event = ?j, "journal write violates a constraint; event dropped");
                    break;
                }
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

/// Integrity violations (SQLSTATE class 23) will fail the same way on every retry.
fn is_permanent(e: &anyhow::Error) -> bool {
    e.downcast_ref::<sqlx::Error>()
        .and_then(|e| e.as_database_error())
        .and_then(|d| d.code())
        .is_some_and(|c| c.starts_with("23"))
}

async fn write(store: &Store, generation: i32, j: &Journal) -> anyhow::Result<()> {
    match j {
        Journal::Order(o) => store.save_order(o, generation).await?,
        Journal::Fill(f) => {
            store.save_fill(f, generation).await?;
        }
        Journal::Conversion { account, conversion, at } => {
            store.save_conversion(account, generation, conversion, *at).await?
        }
    }
    Ok(())
}
