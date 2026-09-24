//! Writes the broker's journal to Postgres in order, then republishes each event on the bus.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use tokio::sync::{broadcast, mpsc};

use crate::broker::Journal;
use crate::market::BusEvent;
use crate::store::Store;

const ATTEMPTS: u32 = 10;

/// `generations` is what each account was restored at: the in-memory state belongs to that
/// generation even if the account is reset in the database meanwhile. Accounts created later are
/// looked up once and cached.
pub async fn persist(
    mut rx: mpsc::UnboundedReceiver<Journal>,
    store: Arc<Store>,
    bus: broadcast::Sender<BusEvent>,
    mut generations: HashMap<String, i32>,
) {
    while let Some(j) = rx.recv().await {
        let mut delay = Duration::from_millis(200);
        for attempt in 1..=ATTEMPTS {
            match write(&store, &mut generations, &j).await {
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

async fn generation(store: &Store, cache: &mut HashMap<String, i32>, account: &str) -> anyhow::Result<i32> {
    if let Some(g) = cache.get(account) {
        return Ok(*g);
    }
    let g = store.generation(account).await?;
    cache.insert(account.to_string(), g);
    Ok(g)
}

async fn write(store: &Store, cache: &mut HashMap<String, i32>, j: &Journal) -> anyhow::Result<()> {
    match j {
        Journal::Order(o) => store.save_order(o, generation(store, cache, &o.account).await?).await?,
        Journal::Fill(f) => {
            store.save_fill(f, generation(store, cache, &f.account).await?).await?;
        }
        Journal::Conversion { account, conversion, at } => {
            store.save_conversion(account, generation(store, cache, account).await?, conversion, *at).await?
        }
    }
    Ok(())
}
