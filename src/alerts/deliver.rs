//! Delivering fired alerts to the account's Attacca agent.

use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex};
use std::time::Duration as StdDuration;

use async_trait::async_trait;
use chrono::Utc;
use tokio::sync::{broadcast, mpsc};
use zyris::Connection;
use zyris_attacca::{AttaccaApi, AttaccaApiClient, ZNewSession};

use super::{Alert, Firing, Limiter, Watcher};
use crate::app::{App, value_account};
use crate::feed::MarketEvent;
use crate::market::BusEvent;

#[async_trait]
pub trait Notifier: Send + Sync {
    /// Send `text` to `agent_id`'s session for `account` (creating one when `session` is `None`
    /// or unusable). Returns the session id used.
    async fn send(&self, agent_id: &str, account: &str, session: Option<String>, text: &str) -> anyhow::Result<String>;
}

/// The live zyris connection, refreshed on every (re)connect.
#[derive(Clone, Default)]
pub struct ConnSlot(Arc<Mutex<Option<Connection>>>);

impl ConnSlot {
    pub fn put(&self, conn: Connection) {
        *self.0.lock().unwrap() = Some(conn);
    }

    pub fn get(&self) -> Option<Connection> {
        self.0.lock().unwrap().clone()
    }
}

pub struct AttaccaNotifier {
    slot: ConnSlot,
}

impl AttaccaNotifier {
    pub fn new(slot: ConnSlot) -> Self {
        AttaccaNotifier { slot }
    }
}

fn node_path(conn: &Connection) -> String {
    conn.info().node.as_ref().map(|n| n.path().to_string()).unwrap_or_else(|| conn.info().node_id.clone())
}

#[async_trait]
impl Notifier for AttaccaNotifier {
    async fn send(&self, agent_id: &str, account: &str, session: Option<String>, text: &str) -> anyhow::Result<String> {
        let conn = self.slot.get().ok_or_else(|| anyhow::anyhow!("not connected to Attacca"))?;
        let api = conn.wait_capability::<AttaccaApiClient>(StdDuration::from_secs(5)).await?;
        let path = node_path(&conn);
        let text = text.replace("{node}", &path);
        if let Some(id) = session {
            if api.send_message(id.clone(), text.clone(), Vec::new()).await.is_ok() {
                return Ok(id);
            }
        }
        let preamble = format!(
            "You manage the ATrader paper-trading account \"{account}\" through the zyris trader tools on node {path}. \
             Messages in this session are alerts you set with create_alert. Check get_quotes and get_account before acting, \
             and always state a reason when you place an order."
        );
        let s = api
            .create_session_with(ZNewSession { agent_id: agent_id.to_string(), title: Some(format!("ATrader alerts: {account}")), project_id: None, preamble: Some(preamble) })
            .await?;
        api.send_message(s.id.clone(), text, Vec::new()).await?;
        Ok(s.id)
    }
}

pub enum AlertCmd {
    Upsert(Alert),
    Remove(i64),
    /// The account was reset: its alerts belong to the closed generation.
    DropAccount(String),
}

/// The account line appended to every alert message.
async fn account_line(app: &App, account: &str) -> String {
    let (Some(pf), Ok(usd_krw)) = (app.broker.portfolio(account), app.fx.usd_krw().await) else { return String::new() };
    let v = value_account(&app.broker, &pf, usd_krw);
    let cash: Vec<String> = pf.cash.iter().map(|(c, b)| format!("{} {}", c.code(), b.round_dp(2))).collect();
    format!(" Account: equity ₩{}, cash {}.", v.equity_krw.round_dp(0), cash.join(", "))
}

async fn deliver(app: Arc<App>, notifier: Arc<dyn Notifier>, account: String, alert_id: i64, text: String) {
    let Some(agent) = app.agent_accounts().into_iter().find(|a| a.id == account).and_then(|a| a.agent_id) else {
        tracing::warn!(%account, "alert fired for an account without an agent");
        return;
    };
    let event = match app.store.record_alert_event(alert_id, &account, Utc::now(), &text, false, None).await {
        Ok(id) => id,
        Err(e) => {
            tracing::error!(error = %e, "could not record alert event");
            return;
        }
    };
    for attempt in 0..2 {
        let session = app.store.alert_session(&account).await.ok().flatten();
        match notifier.send(&agent, &account, session.clone(), &text).await {
            Ok(used) => {
                if session.as_deref() != Some(used.as_str()) {
                    let _ = app.store.set_alert_session(&account, &used).await;
                }
                let _ = app.store.set_event_delivered(event, true, None).await;
                return;
            }
            Err(e) => {
                let _ = app.store.set_event_delivered(event, false, Some(&format!("{e:#}"))).await;
                tracing::warn!(error = %e, %account, attempt, "alert delivery failed");
                if attempt == 0 {
                    tokio::time::sleep(StdDuration::from_secs(30)).await;
                }
            }
        }
    }
}

/// Evaluate alerts against the bus and deliver what fires.
pub async fn alert_loop(
    app: Arc<App>,
    mut bus: broadcast::Receiver<BusEvent>,
    mut cmds: mpsc::UnboundedReceiver<AlertCmd>,
    notifier: Arc<dyn Notifier>,
) {
    let mut watcher = Watcher::default();
    match app.store.all_active_alerts(&app.broker.generations()).await {
        Ok(alerts) => alerts.into_iter().for_each(|a| watcher.upsert(a)),
        Err(e) => tracing::error!(error = %e, "could not load alerts"),
    }
    app.market.set_extra_pins(watcher.instruments());
    let mut limiter = Limiter::default();
    let mut digest: HashMap<String, VecDeque<(i64, String)>> = HashMap::new();
    let mut locks: HashMap<String, Arc<tokio::sync::Mutex<()>>> = HashMap::new();
    let mut tick = tokio::time::interval(StdDuration::from_secs(30));
    loop {
        let firings: Vec<Firing> = tokio::select! {
            cmd = cmds.recv() => {
                match cmd {
                    Some(AlertCmd::Upsert(a)) => watcher.upsert(a),
                    Some(AlertCmd::Remove(id)) => watcher.remove(id),
                    Some(AlertCmd::DropAccount(account)) => {
                        let ids: Vec<i64> = watcher.alerts().iter().filter(|a| a.account == account).map(|a| a.id).collect();
                        ids.into_iter().for_each(|id| watcher.remove(id));
                    }
                    None => return,
                }
                app.market.set_extra_pins(watcher.instruments());
                continue;
            }
            ev = bus.recv() => match ev {
                Ok(BusEvent::Market(MarketEvent::Trade(t))) => {
                    let adv = app.broker.stats(&t.instrument).map(|s| s.adv_notional);
                    watcher.on_trade(&t, adv, Utc::now())
                }
                Ok(BusEvent::Fill(f)) => watcher.on_fill(&f, Utc::now()),
                Ok(_) => continue,
                Err(broadcast::error::RecvError::Lagged(n)) => {
                    tracing::warn!(skipped = n, "alert watcher fell behind the bus");
                    continue;
                }
                Err(broadcast::error::RecvError::Closed) => return,
            },
            _ = tick.tick() => {
                let mut out = watcher.on_tick(&app.broker.calendar(), Utc::now());
                // Flush digests that now have room.
                let now = Utc::now();
                for (account, queued) in digest.iter_mut() {
                    if !queued.is_empty() && limiter.admit(account, now) {
                        let items: Vec<String> = queued.drain(..).map(|(id, t)| format!("#{id}: {t}")).collect();
                        let first = items.len();
                        out.push(Firing { alert_id: -1, account: account.clone(), text: format!("{first} alerts fired while messages were rate-limited: {}", items.join(" | ")), deactivate: false, note: String::new() });
                    }
                }
                out
            }
        };
        if firings.is_empty() {
            continue;
        }
        if firings.iter().any(|f| f.deactivate) {
            app.market.set_extra_pins(watcher.instruments());
        }
        for f in firings {
            let is_digest = f.alert_id < 0;
            if !is_digest && !limiter.admit(&f.account, Utc::now()) {
                digest.entry(f.account.clone()).or_default().push_back((f.alert_id, f.text));
                continue;
            }
            // Everything that waits (DB, FX, Attacca) happens off the evaluation loop.
            let lock = locks.entry(f.account.clone()).or_default().clone();
            tokio::spawn(handle(app.clone(), notifier.clone(), lock, f));
        }
    }
}

/// Persist a firing, compose its message and deliver it. Per-account `lock` serializes delivery
/// so simultaneous firings share one Attacca session.
async fn handle(app: Arc<App>, notifier: Arc<dyn Notifier>, lock: Arc<tokio::sync::Mutex<()>>, f: Firing) {
    if f.alert_id > 0 {
        // A lost deactivation would re-arm a one-shot alert after a restart: retry.
        for attempt in 1..=5u32 {
            match app.store.mark_fired(f.alert_id, Utc::now(), f.deactivate).await {
                Ok(()) => break,
                Err(e) => {
                    tracing::error!(error = %e, alert = f.alert_id, attempt, "could not mark alert fired");
                    tokio::time::sleep(StdDuration::from_secs(u64::from(attempt))).await;
                }
            }
        }
    }
    let _serial = lock.lock().await;
    let note = if f.note.is_empty() { String::new() } else { format!(" Your note: \"{}\".", f.note) };
    let head = if f.alert_id < 0 { "ATrader alert digest".to_string() } else { format!("ATrader alert #{}", f.alert_id) };
    let text = format!(
        "{head} on account {}: {}.{note}{} Use the trader tools on node {{node}} to act; give a reason for any order.",
        f.account,
        f.text,
        account_line(&app, &f.account).await
    );
    if f.alert_id > 0 {
        deliver(app, notifier, f.account, f.alert_id, text).await;
    } else {
        deliver_untracked(app, notifier, f.account, text).await;
    }
}

/// Digests have no single alert row to hang an event on; they are logged, not stored.
async fn deliver_untracked(app: Arc<App>, notifier: Arc<dyn Notifier>, account: String, text: String) {
    let Some(agent) = app.agent_accounts().into_iter().find(|a| a.id == account).and_then(|a| a.agent_id) else { return };
    let session = app.store.alert_session(&account).await.ok().flatten();
    if let Err(e) = notifier.send(&agent, &account, session, &text).await {
        tracing::warn!(error = %e, %account, "alert digest delivery failed");
    }
}
