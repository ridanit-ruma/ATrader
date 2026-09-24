# ATrader Phase 6 (Alerts and Push to Attacca) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Let the agent set alerts: price levels, % moves over a window, volume surges, order fills, and session open/close. When an alert fires, ATrader opens (or reuses) an Attacca session for the account's agent and sends a message, so the agent wakes up and can act without polling.

**Architecture:**
- **Watcher.** A pure `Watcher` holds active alerts plus a short price and volume history per instrument. It turns trades, fills and clock ticks into `Firing`s, applying a per-alert cooldown and one-shot deactivation.
- **`alert_loop`.** It subscribes to the bus, feeds the watcher, persists `alert_events`, rate-limits per account (bursts collapse into a digest), and delivers through a `Notifier`. It takes create/delete commands from the tools over a channel.
- **Delivery.** `AttaccaNotifier` uses the zyris connection captured by `NodeBuilder::on_connect`. It calls `attacca_api.create_session_with` (with a preamble naming this node's `node_path` and the account) once per account, remembering the session id on the account row, then `send_message`.
- **Subscriptions.** Instruments that alerts reference are pinned into the market subscriptions (spec §4 priority 2).

**Tech Stack:** `zyris-attacca` (same git rev as `zyris`). Everything else exists.

**Spec:** `docs/superpowers/specs/2026-09-24-atrader-design.md` §8 and the §7 alert tools. This covers §15 step 6.

## Global Constraints

- Earlier phases' constraints still apply.
- Alert kinds:

  | Kind | Needs | Fires when |
  | --- | --- | --- |
  | `price_above`, `price_below` | `instrument`, `threshold` (price) | a real trade prints at or through the level |
  | `move` | `instrument`, `threshold` (%), `window_minutes` 1–240 | \|last / price `window` minutes ago − 1\| ≥ threshold % |
  | `volume_surge` | `instrument`, `threshold` (factor ≥ 1), `window_minutes` 1–240 | traded value in the window ≥ factor × ADV × window / 1440 |
  | `order_filled` | optional `instrument` | any fill of the account (or of that instrument) |
  | `session_open`, `session_close` | `venue` (`KRX`/`US`) | the venue opens or closes |

- `once` defaults to true. Each alert fires at most once per 5 minutes, and a one-shot alert deactivates when it fires.
- An account may have at most 50 active alerts.
- Delivery limit: 20 messages per account per hour. Anything over that is queued into a digest, which is sent on the next minute tick that has room.
- A failed delivery is recorded (`delivered = false`, `error`) and retried once after 30 s.
- Message: `ATrader alert #<id> on account <account>: <what happened>. Your note: "<note>". Account: equity ₩<n>, cash … . Use the trader tools on node <node_path> to act; give a reason for any order.`
- Session: one per account. Its id is stored in `accounts.alert_session_id` and a new one is created if sending to the stored session fails.
  - Preamble: `You manage the ATrader paper-trading account "<account>" through the zyris trader tools on node <node_path>. Messages in this session are alerts you set with create_alert. Check get_quotes and get_account before acting, and always state a reason when you place an order.`
- Scopes the credential needs: `agents:read`, `sessions:write`, `sessions:read`. They are documented in the README.

## Review Focus

1. **Flapping must not flood the agent.** A price oscillating across a level, or 100 fills in a minute, must stay within the cooldown and the per-account hourly limit, with excess arriving as a single digest and nothing silently dropped. Tasks 2 and 4 test this.
2. **A one-shot alert must fire exactly once.** It must not fire again on the next print, nor after a restart. Tasks 2 and 4 test this.
3. **Alert rows must not corrupt the agent's generation.** Alerts from an old generation must not fire after a reset, and deleting another account's alert must be refused. Tasks 3 and 5 test this.
4. **Delivery must survive Attacca being unreachable.** A missing connection or a failing API must record an undelivered event and retry once, without blocking evaluation of other alerts. Task 4 tests this.
5. **Bad alert definitions must be rejected.** A missing threshold, a zero or negative window, an unknown venue, or an alert on an unknown instrument must return a clear error before anything is stored. Task 5 tests this.

## File Structure

| File | Responsibility |
| --- | --- |
| `migrations/0003_alerts.sql` | `alerts`, `alert_events`, `accounts.alert_session_id` |
| `src/alerts/mod.rs` | `Condition`, `Alert`, `Firing`, `Watcher`, `Limiter` |
| `src/alerts/deliver.rs` | `Notifier` trait, `AttaccaNotifier`, `ConnSlot`, `alert_loop`, `AlertCmd` |
| `src/store.rs` | Modified: alert CRUD, events, session id |
| `src/market.rs` | Modified: extra (alert) pins |
| `src/broker.rs` | Modified: `stats(&id)` accessor |
| `src/app.rs` | Modified: `alerts: Option<mpsc::UnboundedSender<AlertCmd>>` |
| `src/tools/{mod,dto}.rs` | Modified: `create_alert`, `list_alerts`, `delete_alert` |
| `src/cli.rs` | Modified: `on_connect` slot, spawn `alert_loop` |

---

### Task 1: Store: alerts, events, session id

**Files:**
- Create: `migrations/0003_alerts.sql`
- Create: `src/alerts/mod.rs` (only the data types for now)
- Modify: `src/store.rs`, `src/lib.rs`, `tests/store.rs`

**Interfaces:**
- Produces:
  - `Condition`, an enum:
    - `PriceAbove { id: InstrumentId, price: Decimal }`
    - `PriceBelow { .. }`
    - `Move { id, pct: Decimal, window_minutes: u32 }`
    - `VolumeSurge { id, factor: Decimal, window_minutes: u32 }`
    - `OrderFilled { id: Option<InstrumentId> }`
    - `SessionOpen { venue: Venue }`
    - `SessionClose { venue: Venue }`
  - `Alert { id: i64, account: String, generation: i32, condition: Condition, note: String, once: bool, created_at, last_fired_at: Option<DateTime<Utc>> }`.
  - These `Store` methods:
    - `create_alert(&Alert) -> sqlx::Result<i64>`, which ignores `alert.id`
    - `active_alerts(account, generation) -> Vec<Alert>`
    - `all_active_alerts(&HashMap<String, i32>) -> Vec<Alert>`
    - `deactivate_alert(account, id) -> sqlx::Result<bool>`, which is `false` if the alert is not that account's or is already inactive
    - `mark_fired(id, at, deactivate: bool)`
    - `record_alert_event(alert_id, account, at, message, delivered, error: Option<&str>) -> i64`
    - `set_event_delivered(event_id, delivered, error)`
    - `alert_session(account) -> Option<String>`
    - `set_alert_session(account, session)`

- [ ] **Step 1: Write the failing test** (append to `tests/store.rs`)

```rust
#[sqlx::test]
async fn alerts_round_trip_per_account_and_generation(pool: PgPool) {
    use atrader::alerts::{Alert, Condition};
    let store = Store::new(pool);
    store.create_account("a", "A", Some("ag"), &[(Currency::Krw, dec!(1))], Utc::now()).await.unwrap();
    store.create_account("b", "B", Some("ag"), &[(Currency::Krw, dec!(1))], Utc::now()).await.unwrap();
    let btc: InstrumentId = "UPBIT:KRW-BTC".parse().unwrap();
    let alert = |account: &str, condition| Alert {
        id: 0,
        account: account.into(),
        generation: 1,
        condition,
        note: "watch".into(),
        once: true,
        created_at: Utc::now(),
        last_fired_at: None,
    };
    let id = store.create_alert(&alert("a", Condition::PriceAbove { id: btc.clone(), price: dec!(100) })).await.unwrap();
    store.create_alert(&alert("a", Condition::Move { id: btc.clone(), pct: dec!(3), window_minutes: 30 })).await.unwrap();
    store.create_alert(&alert("a", Condition::SessionOpen { venue: Venue::Krx })).await.unwrap();
    store.create_alert(&alert("b", Condition::OrderFilled { id: None })).await.unwrap();
    let a = store.active_alerts("a", 1).await.unwrap();
    assert_eq!(a.len(), 3);
    assert_eq!(a[0].condition, Condition::PriceAbove { id: btc.clone(), price: dec!(100) });
    assert_eq!(a[1].condition, Condition::Move { id: btc.clone(), pct: dec!(3), window_minutes: 30 });
    assert_eq!(a[2].condition, Condition::SessionOpen { venue: Venue::Krx });

    assert!(!store.deactivate_alert("b", id).await.unwrap()); // not b's
    assert!(store.deactivate_alert("a", id).await.unwrap());
    assert!(!store.deactivate_alert("a", id).await.unwrap()); // already off
    assert_eq!(store.active_alerts("a", 1).await.unwrap().len(), 2);

    store.reset_account("a", &[(Currency::Krw, dec!(1))], Utc::now()).await.unwrap();
    let gens = std::collections::HashMap::from([("a".to_string(), 2), ("b".to_string(), 1)]);
    assert_eq!(store.all_active_alerts(&gens).await.unwrap().len(), 1); // a's gen-1 alerts are gone

    let ev = store.record_alert_event(id, "a", Utc::now(), "fired", false, Some("offline")).await.unwrap();
    store.set_event_delivered(ev, true, None).await.unwrap();
    assert_eq!(store.alert_session("a").await.unwrap(), None);
    store.set_alert_session("a", "sess-1").await.unwrap();
    assert_eq!(store.alert_session("a").await.unwrap().as_deref(), Some("sess-1"));
}
```

- [ ] **Step 2: Run the test to verify it fails**

Run: `cargo test --test store alerts_round_trip`
Expected: compile errors.

- [ ] **Step 3: Implement**

Create `migrations/0003_alerts.sql`:

```sql
ALTER TABLE accounts ADD COLUMN alert_session_id TEXT;

CREATE TABLE alerts (
    id             BIGSERIAL PRIMARY KEY,
    account_id     TEXT NOT NULL REFERENCES accounts(id),
    generation     INT  NOT NULL,
    kind           TEXT NOT NULL,
    instrument     TEXT,
    venue          TEXT,
    threshold      NUMERIC,
    window_minutes INT,
    note           TEXT NOT NULL,
    once           BOOLEAN NOT NULL,
    active         BOOLEAN NOT NULL DEFAULT true,
    created_at     TIMESTAMPTZ NOT NULL,
    last_fired_at  TIMESTAMPTZ
);
CREATE INDEX alerts_active ON alerts (account_id, generation) WHERE active;

CREATE TABLE alert_events (
    id         BIGSERIAL PRIMARY KEY,
    alert_id   BIGINT NOT NULL REFERENCES alerts(id),
    account_id TEXT NOT NULL,
    fired_at   TIMESTAMPTZ NOT NULL,
    message    TEXT NOT NULL,
    delivered  BOOLEAN NOT NULL,
    error      TEXT
);
```

Create `src/alerts/mod.rs` (add `pub mod alerts;` to `lib.rs`):

```rust
//! Agent-defined alerts: conditions, a pure evaluator, and delivery to Attacca.

use chrono::{DateTime, Utc};
use rust_decimal::Decimal;

use crate::domain::{InstrumentId, Venue};

#[derive(Debug, Clone, PartialEq)]
pub enum Condition {
    PriceAbove { id: InstrumentId, price: Decimal },
    PriceBelow { id: InstrumentId, price: Decimal },
    /// Absolute % change over the window.
    Move { id: InstrumentId, pct: Decimal, window_minutes: u32 },
    /// Traded value in the window versus the average-day pace.
    VolumeSurge { id: InstrumentId, factor: Decimal, window_minutes: u32 },
    OrderFilled { id: Option<InstrumentId> },
    SessionOpen { venue: Venue },
    SessionClose { venue: Venue },
}

impl Condition {
    pub fn kind(&self) -> &'static str {
        match self {
            Condition::PriceAbove { .. } => "price_above",
            Condition::PriceBelow { .. } => "price_below",
            Condition::Move { .. } => "move",
            Condition::VolumeSurge { .. } => "volume_surge",
            Condition::OrderFilled { .. } => "order_filled",
            Condition::SessionOpen { .. } => "session_open",
            Condition::SessionClose { .. } => "session_close",
        }
    }

    pub fn instrument(&self) -> Option<&InstrumentId> {
        match self {
            Condition::PriceAbove { id, .. } | Condition::PriceBelow { id, .. } | Condition::Move { id, .. } | Condition::VolumeSurge { id, .. } => Some(id),
            Condition::OrderFilled { id } => id.as_ref(),
            Condition::SessionOpen { .. } | Condition::SessionClose { .. } => None,
        }
    }

    pub fn venue(&self) -> Option<Venue> {
        match self {
            Condition::SessionOpen { venue } | Condition::SessionClose { venue } => Some(*venue),
            _ => None,
        }
    }

    pub fn threshold(&self) -> Option<Decimal> {
        match self {
            Condition::PriceAbove { price, .. } | Condition::PriceBelow { price, .. } => Some(*price),
            Condition::Move { pct, .. } => Some(*pct),
            Condition::VolumeSurge { factor, .. } => Some(*factor),
            _ => None,
        }
    }

    pub fn window_minutes(&self) -> Option<u32> {
        match self {
            Condition::Move { window_minutes, .. } | Condition::VolumeSurge { window_minutes, .. } => Some(*window_minutes),
            _ => None,
        }
    }

    /// Rebuild from stored columns.
    pub fn from_parts(kind: &str, instrument: Option<InstrumentId>, venue: Option<Venue>, threshold: Option<Decimal>, window: Option<u32>) -> Option<Condition> {
        Some(match kind {
            "price_above" => Condition::PriceAbove { id: instrument?, price: threshold? },
            "price_below" => Condition::PriceBelow { id: instrument?, price: threshold? },
            "move" => Condition::Move { id: instrument?, pct: threshold?, window_minutes: window? },
            "volume_surge" => Condition::VolumeSurge { id: instrument?, factor: threshold?, window_minutes: window? },
            "order_filled" => Condition::OrderFilled { id: instrument },
            "session_open" => Condition::SessionOpen { venue: venue? },
            "session_close" => Condition::SessionClose { venue: venue? },
            _ => return None,
        })
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Alert {
    pub id: i64,
    pub account: String,
    pub generation: i32,
    pub condition: Condition,
    pub note: String,
    pub once: bool,
    pub created_at: DateTime<Utc>,
    pub last_fired_at: Option<DateTime<Utc>>,
}
```

Add these to `src/store.rs`, with imports `use crate::alerts::{Alert, Condition};` and `use crate::domain::Venue;`:

```rust
fn parse_alert(r: &sqlx::postgres::PgRow) -> sqlx::Result<Alert> {
    let kind: String = r.get("kind");
    let instrument = r.get::<Option<String>, _>("instrument").map(|s| s.parse::<InstrumentId>()).transpose().map_err(decode_err)?;
    let venue = r.get::<Option<String>, _>("venue").and_then(|v| Venue::from_tag(&v));
    let window = r.get::<Option<i32>, _>("window_minutes").map(|w| w as u32);
    let condition = Condition::from_parts(&kind, instrument, venue, r.get("threshold"), window).ok_or_else(|| decode_err(format!("incomplete {kind} alert")))?;
    Ok(Alert {
        id: r.get("id"),
        account: r.get("account_id"),
        generation: r.get("generation"),
        condition,
        note: r.get("note"),
        once: r.get("once"),
        created_at: r.get("created_at"),
        last_fired_at: r.get("last_fired_at"),
    })
}
```

```rust
    pub async fn create_alert(&self, a: &Alert) -> sqlx::Result<i64> {
        let c = &a.condition;
        sqlx::query_scalar(
            "INSERT INTO alerts (account_id, generation, kind, instrument, venue, threshold, window_minutes, note, once, created_at)
             VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10) RETURNING id",
        )
        .bind(&a.account)
        .bind(a.generation)
        .bind(c.kind())
        .bind(c.instrument().map(|i| i.to_string()))
        .bind(c.venue().map(|v| v.tag()))
        .bind(c.threshold())
        .bind(c.window_minutes().map(|w| w as i32))
        .bind(&a.note)
        .bind(a.once)
        .bind(a.created_at)
        .fetch_one(&self.pool)
        .await
    }

    pub async fn active_alerts(&self, account: &str, generation: i32) -> sqlx::Result<Vec<Alert>> {
        let rows = sqlx::query("SELECT * FROM alerts WHERE account_id = $1 AND generation = $2 AND active ORDER BY id")
            .bind(account)
            .bind(generation)
            .fetch_all(&self.pool)
            .await?;
        rows.iter().map(parse_alert).collect()
    }

    /// Active alerts of every account at the generation it was restored at.
    pub async fn all_active_alerts(&self, generations: &HashMap<String, i32>) -> sqlx::Result<Vec<Alert>> {
        let mut out = Vec::new();
        for (account, generation) in generations {
            out.extend(self.active_alerts(account, *generation).await?);
        }
        Ok(out)
    }

    /// Turn off `account`'s alert `id`. False when it is not that account's or already off.
    pub async fn deactivate_alert(&self, account: &str, id: i64) -> sqlx::Result<bool> {
        let done = sqlx::query("UPDATE alerts SET active = false WHERE id = $1 AND account_id = $2 AND active")
            .bind(id)
            .bind(account)
            .execute(&self.pool)
            .await?;
        Ok(done.rows_affected() == 1)
    }

    pub async fn mark_fired(&self, id: i64, at: DateTime<Utc>, deactivate: bool) -> sqlx::Result<()> {
        sqlx::query("UPDATE alerts SET last_fired_at = $2, active = active AND NOT $3 WHERE id = $1")
            .bind(id)
            .bind(at)
            .bind(deactivate)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    pub async fn record_alert_event(&self, alert_id: i64, account: &str, at: DateTime<Utc>, message: &str, delivered: bool, error: Option<&str>) -> sqlx::Result<i64> {
        sqlx::query_scalar(
            "INSERT INTO alert_events (alert_id, account_id, fired_at, message, delivered, error) VALUES ($1,$2,$3,$4,$5,$6) RETURNING id",
        )
        .bind(alert_id)
        .bind(account)
        .bind(at)
        .bind(message)
        .bind(delivered)
        .bind(error)
        .fetch_one(&self.pool)
        .await
    }

    pub async fn set_event_delivered(&self, event_id: i64, delivered: bool, error: Option<&str>) -> sqlx::Result<()> {
        sqlx::query("UPDATE alert_events SET delivered = $2, error = $3 WHERE id = $1")
            .bind(event_id)
            .bind(delivered)
            .bind(error)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    pub async fn alert_session(&self, account: &str) -> sqlx::Result<Option<String>> {
        sqlx::query_scalar("SELECT alert_session_id FROM accounts WHERE id = $1").bind(account).fetch_one(&self.pool).await
    }

    pub async fn set_alert_session(&self, account: &str, session: &str) -> sqlx::Result<()> {
        sqlx::query("UPDATE accounts SET alert_session_id = $2 WHERE id = $1").bind(account).bind(session).execute(&self.pool).await?;
        Ok(())
    }
```

- [ ] **Step 4: Run the test to verify it passes**

Run: `cargo test --test store` (with `DATABASE_URL`)
Expected: all pass.

- [ ] **Step 5: Commit**

```bash
git add migrations/0003_alerts.sql src/alerts src/lib.rs src/store.rs tests/store.rs
git commit -m "Store alerts, alert events and alert sessions"
git log -1 --format=%B
```

---

### Task 2: The watcher and the rate limiter

**Files:**
- Modify: `src/alerts/mod.rs`

**Interfaces:**
- Produces:
  - `Firing { alert_id: i64, account: String, text: String, deactivate: bool }`.
  - `Watcher` (Default) with these methods:
    - `upsert(Alert)` and `remove(id)`
    - `alerts() -> &[Alert]`
    - `instruments() -> Vec<InstrumentId>`
    - `on_trade(&Trade, adv: Option<Decimal>, now) -> Vec<Firing>`
    - `on_fill(&Fill, now) -> Vec<Firing>`
    - `on_tick(&Calendar, now) -> Vec<Firing>`
  - `COOLDOWN` (5 min).
  - `Limiter` (Default) with `admit(account, now) -> bool` (20 per rolling hour).

- [ ] **Step 1: Write the failing tests** (append a tests module to `src/alerts/mod.rs`)

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::broker::{Fill, Liquidity};
    use crate::domain::{Side, Trade};
    use crate::venue::Calendar;
    use chrono::{Duration, TimeZone};
    use rust_decimal_macros::dec;

    fn t0() -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 9, 23, 1, 0, 0).unwrap()
    }

    fn btc() -> InstrumentId {
        "UPBIT:KRW-BTC".parse().unwrap()
    }

    fn alert(id: i64, condition: Condition, once: bool) -> Alert {
        Alert { id, account: "a".into(), generation: 1, condition, note: "n".into(), once, created_at: t0(), last_fired_at: None }
    }

    fn trade(price: Decimal, qty: Decimal, at: DateTime<Utc>) -> Trade {
        Trade { instrument: btc(), price, qty, at }
    }

    #[test]
    fn price_levels_fire_once_or_with_cooldown() {
        let mut w = Watcher::default();
        w.upsert(alert(1, Condition::PriceAbove { id: btc(), price: dec!(100) }, true));
        w.upsert(alert(2, Condition::PriceBelow { id: btc(), price: dec!(90) }, false));
        assert!(w.on_trade(&trade(dec!(99), dec!(1), t0()), None, t0()).is_empty());
        let f = w.on_trade(&trade(dec!(100), dec!(1), t0()), None, t0());
        assert_eq!((f.len(), f[0].alert_id, f[0].deactivate), (1, 1, true));
        assert!(f[0].text.contains("100"));
        assert!(w.on_trade(&trade(dec!(101), dec!(1), t0()), None, t0()).is_empty()); // one-shot removed
        assert_eq!(w.alerts().len(), 1);

        let at = t0() + Duration::seconds(10);
        assert_eq!(w.on_trade(&trade(dec!(89), dec!(1), at), None, at).len(), 1);
        let soon = at + Duration::minutes(2);
        assert!(w.on_trade(&trade(dec!(88), dec!(1), soon), None, soon).is_empty()); // cooldown
        let later = at + COOLDOWN;
        assert_eq!(w.on_trade(&trade(dec!(88), dec!(1), later), None, later).len(), 1);
    }

    #[test]
    fn moves_are_measured_over_the_window() {
        let mut w = Watcher::default();
        w.upsert(alert(1, Condition::Move { id: btc(), pct: dec!(3), window_minutes: 10 }, true));
        w.on_trade(&trade(dec!(100), dec!(1), t0()), None, t0());
        let t5 = t0() + Duration::minutes(5);
        assert!(w.on_trade(&trade(dec!(102), dec!(1), t5), None, t5).is_empty());
        let t9 = t0() + Duration::minutes(9);
        let f = w.on_trade(&trade(dec!(96.9), dec!(1), t9), None, t9);
        assert_eq!(f.len(), 1);
        assert!(f[0].text.contains("-3.1"), "{}", f[0].text);
    }

    #[test]
    fn a_move_needs_history_from_the_start_of_the_window() {
        let mut w = Watcher::default();
        w.upsert(alert(1, Condition::Move { id: btc(), pct: dec!(1), window_minutes: 10 }, true));
        let t20 = t0() + Duration::minutes(20);
        w.on_trade(&trade(dec!(100), dec!(1), t0()), None, t0());
        // The only reference is 20 minutes old: compare against the oldest print inside the window instead.
        assert!(w.on_trade(&trade(dec!(150), dec!(1), t20), None, t20).is_empty());
        let t21 = t20 + Duration::minutes(1);
        assert_eq!(w.on_trade(&trade(dec!(152), dec!(1), t21), None, t21).len(), 1);
    }

    #[test]
    fn volume_surges_compare_with_the_daily_pace() {
        let mut w = Watcher::default();
        w.upsert(alert(1, Condition::VolumeSurge { id: btc(), factor: dec!(3), window_minutes: 60 }, true));
        // ADV 2,400 per day = 100 per hour; 3x = 300 in the hour.
        let adv = Some(dec!(2400));
        assert!(w.on_trade(&trade(dec!(100), dec!(2), t0()), adv, t0()).is_empty()); // 200
        assert!(w.on_trade(&trade(dec!(100), dec!(1), t0()), None, t0()).is_empty()); // no ADV known: never fires
        assert_eq!(w.on_trade(&trade(dec!(100), dec!(0.5), t0()), adv, t0()).len(), 1); // 350
    }

    #[test]
    fn fills_match_account_and_instrument() {
        let mut w = Watcher::default();
        w.upsert(alert(1, Condition::OrderFilled { id: None }, false));
        w.upsert(alert(2, Condition::OrderFilled { id: Some("UPBIT:KRW-ETH".parse().unwrap()) }, false));
        let fill = |account: &str| Fill {
            order_id: 7,
            account: account.into(),
            instrument: btc(),
            side: Side::Buy,
            qty: dec!(0.1),
            notional: dec!(10000000),
            price: dec!(100000000),
            fee: dec!(5000),
            tax: dec!(0),
            realized_pnl: None,
            liquidity: Liquidity::Maker,
            at: t0(),
        };
        let f = w.on_fill(&fill("a"), t0());
        assert_eq!(f.iter().map(|x| x.alert_id).collect::<Vec<_>>(), vec![1]);
        assert!(f[0].text.contains("order 7"));
        assert!(w.on_fill(&fill("b"), t0()).is_empty());
    }

    #[test]
    fn sessions_fire_on_transitions_only() {
        let mut w = Watcher::default();
        w.upsert(alert(1, Condition::SessionOpen { venue: Venue::Krx }, false));
        w.upsert(alert(2, Condition::SessionClose { venue: Venue::Krx }, false));
        let cal = Calendar::default();
        let before = Utc.with_ymd_and_hms(2026, 9, 22, 23, 59, 0).unwrap(); // 08:59 KST
        assert!(w.on_tick(&cal, before).is_empty()); // first tick only learns the state
        let open = Utc.with_ymd_and_hms(2026, 9, 23, 0, 0, 30).unwrap();
        assert_eq!(w.on_tick(&cal, open).iter().map(|f| f.alert_id).collect::<Vec<_>>(), vec![1]);
        assert!(w.on_tick(&cal, open + Duration::minutes(1)).is_empty());
        let close = Utc.with_ymd_and_hms(2026, 9, 23, 6, 30, 30).unwrap();
        assert_eq!(w.on_tick(&cal, close).iter().map(|f| f.alert_id).collect::<Vec<_>>(), vec![2]);
    }

    #[test]
    fn limiter_allows_twenty_an_hour_per_account() {
        let mut l = Limiter::default();
        for i in 0..20 {
            assert!(l.admit("a", t0() + Duration::seconds(i)));
        }
        assert!(!l.admit("a", t0() + Duration::minutes(30)));
        assert!(l.admit("b", t0() + Duration::minutes(30)));
        assert!(l.admit("a", t0() + Duration::minutes(61)));
    }

    #[test]
    fn instruments_lists_what_alerts_watch() {
        let mut w = Watcher::default();
        w.upsert(alert(1, Condition::PriceAbove { id: btc(), price: dec!(1) }, true));
        w.upsert(alert(2, Condition::SessionOpen { venue: Venue::Us }, true));
        assert_eq!(w.instruments(), vec![btc()]);
        w.remove(1);
        assert!(w.instruments().is_empty());
    }
}
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test --lib alerts::`
Expected: compile errors.

- [ ] **Step 3: Implement** (append to the non-test part of `src/alerts/mod.rs`; add imports for `std::collections::{HashMap, VecDeque}`, `chrono::Duration`, `crate::broker::Fill`, `crate::domain::Trade` and `crate::venue::Calendar`)

```rust
pub const COOLDOWN: Duration = Duration::minutes(5);
const MAX_WINDOW: Duration = Duration::minutes(240);

/// An alert that fired, ready to deliver.
#[derive(Debug, Clone, PartialEq)]
pub struct Firing {
    pub alert_id: i64,
    pub account: String,
    /// What happened, without the account summary delivery adds.
    pub text: String,
    /// One-shot alerts turn off when they fire.
    pub deactivate: bool,
}

#[derive(Debug, Default)]
pub struct Watcher {
    alerts: Vec<Alert>,
    /// Recent prints per watched instrument: (time, price, value).
    history: HashMap<InstrumentId, VecDeque<(DateTime<Utc>, Decimal, Decimal)>>,
    open: HashMap<Venue, bool>,
}

impl Watcher {
    pub fn upsert(&mut self, alert: Alert) {
        self.remove(alert.id);
        self.alerts.push(alert);
    }

    pub fn remove(&mut self, id: i64) {
        self.alerts.retain(|a| a.id != id);
    }

    pub fn alerts(&self) -> &[Alert] {
        &self.alerts
    }

    /// Instruments price/move/volume alerts watch (they should stay subscribed).
    pub fn instruments(&self) -> Vec<InstrumentId> {
        let mut ids: Vec<InstrumentId> = self
            .alerts
            .iter()
            .filter(|a| !matches!(a.condition, Condition::OrderFilled { .. }))
            .filter_map(|a| a.condition.instrument().cloned())
            .collect();
        ids.sort_by_key(|i| i.to_string());
        ids.dedup();
        ids
    }

    /// Fire `hit` alerts that are off cooldown; one-shots are removed.
    fn fire(&mut self, now: DateTime<Utc>, hit: impl Fn(&Alert) -> Option<String>) -> Vec<Firing> {
        let mut out = Vec::new();
        for a in self.alerts.iter_mut() {
            if a.last_fired_at.is_some_and(|t| now - t < COOLDOWN) {
                continue;
            }
            if let Some(text) = hit(a) {
                a.last_fired_at = Some(now);
                out.push(Firing { alert_id: a.id, account: a.account.clone(), text, deactivate: a.once });
            }
        }
        let gone: Vec<i64> = out.iter().filter(|f| f.deactivate).map(|f| f.alert_id).collect();
        self.alerts.retain(|a| !gone.contains(&a.id));
        out
    }

    /// A real trade print. `adv` is the instrument's average daily traded value, if known.
    pub fn on_trade(&mut self, t: &Trade, adv: Option<Decimal>, now: DateTime<Utc>) -> Vec<Firing> {
        let watched = self.alerts.iter().any(|a| a.condition.instrument() == Some(&t.instrument) && !matches!(a.condition, Condition::OrderFilled { .. }));
        if !watched {
            return Vec::new();
        }
        let h = self.history.entry(t.instrument.clone()).or_default();
        h.push_back((t.at, t.price, t.price * t.qty));
        while h.front().is_some_and(|(at, _, _)| now - *at > MAX_WINDOW) {
            h.pop_front();
        }
        let history = h.clone();
        let id = t.instrument.clone();
        self.fire(now, |a| match &a.condition {
            Condition::PriceAbove { id: i, price } if *i == id && t.price >= *price => Some(format!("{id} traded at {} (at or above {price})", t.price)),
            Condition::PriceBelow { id: i, price } if *i == id && t.price <= *price => Some(format!("{id} traded at {} (at or below {price})", t.price)),
            Condition::Move { id: i, pct, window_minutes } if *i == id => {
                let since = now - Duration::minutes(i64::from(*window_minutes));
                let (_, base, _) = history.iter().find(|(at, _, _)| *at >= since)?;
                if base.is_zero() {
                    return None;
                }
                let change = (t.price / base - Decimal::ONE) * Decimal::ONE_HUNDRED;
                (change.abs() >= *pct).then(|| format!("{id} moved {}% in {window_minutes} min ({base} → {})", change.round_dp(2), t.price))
            }
            Condition::VolumeSurge { id: i, factor, window_minutes } if *i == id => {
                let adv = adv.filter(|v| *v > Decimal::ZERO)?;
                let since = now - Duration::minutes(i64::from(*window_minutes));
                let traded: Decimal = history.iter().filter(|(at, _, _)| *at >= since).map(|(_, _, v)| *v).sum();
                let pace = adv * Decimal::from(*window_minutes) / Decimal::from(1440);
                (traded >= *factor * pace).then(|| format!("{id} traded {} in {window_minutes} min, {}x the average pace", traded.round_dp(0), (traded / pace).round_dp(1)))
            }
            _ => None,
        })
    }

    pub fn on_fill(&mut self, f: &Fill, now: DateTime<Utc>) -> Vec<Firing> {
        self.fire(now, |a| match &a.condition {
            Condition::OrderFilled { id } if a.account == f.account && id.as_ref().is_none_or(|i| *i == f.instrument) => Some(format!(
                "order {} filled: {:?} {} {} at {} (fee {})",
                f.order_id, f.side, f.qty, f.instrument, f.price, f.fee
            )),
            _ => None,
        })
    }

    /// Clock tick: detect session opens and closes. The first tick only records the state.
    pub fn on_tick(&mut self, calendar: &Calendar, now: DateTime<Utc>) -> Vec<Firing> {
        let mut changed = Vec::new();
        for venue in [Venue::Krx, Venue::Us] {
            let open = calendar.is_open(venue, now);
            if let Some(was) = self.open.insert(venue, open) {
                if was != open {
                    changed.push((venue, open));
                }
            }
        }
        self.fire(now, |a| match &a.condition {
            Condition::SessionOpen { venue } if changed.contains(&(*venue, true)) => Some(format!("{} opened", venue.tag())),
            Condition::SessionClose { venue } if changed.contains(&(*venue, false)) => Some(format!("{} closed", venue.tag())),
            _ => None,
        })
    }
}

/// At most `PER_HOUR` deliveries per account per rolling hour.
#[derive(Debug, Default)]
pub struct Limiter {
    sent: HashMap<String, VecDeque<DateTime<Utc>>>,
}

pub const PER_HOUR: usize = 20;

impl Limiter {
    pub fn admit(&mut self, account: &str, now: DateTime<Utc>) -> bool {
        let q = self.sent.entry(account.to_string()).or_default();
        while q.front().is_some_and(|t| now - *t >= Duration::hours(1)) {
            q.pop_front();
        }
        if q.len() >= PER_HOUR {
            return false;
        }
        q.push_back(now);
        true
    }
}
```

`a_move_needs_history_from_the_start_of_the_window`: at t20 the window is [t10, t20] and the only print inside it is the t20 print itself (150), so the change is 0 and nothing fires. At t21 the base is 150, 152/150 is +1.33%, and it fires. That confirms a stale reference outside the window is never used.

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test --lib alerts::`
Expected: 8 passed.

- [ ] **Step 5: Commit**

```bash
git add src/alerts/mod.rs
git commit -m "Evaluate alerts with cooldowns and a per-account delivery limit"
git log -1 --format=%B
```

---

### Task 3: Delivery: notifier, alert loop, pins

**Files:**
- Create: `src/alerts/deliver.rs`
- Modify: `src/alerts/mod.rs` (`pub mod deliver;`), `src/market.rs` (extra pins), `src/broker.rs` (`stats`), `Cargo.toml` (`zyris-attacca`)

**Interfaces:**
- Produces:
  - `trait Notifier: Send + Sync { async fn send(&self, agent_id: &str, account: &str, session: Option<String>, text: &str) -> anyhow::Result<String> }`, which returns the session id used.
  - `ConnSlot`, which is `Clone` and has `put(Connection)` and `get() -> Option<Connection>`.
  - `AttaccaNotifier::new(ConnSlot)`.
  - `enum AlertCmd { Upsert(Alert), Remove(i64) }`.
  - `alert_loop(app: Arc<App>, bus: broadcast::Receiver<BusEvent>, cmds: mpsc::UnboundedReceiver<AlertCmd>, notifier: Arc<dyn Notifier>, generations: HashMap<String, i32>)`, an async function.
  - `Market::set_extra_pins(Vec<InstrumentId>)`, which `refresh_pins` merges in.
  - `SimBroker::stats(&InstrumentId) -> Option<DailyStats>`.

- [ ] **Step 1: Write the failing test** (append to `tests/tools.rs`, reusing its rig)

```rust
struct Recorder {
    sent: std::sync::Mutex<Vec<(String, String, Option<String>, String)>>,
    fail: std::sync::atomic::AtomicBool,
}

#[async_trait]
impl atrader::alerts::deliver::Notifier for Recorder {
    async fn send(&self, agent_id: &str, account: &str, session: Option<String>, text: &str) -> anyhow::Result<String> {
        if self.fail.load(std::sync::atomic::Ordering::SeqCst) {
            anyhow::bail!("attacca unreachable");
        }
        self.sent.lock().unwrap().push((agent_id.into(), account.into(), session, text.into()));
        Ok("sess-1".into())
    }
}

#[sqlx::test]
async fn fired_alerts_reach_the_agent_once(pool: PgPool) {
    use atrader::alerts::{Alert, Condition, deliver::{AlertCmd, alert_loop}};
    let (app, _t) = rig(pool).await;
    let (bus, _) = broadcast::channel(64);
    let (cmd_tx, cmd_rx) = mpsc::unbounded_channel();
    let rec = Arc::new(Recorder { sent: Default::default(), fail: Default::default() });
    let gens = std::collections::HashMap::from([("bot".to_string(), 1)]);
    tokio::spawn(alert_loop(app.clone(), bus.subscribe(), cmd_rx, rec.clone(), gens));
    let alert = Alert {
        id: 0,
        account: "bot".into(),
        generation: 1,
        condition: Condition::PriceAbove { id: "UPBIT:KRW-BTC".parse().unwrap(), price: dec!(100000000) },
        note: "breakout".into(),
        once: true,
        created_at: Utc::now(),
        last_fired_at: None,
    };
    let id = app.store.create_alert(&alert).await.unwrap();
    cmd_tx.send(AlertCmd::Upsert(Alert { id, ..alert })).unwrap();
    tokio::time::sleep(Duration::from_millis(50)).await;
    let print = |p| atrader::market::BusEvent::Market(MarketEvent::Trade(Trade { instrument: "UPBIT:KRW-BTC".parse().unwrap(), price: p, qty: dec!(1), at: Utc::now() }));
    bus.send(print(dec!(100000000))).unwrap();
    bus.send(print(dec!(100100000))).unwrap();
    for _ in 0..100 {
        if !rec.sent.lock().unwrap().is_empty() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    tokio::time::sleep(Duration::from_millis(100)).await;
    let sent = rec.sent.lock().unwrap().clone();
    assert_eq!(sent.len(), 1, "{sent:?}");
    let (agent, account, session, text) = &sent[0];
    assert_eq!((agent.as_str(), account.as_str(), session.as_deref()), ("agent-1", "bot", None));
    assert!(text.contains("breakout") && text.contains("#") && text.contains("equity"), "{text}");
    assert!(app.store.active_alerts("bot", 1).await.unwrap().is_empty()); // one-shot persisted as off
    assert_eq!(app.store.alert_session("bot").await.unwrap().as_deref(), Some("sess-1"));
}

#[sqlx::test]
async fn failed_deliveries_are_recorded(pool: PgPool) {
    use atrader::alerts::{Alert, Condition, deliver::{AlertCmd, alert_loop}};
    let (app, _t) = rig(pool.clone()).await;
    let (bus, _) = broadcast::channel(64);
    let (cmd_tx, cmd_rx) = mpsc::unbounded_channel();
    let rec = Arc::new(Recorder { sent: Default::default(), fail: std::sync::atomic::AtomicBool::new(true) });
    tokio::spawn(alert_loop(app.clone(), bus.subscribe(), cmd_rx, rec, std::collections::HashMap::from([("bot".to_string(), 1)])));
    let alert = Alert { id: 0, account: "bot".into(), generation: 1, condition: Condition::OrderFilled { id: None }, note: "fills".into(), once: false, created_at: Utc::now(), last_fired_at: None };
    let id = app.store.create_alert(&alert).await.unwrap();
    cmd_tx.send(AlertCmd::Upsert(Alert { id, ..alert })).unwrap();
    tokio::time::sleep(Duration::from_millis(50)).await;
    let fill = atrader::broker::Fill {
        order_id: 1,
        account: "bot".into(),
        instrument: "UPBIT:KRW-BTC".parse().unwrap(),
        side: Side::Buy,
        qty: dec!(0.1),
        notional: dec!(10000000),
        price: dec!(100000000),
        fee: dec!(5000),
        tax: dec!(0),
        realized_pnl: None,
        liquidity: atrader::broker::Liquidity::Taker,
        at: Utc::now(),
    };
    bus.send(atrader::market::BusEvent::Fill(fill)).unwrap();
    let mut row = None;
    for _ in 0..100 {
        row = sqlx::query_as::<_, (bool, Option<String>)>("SELECT delivered, error FROM alert_events").fetch_optional(&pool).await.unwrap();
        if row.is_some() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    let (delivered, error) = row.expect("an event row");
    assert!(!delivered);
    assert!(error.unwrap().contains("unreachable"));
}
```

These tests need two more imports at the top of `tests/tools.rs`: `atrader::domain::Trade` (already under `domain::*`) and `Side` (also under `domain::*`).

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test --test tools alert`
Expected: compile errors.

- [ ] **Step 3: Implement**

Run `cargo add zyris-attacca --git https://github.com/attacca-cc/zyris-protocol --rev 4614f0ea16f646408df590c0b249bd34c7a05d69`.

`SimBroker::stats`:

```rust
    pub fn stats(&self, id: &InstrumentId) -> Option<DailyStats> {
        self.world.lock().unwrap().stats.get(id).copied()
    }
```

`Market`: add `extra: std::sync::Mutex<Vec<InstrumentId>>` (initialised empty) and:

```rust
    /// Instruments that must stay subscribed besides open exposure (alerts watch them).
    pub fn set_extra_pins(&self, ids: Vec<InstrumentId>) {
        *self.extra.lock().unwrap() = ids;
        self.refresh_pins();
    }
```

In `refresh_pins`, chain the extra ids with `active` before filtering by venue: `let mut active = self.broker.active_instruments(); active.extend(self.extra.lock().unwrap().iter().cloned());`.

Create `src/alerts/deliver.rs` and add `pub mod deliver;` to `alerts/mod.rs`:

```rust
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
    generations: HashMap<String, i32>,
) {
    let mut watcher = Watcher::default();
    match app.store.all_active_alerts(&generations).await {
        Ok(alerts) => alerts.into_iter().for_each(|a| watcher.upsert(a)),
        Err(e) => tracing::error!(error = %e, "could not load alerts"),
    }
    app.market.set_extra_pins(watcher.instruments());
    let mut limiter = Limiter::default();
    let mut digest: HashMap<String, VecDeque<(i64, String)>> = HashMap::new();
    let mut tick = tokio::time::interval(StdDuration::from_secs(30));
    loop {
        let firings: Vec<Firing> = tokio::select! {
            cmd = cmds.recv() => {
                match cmd {
                    Some(AlertCmd::Upsert(a)) => watcher.upsert(a),
                    Some(AlertCmd::Remove(id)) => watcher.remove(id),
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
                        out.push(Firing { alert_id: -1, account: account.clone(), text: format!("{first} alerts fired while messages were rate-limited: {}", items.join(" | ")), deactivate: false });
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
            if f.alert_id > 0 {
                if let Err(e) = app.store.mark_fired(f.alert_id, Utc::now(), f.deactivate).await {
                    tracing::error!(error = %e, alert = f.alert_id, "could not mark alert fired");
                }
            }
            let note = watcher_note(&app, f.alert_id).await;
            let is_digest = f.alert_id < 0;
            if !is_digest && !limiter.admit(&f.account, Utc::now()) {
                digest.entry(f.account.clone()).or_default().push_back((f.alert_id, f.text));
                continue;
            }
            let head = if is_digest { "ATrader alert digest".to_string() } else { format!("ATrader alert #{}", f.alert_id) };
            let text = format!(
                "{head} on account {}: {}.{note}{} Use the trader tools on node {{node}} to act; give a reason for any order.",
                f.account,
                f.text,
                account_line(&app, &f.account).await
            );
            // The digest's alert id is not a row; record it against the first queued alert instead.
            let event_alert = if is_digest { 0 } else { f.alert_id };
            if event_alert > 0 {
                tokio::spawn(deliver(app.clone(), notifier.clone(), f.account.clone(), event_alert, text));
            } else {
                tokio::spawn(deliver_untracked(app.clone(), notifier.clone(), f.account.clone(), text));
            }
        }
    }
}

/// The agent's own note for alert `id`, quoted.
async fn watcher_note(app: &App, id: i64) -> String {
    if id <= 0 {
        return String::new();
    }
    match sqlx_note(app, id).await {
        Some(n) if !n.is_empty() => format!(" Your note: \"{n}\"."),
        _ => String::new(),
    }
}

async fn sqlx_note(app: &App, id: i64) -> Option<String> {
    app.store.alert_note(id).await.ok().flatten()
}

/// Digests have no single alert row to hang an event on; they are logged, not stored.
async fn deliver_untracked(app: Arc<App>, notifier: Arc<dyn Notifier>, account: String, text: String) {
    let Some(agent) = app.agent_accounts().into_iter().find(|a| a.id == account).and_then(|a| a.agent_id) else { return };
    let session = app.store.alert_session(&account).await.ok().flatten();
    if let Err(e) = notifier.send(&agent, &account, session, &text).await {
        tracing::warn!(error = %e, %account, "alert digest delivery failed");
    }
}
```

That loop needs two small helpers elsewhere:
- `SimBroker::calendar(&self) -> Calendar`, which clones the broker's calendar.
- `Store::alert_note(id) -> sqlx::Result<Option<String>>`, which runs `SELECT note FROM alerts WHERE id = $1` with `fetch_optional`.

Keep the note in the watcher instead, to save a query: `Firing` gets a `note: String` field, filled from `a.note` in `Watcher::fire`, and `watcher_note`/`sqlx_note`/`alert_note` are dropped. **Do that:** it is simpler, and the ledger records it as the chosen shape. Update Task 2's `Firing` construction accordingly (`note: a.note.clone()`); the Task 2 tests still pass because they do not compare whole `Firing`s. Digest firings carry an empty note.

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test` (with `DATABASE_URL`)
Expected: all pass, including the two alert delivery tests. The failure test takes about 0.1 s, because it checks the row written after the first attempt, before the 30 s retry.

- [ ] **Step 5: Commit**

```bash
git add Cargo.toml Cargo.lock src tests/tools.rs
git commit -m "Deliver fired alerts to the account's Attacca agent"
git log -1 --format=%B
```

---

### Task 4: Alert tools and wiring

**Files:**
- Modify: `src/app.rs` (`alerts` sender), `src/tools/{mod,dto}.rs`, `src/cli.rs`, `tests/tools.rs`, `README.md`

**Interfaces:**
- Produces:
  - Tools:
    - `create_alert(account, alert: AlertInput) -> AlertView`
    - `list_alerts(account) -> Vec<AlertView>`
    - `delete_alert(account, alert_id: i64) -> AlertView`
  - `App::with_alerts(mpsc::UnboundedSender<AlertCmd>)`.

- [ ] **Step 1: Write the failing tests** (append to `tests/tools.rs`)

```rust
fn alert_input(kind: &str) -> AlertInput {
    AlertInput { kind: kind.into(), instrument: Some("UPBIT:KRW-BTC".into()), venue: None, threshold: Some(dec!(100000000)), window_minutes: None, note: "watch".into(), once: None }
}

#[sqlx::test]
async fn alert_tools_validate_and_scope_to_the_account(pool: PgPool) {
    let (_, t) = rig(pool).await;
    let a = t.create_alert("bot".into(), alert_input("price_above")).await.unwrap();
    assert!(a.once && a.id > 0);
    assert_eq!(t.list_alerts("bot".into()).await.unwrap().len(), 1);

    let mut m = alert_input("move");
    assert_eq!(code(&t.create_alert("bot".into(), m.clone()).await.unwrap_err()), "InvalidParams"); // no window
    m.window_minutes = Some(0);
    assert_eq!(code(&t.create_alert("bot".into(), m.clone()).await.unwrap_err()), "InvalidParams");
    m.window_minutes = Some(30);
    m.threshold = Some(dec!(3));
    t.create_alert("bot".into(), m).await.unwrap();

    let mut no_threshold = alert_input("price_below");
    no_threshold.threshold = None;
    assert_eq!(code(&t.create_alert("bot".into(), no_threshold).await.unwrap_err()), "InvalidParams");
    let mut unknown = alert_input("price_above");
    unknown.instrument = Some("UPBIT:KRW-NOPE".into());
    assert_eq!(code(&t.create_alert("bot".into(), unknown).await.unwrap_err()), "UNKNOWN_INSTRUMENT");
    let session = AlertInput { kind: "session_open".into(), instrument: None, venue: Some("NASDAQ".into()), threshold: None, window_minutes: None, note: "x".into(), once: None };
    assert_eq!(code(&t.create_alert("bot".into(), session).await.unwrap_err()), "InvalidParams");
    assert_eq!(code(&t.create_alert("bot".into(), alert_input("teleport")).await.unwrap_err()), "InvalidParams");
    assert_eq!(code(&t.create_alert("manual".into(), alert_input("price_above")).await.unwrap_err()), "UNKNOWN_ACCOUNT");

    assert_eq!(code(&t.delete_alert("bot".into(), 999_999).await.unwrap_err()), "NOT_FOUND");
    let gone = t.delete_alert("bot".into(), a.id).await.unwrap();
    assert!(!gone.active);
    assert_eq!(t.list_alerts("bot".into()).await.unwrap().len(), 1);
}
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test --test tools alert_tools`
Expected: compile errors.

- [ ] **Step 3: Implement**

`App`: add `pub alerts: Option<tokio::sync::mpsc::UnboundedSender<crate::alerts::deliver::AlertCmd>>`, set to `None` in `new`, plus:

```rust
    pub fn with_alerts(mut self, tx: tokio::sync::mpsc::UnboundedSender<crate::alerts::deliver::AlertCmd>) -> Self {
        self.alerts = Some(tx);
        self
    }
```

Add the DTOs:

```rust
/// An alert to create. Kinds and what they need:
/// `price_above` / `price_below`: instrument + threshold (price);
/// `move`: instrument + threshold (percent) + window_minutes (1-240);
/// `volume_surge`: instrument + threshold (multiple of the average pace, >= 1) + window_minutes;
/// `order_filled`: optional instrument; `session_open` / `session_close`: venue (KRX or US).
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct AlertInput {
    pub kind: String,
    #[serde(default)]
    pub instrument: Option<String>,
    #[serde(default)]
    pub venue: Option<String>,
    #[serde(default)]
    pub threshold: Option<Decimal>,
    #[serde(default)]
    pub window_minutes: Option<u32>,
    /// What you want to remember when it fires — your plan for this moment.
    pub note: String,
    /// Fire once and turn off (default true); false re-arms after a 5-minute cooldown.
    #[serde(default)]
    pub once: Option<bool>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct AlertView {
    pub id: i64,
    pub kind: String,
    pub instrument: Option<String>,
    pub venue: Option<String>,
    pub threshold: Option<Decimal>,
    pub window_minutes: Option<u32>,
    pub note: String,
    pub once: bool,
    pub active: bool,
    pub created_at: DateTime<Utc>,
    pub last_fired_at: Option<DateTime<Utc>>,
}

impl AlertView {
    pub fn from_alert(a: &crate::alerts::Alert, active: bool) -> Self {
        let c = &a.condition;
        AlertView {
            id: a.id,
            kind: c.kind().into(),
            instrument: c.instrument().map(|i| i.to_string()),
            venue: c.venue().map(|v| v.tag().into()),
            threshold: c.threshold(),
            window_minutes: c.window_minutes(),
            note: a.note.clone(),
            once: a.once,
            active,
            created_at: a.created_at,
            last_fired_at: a.last_fired_at,
        }
    }
}
```

Add the trait methods:

```rust
    /// Get woken up when something happens: a price level, a % move, a volume surge, one of
    /// your orders filling, or a stock market opening/closing. When it fires, this account's
    /// Attacca session receives a message with your note. At most 50 active alerts per account.
    async fn create_alert(&self, account: String, alert: AlertInput) -> zyris::Result<AlertView>;

    /// Active alerts of an account.
    async fn list_alerts(&self, account: String) -> zyris::Result<Vec<AlertView>>;

    /// Turn an alert off.
    async fn delete_alert(&self, account: String, alert_id: i64) -> zyris::Result<AlertView>;
```

Add the implementation and the validation helper:

```rust
    fn alert_condition(&self, a: &AlertInput) -> zyris::Result<crate::alerts::Condition> {
        use crate::alerts::Condition;
        let inst = || -> zyris::Result<InstrumentId> { self.known(a.instrument.as_deref().ok_or_else(|| bad(format!("{} needs an instrument", a.kind)))?) };
        let threshold = |what: &str| a.threshold.filter(|t| *t > Decimal::ZERO).ok_or_else(|| bad(format!("{} needs a positive threshold ({what})", a.kind)));
        let window = || a.window_minutes.filter(|w| (1..=240).contains(w)).ok_or_else(|| bad(format!("{} needs window_minutes between 1 and 240", a.kind)));
        let venue = || -> zyris::Result<Venue> {
            match a.venue.as_deref().map(parse_venue).transpose()? {
                Some(v) if v.has_session() => Ok(v),
                _ => Err(bad(format!("{} needs venue KRX or US", a.kind))),
            }
        };
        Ok(match a.kind.trim() {
            "price_above" => Condition::PriceAbove { id: inst()?, price: threshold("price")? },
            "price_below" => Condition::PriceBelow { id: inst()?, price: threshold("price")? },
            "move" => Condition::Move { id: inst()?, pct: threshold("percent")?, window_minutes: window()? },
            "volume_surge" => {
                let f = threshold("multiple of the average pace")?;
                if f < Decimal::ONE {
                    return Err(bad("volume_surge threshold must be at least 1"));
                }
                Condition::VolumeSurge { id: inst()?, factor: f, window_minutes: window()? }
            }
            "order_filled" => Condition::OrderFilled { id: a.instrument.as_deref().map(|i| self.known(i)).transpose()? },
            "session_open" => Condition::SessionOpen { venue: venue()? },
            "session_close" => Condition::SessionClose { venue: venue()? },
            other => return Err(bad(format!("unknown alert kind {other:?}"))),
        })
    }
```

```rust
    async fn create_alert(&self, account: String, alert: AlertInput) -> zyris::Result<AlertView> {
        let row = self.app.agent_account(&account)?;
        let condition = self.alert_condition(&alert)?;
        if alert.note.trim().is_empty() {
            return Err(bad("note is required: say what you want to do when it fires"));
        }
        let active = self.app.store.active_alerts(&row.id, row.generation).await.map_err(upstream)?;
        if active.len() >= 50 {
            return Err(order_error(OrderError::InvalidRequest("50 active alerts is the limit; delete some first".into())));
        }
        let mut a = crate::alerts::Alert {
            id: 0,
            account: row.id.clone(),
            generation: row.generation,
            condition,
            note: alert.note.trim().to_string(),
            once: alert.once.unwrap_or(true),
            created_at: self.app.broker.now(),
            last_fired_at: None,
        };
        a.id = self.app.store.create_alert(&a).await.map_err(upstream)?;
        if let Some(tx) = &self.app.alerts {
            let _ = tx.send(crate::alerts::deliver::AlertCmd::Upsert(a.clone()));
        }
        Ok(AlertView::from_alert(&a, true))
    }

    async fn list_alerts(&self, account: String) -> zyris::Result<Vec<AlertView>> {
        let row = self.app.agent_account(&account)?;
        let alerts = self.app.store.active_alerts(&row.id, row.generation).await.map_err(upstream)?;
        Ok(alerts.iter().map(|a| AlertView::from_alert(a, true)).collect())
    }

    async fn delete_alert(&self, account: String, alert_id: i64) -> zyris::Result<AlertView> {
        let row = self.app.agent_account(&account)?;
        let active = self.app.store.active_alerts(&row.id, row.generation).await.map_err(upstream)?;
        let a = active.into_iter().find(|a| a.id == alert_id).ok_or_else(|| order_error(OrderError::NotFound))?;
        self.app.store.deactivate_alert(&row.id, alert_id).await.map_err(upstream)?;
        if let Some(tx) = &self.app.alerts {
            let _ = tx.send(crate::alerts::deliver::AlertCmd::Remove(alert_id));
        }
        Ok(AlertView::from_alert(&a, false))
    }
```

Wire it up in `src/cli.rs` `serve`:
- Before the node is built: `let slot = crate::alerts::deliver::ConnSlot::default(); let (alert_tx, alert_rx) = mpsc::unbounded_channel();`.
- Chain `.with_alerts(alert_tx)` onto the `App` construction.
- After `app` exists: `tokio::spawn(crate::alerts::deliver::alert_loop(app.clone(), bus.subscribe(), alert_rx, Arc::new(crate::alerts::deliver::AttaccaNotifier::new(slot.clone())), generations.clone()));`. `generations` is already cloned for the other loops; clone it once more here and move the original into `snapshot_loop`.
- On the node builder, before `.build()`: `.on_connect({ let slot = slot.clone(); move |conn| { let slot = slot.clone(); async move { slot.put(conn) } } })`.
- Without zyris the slot stays empty, so deliveries record "not connected to Attacca".

README: add a short "Alerts" paragraph. It says that alerts message the account's agent through Attacca, and that the zyris credential needs the `agents:read`, `sessions:read` and `sessions:write` scopes.

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test` (with `DATABASE_URL`)
Expected: all pass.

- [ ] **Step 5: Smoke**

Run `serve --no-zyris` for 20 s.
Expected: it starts and stops cleanly, with no alert-loop errors.

- [ ] **Step 6: Commit**

```bash
git add src tests README.md
git commit -m "Add alert tools and wire alert delivery into serve"
git log -1 --format=%B
```
