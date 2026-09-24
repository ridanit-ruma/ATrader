# ATrader Phase 7a (Web Backend: Auth, API, SSE) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** The dashboard's server side:
- single-user login with a mandatory TOTP second factor, sessions, lockout and an audit log;
- a JSON API for accounts, positions, orders, fills with reasons, equity, daily PnL, candles and alerts;
- live account create and reset, with no restart needed;
- an SSE stream;
- serving the SPA.

**Architecture:**
1. **Generation stamping.** The broker now knows each account's generation and stamps every journal event with it. This makes a live reset safe: the persister, the snapshot loop and the alert loop read generations from the broker instead of a startup map.
2. **`src/web/auth.rs`.** Password hashing (argon2id), TOTP (totp-rs), session tokens stored as SHA-256 hashes, and an in-memory failure limiter.
3. **`src/web/mod.rs`.** An axum router whose middleware adds the security headers, a CSRF header check and session auth. Handlers reuse the agent tool logic through `TraderTools::for_dashboard`, which may see every account.
4. **`serve`.** Listens on `ATRADER_HTTP_ADDR` (default `127.0.0.1:8750`); `tailscale serve` fronts it with HTTPS.

**Tech Stack:** axum 0.8, argon2 0.5, totp-rs 5 (`otpauth`, `gen_secret`), sha2, rand, rust-embed 8 (`allow_missing`), rpassword, tower (dev, for `oneshot`).

**Spec:** §10 (API side), §11, and the account and user parts of §12. This covers §15 step 7 (backend half).

## Global Constraints

- Earlier phases' constraints still apply.
- There is one user. It is created with `atrader user create <username>` (password prompted twice, minimum 12 characters). `atrader user reset-2fa <username>` clears TOTP.
- Login:
  - `POST /api/auth/login {username, password, code?}`.
  - A password with TOTP not yet enrolled creates a session with `mfa_pending`. Such a session can only use `/api/auth/totp/*`, `/api/auth/me` and logout.
  - With TOTP enrolled, the request must carry a valid TOTP code or an unused recovery code.
  - `POST /api/auth/totp/setup` returns the secret and the otpauth URL. `POST /api/auth/totp/enable {code}` enables TOTP and returns 10 recovery codes, once.
- Lockout: 5 failures within 15 min for a username or a client (the `X-Forwarded-For` first hop, else the peer IP) lock it out for 2^(n−5) minutes, capped at 60. A failure's error never says which factor failed.
- Sessions:
  - 32 random bytes as hex in cookie `atrader_session` (`HttpOnly; Secure; SameSite=Strict; Path=/`). Only the SHA-256 is stored.
  - Timeouts: 12 h idle, 7 d absolute.
  - `GET/DELETE /api/auth/sessions`.
- CSRF: every non-GET `/api/*` request needs the header `X-Requested-With: atrader`. Without it the answer is 403.
- Headers on every response:
  - `Content-Security-Policy: default-src 'self'; img-src 'self' data:; style-src 'self' 'unsafe-inline'; frame-ancestors 'none'`
  - `X-Frame-Options: DENY`
  - `X-Content-Type-Options: nosniff`
  - `Referrer-Policy: no-referrer`
  - No CORS.
- The audit log records login success and failure, logout, TOTP enable, password change, session revoke, account create and reset. Each row has the IP and username.
- API errors are JSON `{error: code, message}` with 400/401/403/404/409/429/500.
- The SSE stream is `GET /api/stream` and sends events `order`, `fill`, `equity` (every 10 s, per account) and `health` (every 30 s).
- The SPA is embedded from `web/dist` via rust-embed. An unknown non-API path serves `index.html`, and the build succeeds while `web/dist` is empty.

## Review Focus

1. **Only a completed login may use the API.** Endpoints other than login, the TOTP enrolment endpoints, `/me` and logout must reject both a session still pending TOTP and a request with no cookie.
2. **Lockout must not reveal which part failed.** Lockout must trigger per user and per IP, and its response must not say whether the password or the code was wrong. Recovery codes must work exactly once.
3. **A live reset must not mix generations.** A reset while trades and fills are in flight must write every in-flight event to the generation it happened in, clear the account's resting orders and alerts, and let new fills land in the new generation.
4. **Cookie flags and session expiry must hold.** The cookie must be `HttpOnly; Secure; SameSite=Strict`, idle and absolute expiry must be enforced, and a revoked session must stop working immediately.
5. **Input must be validated.** Account ids must be `[a-z0-9_-]{1,32}`, cash must be non-negative and in a known currency, and an unknown account must return 404, not 500.

## File Structure

| File | Responsibility |
| --- | --- |
| `src/broker.rs` | Modified: `Stamped` journal entries, per-account generations, `reset_account` |
| `src/persist.rs`, `src/app.rs`, `src/alerts/deliver.rs`, `src/cli.rs` | Modified: read generations from the broker; `App::create_account` / `App::reset_account` |
| `migrations/0004_web.sql` | `users`, `user_sessions`, `recovery_codes`, `audit_log` |
| `src/web/auth.rs` | Hashing, TOTP, tokens, `Limiter`, user/session store functions |
| `src/web/mod.rs` | Router, middleware, handlers, SSE, static files, `serve_http` |
| `src/tools/mod.rs` | Modified: `TraderTools::for_dashboard` |
| `tests/web.rs` | HTTP-level tests with `tower::ServiceExt::oneshot` |

---

### Task 1: Generation stamping and live account create/reset

**Files:**
- Modify: `src/broker.rs`, `src/persist.rs`, `src/app.rs`, `src/alerts/deliver.rs`, `src/cli.rs`, `tests/broker.rs`, `tests/store.rs`, `tests/tools.rs`

**Interfaces:**
- Produces:
  - `pub struct Stamped { pub generation: i32, pub event: Journal }`. `with_journal` now takes `UnboundedSender<Stamped>`.
  - `SimBroker` methods:
    - `restore_account(id, Portfolio, generation: i32)`. `open_account` uses generation 1.
    - `generations() -> HashMap<String, i32>`.
    - `reset_account(id, cash, generation)`, which drops the account's resting orders without journaling them and replaces its portfolio.
  - `persist(rx, store, bus)`, with no generation map.
  - `snapshot_loop(app)` and `alert_loop(app, bus, cmds, notifier)` read `broker.generations()`.
  - `AlertCmd::DropAccount(String)`.
  - `App::create_account(id, name, agent, cash) -> anyhow::Result<()>` and `App::reset_account(id, cash) -> anyhow::Result<i32>`.

- [ ] **Step 1: Write the failing tests**

Append to `tests/broker.rs`, and update the `journaled`/`drain` helpers to the stamped channel: `drain` returns `Vec<Journal>` by mapping `.event`.

```rust
#[test]
fn journal_entries_carry_the_generation_and_resets_start_clean() {
    let (b, clock, mut rx) = journaled();
    b.place_sync("a", market_buy(dec!(0.1))).unwrap();
    let (resting, _) = b.place_sync("a", limit(btc(), Side::Buy, dec!(0.1), dec!(99000000), Tif::Gtc)).unwrap();
    assert!(std::iter::from_fn(|| rx.try_recv().ok()).all(|s| s.generation == 1));
    b.reset_account("a", &[(Currency::Krw, dec!(5000000))], 2);
    assert_eq!(b.generations()["a"], 2);
    let pf = b.portfolio("a").unwrap();
    assert_eq!((pf.cash(Currency::Krw), pf.available_cash(Currency::Krw)), (dec!(5000000), dec!(5000000)));
    assert!(pf.positions.is_empty());
    assert!(b.on_trade(trade(&clock, btc(), dec!(98000000), dec!(1))).is_empty()); // old resting order is gone
    assert_eq!(b.order(resting.id).unwrap().status, OrderStatus::Cancelled);
    b.place_sync("a", market_buy(dec!(0.01))).unwrap();
    assert!(std::iter::from_fn(|| rx.try_recv().ok()).all(|s| s.generation == 2));
}
```

Update the callers so they compile:
- `restore_account(id, pf)` becomes `restore_account(id, pf, generation)`.
- `persist(rx, store, bus, gens)` becomes `persist(rx, store, bus)`.
- The store tests that feed `persist` send `Stamped { generation: 1, event: … }`.
- The persister generation test is replaced: it now sends generation-1 entries after a reset, and still expects gen-1 balances.
- `alert_loop(…, gens)` becomes `alert_loop(…)`. The rig's accounts are restored at generation 1.

Append to `tests/tools.rs`:

```rust
#[sqlx::test]
async fn accounts_are_created_and_reset_live(pool: PgPool) {
    let (app, t) = rig(pool).await;
    app.create_account("fresh", "Fresh", Some("agent-2"), &[(Currency::Krw, dec!(1000000))]).await.unwrap();
    assert!(t.list_accounts().await.unwrap().iter().any(|a| a.id == "fresh"));
    t.place_order(buy("fresh", Some(dec!(0.001)), "first")).await.unwrap();
    let generation = app.reset_account("fresh", &[(Currency::Krw, dec!(2000000))]).await.unwrap();
    assert_eq!(generation, 2);
    let acct = t.get_account("fresh".into()).await.unwrap();
    assert_eq!(acct.equity_krw, dec!(2000000));
    t.place_order(buy("fresh", Some(dec!(0.001)), "second")).await.unwrap();
    for _ in 0..100 {
        if !app.store.fills("fresh", 2, None, 10).await.unwrap().is_empty() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert_eq!(app.store.fills("fresh", 1, None, 10).await.unwrap().len(), 1);
    assert_eq!(app.store.fills("fresh", 2, None, 10).await.unwrap().len(), 1);
}
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test` (with `DATABASE_URL`)
Expected: compile errors (`reset_account`, `generations`, `Stamped` and `create_account` not found).

- [ ] **Step 3: Implement**

In `src/broker.rs`:
- Add `pub struct Stamped { pub generation: i32, pub event: Journal }` (derive `Debug, Clone, PartialEq`).
- Add `generations: HashMap<AccountId, i32>` to `World`.
- Change `journal` to `Mutex<Option<mpsc::UnboundedSender<Stamped>>>`.
- `emit` looks up the account's generation and sends a `Stamped`. It runs while the world lock is held, so it takes `&World`:

```rust
    fn emit(&self, w: &World, j: Journal) {
        let account = match &j {
            Journal::Order(o) => &o.account,
            Journal::Fill(f) => &f.account,
            Journal::Conversion { account, .. } => account,
        };
        let generation = w.generations.get(account).copied().unwrap_or(1);
        if let Some(tx) = &*self.journal.lock().unwrap() {
            let _ = tx.send(Stamped { generation, event: j });
        }
    }
```

  Pass `w` at every call site: `self.emit(w, Journal::…)`. In `convert_sync`, the world guard is `w`; keep it alive until after the emit.
- `open_account` inserts generation 1. `restore_account(id, pf, generation)` inserts the given generation.

```rust
    pub fn generations(&self) -> HashMap<AccountId, i32> {
        self.world.lock().unwrap().generations.clone()
    }

    /// Start `id` over at `generation` with `cash`: its resting orders are dropped (not journaled —
    /// they belong to the closed generation) and its portfolio replaced.
    pub fn reset_account(&self, id: &str, cash: &[(Currency, Decimal)], generation: i32) {
        let mut guard = self.world.lock().unwrap();
        let w = &mut *guard;
        let open: Vec<OrderId> = w.orders.values().filter(|o| o.account == id && o.status == OrderStatus::Open).map(|o| o.id).collect();
        for oid in open {
            if let Some(o) = w.orders.get_mut(&oid) {
                o.status = OrderStatus::Cancelled;
            }
            for rest in w.resting.values_mut() {
                rest.retain(|r| r.order_id != oid);
            }
        }
        w.accounts.insert(id.to_string(), Portfolio::new(cash));
        w.generations.insert(id.to_string(), generation);
    }
```

In `src/persist.rs`, `persist(rx: UnboundedReceiver<Stamped>, store, bus)` writes each event with `entry.generation`. Delete the cache and `generation()` helper, and delete the `generation` store calls in `write`.

In `src/app.rs`:
- `restore` returns `anyhow::Result<usize>` (the number of accounts) and passes `a.generation` to `restore_account`.
- `snapshot_loop(app)` iterates `app.broker.generations()` each tick.
- Add:

```rust
    /// Create an account and make it tradable immediately.
    pub async fn create_account(&self, id: &str, name: &str, agent: Option<&str>, cash: &[(Currency, Decimal)]) -> anyhow::Result<()> {
        self.store.create_account(id, name, agent, cash, self.broker.now()).await?;
        self.broker.restore_account(id, crate::ledger::Portfolio::new(cash), 1);
        self.reload_accounts().await
    }

    /// Start an account over while serving. Returns the new generation.
    pub async fn reset_account(&self, id: &str, cash: &[(Currency, Decimal)]) -> anyhow::Result<i32> {
        let generation = self.store.reset_account(id, cash, self.broker.now()).await?;
        self.broker.reset_account(id, cash, generation);
        if let Some(tx) = &self.alerts {
            let _ = tx.send(crate::alerts::deliver::AlertCmd::DropAccount(id.to_string()));
        }
        self.reload_accounts().await?;
        Ok(generation)
    }
```

In `src/alerts/deliver.rs`:
- Add `AlertCmd::DropAccount(String)`, handled by removing every watcher alert of that account.
- `alert_loop` loses its `generations` parameter and calls `app.store.all_active_alerts(&app.broker.generations())`.

In `src/cli.rs` `serve`, use the new `restore`, `persist` and `alert_loop` signatures, and pass `app.clone()` to `snapshot_loop`.

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test`
Expected: all pass.

- [ ] **Step 5: Commit**

```bash
git add -A src tests
git commit -m "Stamp journal entries with the account generation; create and reset accounts live"
git log -1 --format=%B
```

---

### Task 2: Auth core and user CLI

**Files:**
- Create: `migrations/0004_web.sql`, `src/web/mod.rs` (`pub mod auth;` only), `src/web/auth.rs`
- Modify: `src/lib.rs`, `src/cli.rs`, `Cargo.toml`

**Interfaces:**
- Produces, in `web::auth`:
  - Password functions: `hash_password(&str) -> String` and `verify_password(&str, &str) -> bool`.
  - TOTP functions: `new_totp_secret() -> String` (base32), `totp_url(secret, username) -> String` and `check_totp(secret, code, now: DateTime<Utc>) -> bool`.
  - Token functions: `new_token() -> String` (64 hex characters) and `token_hash(&str) -> String`.
  - `new_recovery_codes() -> Vec<String>` (10 × `xxxx-xxxx`).
  - `Limiter` with `check(key, now) -> Result<(), i64 /*retry secs*/>`, `fail(key, now)` and `succeed(key)`.
  - `User { id, username, password_hash, totp_secret: Option<String>, totp_enabled }` and `Session { id_hash, user_id, mfa_pending, created_at, last_seen }`.
  - `AuthStore(PgPool)` with these methods:
    - `create_user`, `user_by_name`, `user`
    - `set_totp(user_id, secret: Option<&str>, enabled)`, `set_password`
    - `create_session(user_id, token_hash, mfa_pending, ip, ua, now)`
    - `session(token_hash, now) -> Option<Session>`, which enforces idle and absolute expiry and touches `last_seen`
    - `upgrade_session`, `delete_session(user_id, id_hash)`, `sessions(user_id)`
    - `save_recovery_codes(user_id, &[String])`, `use_recovery_code(user_id, code) -> bool`
    - `audit(user_id: Option<i64>, action, detail, ip)`, `audit_log(limit)`
  - `Store::pool()` accessor, so the web layer builds its `AuthStore` from the same pool.

- [ ] **Step 1: Write the failing tests** (a tests module in `auth.rs` plus a `#[sqlx::test]` in `tests/web.rs`)

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    #[test]
    fn passwords_hash_and_verify() {
        let h = hash_password("correct horse battery");
        assert!(h.starts_with("$argon2id$"));
        assert!(verify_password("correct horse battery", &h));
        assert!(!verify_password("wrong", &h));
        assert!(!verify_password("x", "not a hash"));
    }

    #[test]
    fn totp_codes_verify_for_the_current_step_only() {
        let secret = new_totp_secret();
        let now = Utc.with_ymd_and_hms(2026, 9, 25, 0, 0, 0).unwrap();
        let code = current_code(&secret, now);
        assert!(check_totp(&secret, &code, now));
        assert!(!check_totp(&secret, &code, now + chrono::Duration::minutes(5)));
        assert!(!check_totp(&secret, "abc", now));
        assert!(totp_url(&secret, "ruma").starts_with("otpauth://totp/ATrader:ruma?secret="));
    }

    #[test]
    fn tokens_are_random_and_hashed() {
        let (a, b) = (new_token(), new_token());
        assert_eq!(a.len(), 64);
        assert_ne!(a, b);
        assert_eq!(token_hash(&a), token_hash(&a));
        assert_ne!(token_hash(&a), a);
        let codes = new_recovery_codes();
        assert_eq!(codes.len(), 10);
        assert!(codes.iter().all(|c| c.len() == 9 && c.as_bytes()[4] == b'-'));
    }

    #[test]
    fn limiter_locks_out_after_five_failures_and_backs_off() {
        let mut l = Limiter::default();
        let t = Utc.with_ymd_and_hms(2026, 9, 25, 0, 0, 0).unwrap();
        for _ in 0..4 {
            l.fail("u:ruma", t);
            assert!(l.check("u:ruma", t).is_ok());
        }
        l.fail("u:ruma", t);
        assert_eq!(l.check("u:ruma", t), Err(60));
        assert!(l.check("u:ruma", t + chrono::Duration::seconds(61)).is_ok());
        l.fail("u:ruma", t + chrono::Duration::seconds(61));
        assert_eq!(l.check("u:ruma", t + chrono::Duration::seconds(61)), Err(120));
        l.succeed("u:ruma");
        assert!(l.check("u:ruma", t + chrono::Duration::seconds(62)).is_ok());
        assert!(l.check("ip:1.2.3.4", t).is_ok());
    }
}
```

`tests/web.rs` (store part):

```rust
use atrader::web::auth::*;
use chrono::{Duration, Utc};
use sqlx::PgPool;

#[sqlx::test]
async fn sessions_expire_and_recovery_codes_are_single_use(pool: PgPool) {
    let s = AuthStore(pool);
    let uid = s.create_user("ruma", &hash_password("long enough pass")).await.unwrap();
    let now = Utc::now();
    let tok = new_token();
    s.create_session(uid, &token_hash(&tok), true, "1.2.3.4", "test", now).await.unwrap();
    let sess = s.session(&token_hash(&tok), now).await.unwrap().unwrap();
    assert!(sess.mfa_pending);
    s.upgrade_session(&token_hash(&tok)).await.unwrap();
    assert!(!s.session(&token_hash(&tok), now).await.unwrap().unwrap().mfa_pending);
    assert!(s.session(&token_hash(&tok), now + Duration::hours(13)).await.unwrap().is_none()); // idle
    let tok2 = new_token();
    s.create_session(uid, &token_hash(&tok2), false, "1.2.3.4", "test", now).await.unwrap();
    for h in (1..=7 * 24).step_by(11) {
        s.session(&token_hash(&tok2), now + Duration::hours(h)).await.unwrap(); // keep it warm
    }
    assert!(s.session(&token_hash(&tok2), now + Duration::days(7) + Duration::minutes(1)).await.unwrap().is_none()); // absolute
    s.save_recovery_codes(uid, &["aaaa-bbbb".into()]).await.unwrap();
    assert!(s.use_recovery_code(uid, "aaaa-bbbb").await.unwrap());
    assert!(!s.use_recovery_code(uid, "aaaa-bbbb").await.unwrap());
    s.audit(Some(uid), "login", "ok", "1.2.3.4").await.unwrap();
    assert_eq!(s.audit_log(10).await.unwrap().len(), 1);
}
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test --lib web::auth && cargo test --test web`
Expected: compile errors.

- [ ] **Step 3: Implement**

```bash
cargo add argon2@0.5 sha2 rand@0.8 rpassword
cargo add totp-rs@5 --features otpauth,gen_secret
```

Create `migrations/0004_web.sql`:

```sql
CREATE TABLE users (
    id            BIGSERIAL PRIMARY KEY,
    username      TEXT NOT NULL UNIQUE,
    password_hash TEXT NOT NULL,
    totp_secret   TEXT,
    totp_enabled  BOOLEAN NOT NULL DEFAULT false,
    created_at    TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE TABLE user_sessions (
    id_hash     TEXT PRIMARY KEY,
    user_id     BIGINT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    mfa_pending BOOLEAN NOT NULL,
    ip          TEXT NOT NULL,
    user_agent  TEXT NOT NULL,
    created_at  TIMESTAMPTZ NOT NULL,
    last_seen   TIMESTAMPTZ NOT NULL
);

CREATE TABLE recovery_codes (
    user_id   BIGINT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    code_hash TEXT NOT NULL,
    used_at   TIMESTAMPTZ
);

CREATE TABLE audit_log (
    id      BIGSERIAL PRIMARY KEY,
    at      TIMESTAMPTZ NOT NULL DEFAULT now(),
    user_id BIGINT,
    action  TEXT NOT NULL,
    detail  TEXT NOT NULL,
    ip      TEXT NOT NULL
);
```

Write `src/web/auth.rs`, where `current_code` is a test helper exposed as `pub(crate)`:

```rust
//! Credentials, second factor, session tokens and brute-force limiting.

use std::collections::HashMap;

use argon2::password_hash::{PasswordHash, PasswordHasher, PasswordVerifier, SaltString, rand_core::OsRng};
use argon2::Argon2;
use chrono::{DateTime, Duration, Utc};
use rand::RngCore;
use sha2::{Digest, Sha256};
use sqlx::{PgPool, Row};
use totp_rs::{Algorithm, Secret, TOTP};

pub const IDLE: Duration = Duration::hours(12);
pub const ABSOLUTE: Duration = Duration::days(7);

pub fn hash_password(password: &str) -> String {
    Argon2::default().hash_password(password.as_bytes(), &SaltString::generate(&mut OsRng)).expect("argon2 hashes").to_string()
}

pub fn verify_password(password: &str, hash: &str) -> bool {
    PasswordHash::new(hash).is_ok_and(|h| Argon2::default().verify_password(password.as_bytes(), &h).is_ok())
}

fn totp(secret: &str, account: &str) -> Option<TOTP> {
    let bytes = Secret::Encoded(secret.to_string()).to_bytes().ok()?;
    TOTP::new(Algorithm::SHA1, 6, 1, 30, bytes, Some("ATrader".into()), account.into()).ok()
}

pub fn new_totp_secret() -> String {
    Secret::generate_secret().to_encoded().to_string()
}

pub fn totp_url(secret: &str, username: &str) -> String {
    totp(secret, username).map(|t| t.get_url()).unwrap_or_default()
}

/// Accepts the current 30 s step and one step either side (clock skew).
pub fn check_totp(secret: &str, code: &str, now: DateTime<Utc>) -> bool {
    let code = code.trim();
    code.len() == 6 && code.bytes().all(|b| b.is_ascii_digit()) && totp(secret, "u").is_some_and(|t| t.check(code, now.timestamp() as u64))
}

pub(crate) fn current_code(secret: &str, now: DateTime<Utc>) -> String {
    totp(secret, "u").expect("valid secret").generate(now.timestamp() as u64)
}

pub fn new_token() -> String {
    let mut b = [0u8; 32];
    rand::thread_rng().fill_bytes(&mut b);
    b.iter().map(|x| format!("{x:02x}")).collect()
}

pub fn token_hash(token: &str) -> String {
    Sha256::digest(token.as_bytes()).iter().map(|x| format!("{x:02x}")).collect()
}

pub fn new_recovery_codes() -> Vec<String> {
    const ALPHABET: &[u8] = b"abcdefghjkmnpqrstuvwxyz23456789";
    let mut rng = rand::thread_rng();
    (0..10)
        .map(|_| {
            let mut s: String = (0..8).map(|_| ALPHABET[(rng.next_u32() as usize) % ALPHABET.len()] as char).collect();
            s.insert(4, '-');
            s
        })
        .collect()
}

/// Failures per key (user or client) in a 15-minute window; from the 5th, locked for
/// 2^(n-5) minutes, capped at 60.
#[derive(Debug, Default)]
pub struct Limiter {
    failures: HashMap<String, (Vec<DateTime<Utc>>, Option<DateTime<Utc>>)>,
}

impl Limiter {
    pub fn check(&self, key: &str, now: DateTime<Utc>) -> Result<(), i64> {
        match self.failures.get(key).and_then(|(_, until)| *until) {
            Some(until) if until > now => Err((until - now).num_seconds().max(1)),
            _ => Ok(()),
        }
    }

    pub fn fail(&mut self, key: &str, now: DateTime<Utc>) {
        let (times, until) = self.failures.entry(key.to_string()).or_default();
        times.retain(|t| now - *t < Duration::minutes(15));
        times.push(now);
        if times.len() >= 5 {
            let minutes = 1i64 << (times.len() - 5).min(6);
            *until = Some(now + Duration::minutes(minutes.min(60)));
        }
    }

    pub fn succeed(&mut self, key: &str) {
        self.failures.remove(key);
    }
}

#[derive(Debug, Clone)]
pub struct User {
    pub id: i64,
    pub username: String,
    pub password_hash: String,
    pub totp_secret: Option<String>,
    pub totp_enabled: bool,
}

#[derive(Debug, Clone)]
pub struct Session {
    pub id_hash: String,
    pub user_id: i64,
    pub mfa_pending: bool,
    pub created_at: DateTime<Utc>,
    pub last_seen: DateTime<Utc>,
}

fn user_row(r: &sqlx::postgres::PgRow) -> User {
    User { id: r.get("id"), username: r.get("username"), password_hash: r.get("password_hash"), totp_secret: r.get("totp_secret"), totp_enabled: r.get("totp_enabled") }
}

pub struct AuthStore(pub PgPool);

impl AuthStore {
    pub async fn create_user(&self, username: &str, password_hash: &str) -> sqlx::Result<i64> {
        sqlx::query_scalar("INSERT INTO users (username, password_hash) VALUES ($1, $2) RETURNING id").bind(username).bind(password_hash).fetch_one(&self.0).await
    }

    pub async fn user_by_name(&self, username: &str) -> sqlx::Result<Option<User>> {
        Ok(sqlx::query("SELECT * FROM users WHERE username = $1").bind(username).fetch_optional(&self.0).await?.as_ref().map(user_row))
    }

    pub async fn user(&self, id: i64) -> sqlx::Result<Option<User>> {
        Ok(sqlx::query("SELECT * FROM users WHERE id = $1").bind(id).fetch_optional(&self.0).await?.as_ref().map(user_row))
    }

    pub async fn set_totp(&self, user_id: i64, secret: Option<&str>, enabled: bool) -> sqlx::Result<()> {
        sqlx::query("UPDATE users SET totp_secret = $2, totp_enabled = $3 WHERE id = $1").bind(user_id).bind(secret).bind(enabled).execute(&self.0).await?;
        Ok(())
    }

    pub async fn set_password(&self, user_id: i64, hash: &str) -> sqlx::Result<()> {
        sqlx::query("UPDATE users SET password_hash = $2 WHERE id = $1").bind(user_id).bind(hash).execute(&self.0).await?;
        Ok(())
    }

    pub async fn create_session(&self, user_id: i64, id_hash: &str, mfa_pending: bool, ip: &str, ua: &str, now: DateTime<Utc>) -> sqlx::Result<()> {
        sqlx::query("INSERT INTO user_sessions (id_hash, user_id, mfa_pending, ip, user_agent, created_at, last_seen) VALUES ($1,$2,$3,$4,$5,$6,$6)")
            .bind(id_hash)
            .bind(user_id)
            .bind(mfa_pending)
            .bind(ip)
            .bind(ua)
            .bind(now)
            .execute(&self.0)
            .await?;
        Ok(())
    }

    /// A live session: not idle for 12 h, not older than 7 d. Touches `last_seen`; deletes expired ones.
    pub async fn session(&self, id_hash: &str, now: DateTime<Utc>) -> sqlx::Result<Option<Session>> {
        let Some(r) = sqlx::query("SELECT * FROM user_sessions WHERE id_hash = $1").bind(id_hash).fetch_optional(&self.0).await? else { return Ok(None) };
        let s = Session { id_hash: r.get("id_hash"), user_id: r.get("user_id"), mfa_pending: r.get("mfa_pending"), created_at: r.get("created_at"), last_seen: r.get("last_seen") };
        if now - s.last_seen > IDLE || now - s.created_at > ABSOLUTE {
            sqlx::query("DELETE FROM user_sessions WHERE id_hash = $1").bind(id_hash).execute(&self.0).await?;
            return Ok(None);
        }
        sqlx::query("UPDATE user_sessions SET last_seen = $2 WHERE id_hash = $1").bind(id_hash).bind(now).execute(&self.0).await?;
        Ok(Some(s))
    }

    pub async fn upgrade_session(&self, id_hash: &str) -> sqlx::Result<()> {
        sqlx::query("UPDATE user_sessions SET mfa_pending = false WHERE id_hash = $1").bind(id_hash).execute(&self.0).await?;
        Ok(())
    }

    pub async fn delete_session(&self, user_id: i64, id_hash: &str) -> sqlx::Result<bool> {
        Ok(sqlx::query("DELETE FROM user_sessions WHERE id_hash = $1 AND user_id = $2").bind(id_hash).bind(user_id).execute(&self.0).await?.rows_affected() == 1)
    }

    /// (id_hash, ip, user agent, created, last seen), newest first.
    pub async fn sessions(&self, user_id: i64) -> sqlx::Result<Vec<(String, String, String, DateTime<Utc>, DateTime<Utc>)>> {
        let rows = sqlx::query("SELECT * FROM user_sessions WHERE user_id = $1 ORDER BY last_seen DESC").bind(user_id).fetch_all(&self.0).await?;
        Ok(rows.iter().map(|r| (r.get("id_hash"), r.get("ip"), r.get("user_agent"), r.get("created_at"), r.get("last_seen"))).collect())
    }

    pub async fn save_recovery_codes(&self, user_id: i64, codes: &[String]) -> sqlx::Result<()> {
        let mut tx = self.0.begin().await?;
        sqlx::query("DELETE FROM recovery_codes WHERE user_id = $1").bind(user_id).execute(&mut *tx).await?;
        for c in codes {
            sqlx::query("INSERT INTO recovery_codes (user_id, code_hash) VALUES ($1, $2)").bind(user_id).bind(token_hash(c)).execute(&mut *tx).await?;
        }
        tx.commit().await
    }

    pub async fn use_recovery_code(&self, user_id: i64, code: &str) -> sqlx::Result<bool> {
        let done = sqlx::query("UPDATE recovery_codes SET used_at = now() WHERE user_id = $1 AND code_hash = $2 AND used_at IS NULL")
            .bind(user_id)
            .bind(token_hash(code.trim()))
            .execute(&self.0)
            .await?;
        Ok(done.rows_affected() > 0)
    }

    pub async fn audit(&self, user_id: Option<i64>, action: &str, detail: &str, ip: &str) -> sqlx::Result<()> {
        sqlx::query("INSERT INTO audit_log (user_id, action, detail, ip) VALUES ($1,$2,$3,$4)").bind(user_id).bind(action).bind(detail).bind(ip).execute(&self.0).await?;
        Ok(())
    }

    /// (at, action, detail, ip), newest first.
    pub async fn audit_log(&self, limit: i64) -> sqlx::Result<Vec<(DateTime<Utc>, String, String, String)>> {
        let rows = sqlx::query("SELECT at, action, detail, ip FROM audit_log ORDER BY id DESC LIMIT $1").bind(limit).fetch_all(&self.0).await?;
        Ok(rows.iter().map(|r| (r.get("at"), r.get("action"), r.get("detail"), r.get("ip"))).collect())
    }
}
```

The `limiter_locks_out_after_five_failures_and_backs_off` expectation is that the 5th failure locks for 2^0 = 1 min (60 s) and the 6th for 2^1 = 2 min (120 s).

Add `pub fn pool(&self) -> &PgPool { &self.pool }` to `Store`, and add `pub mod web;` to `lib.rs`.

CLI: add `Command::UserCreate { username }` and `Command::UserReset2fa { username }` (`["user", "create", name]` and `["user", "reset-2fa", name]`):
- `user create` reads the password twice with `rpassword::prompt_password`, requires at least 12 characters and a match, then calls `AuthStore(store.pool().clone()).create_user(name, &hash_password(pw))` and prints `created user <name>; log in to enrol two-factor authentication`.
- `reset-2fa` calls `set_totp(id, None, false)`, deletes that user's recovery codes, and prints a notice.
- Add parse tests for both to `cli.rs`.

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test` (with `DATABASE_URL`)
Expected: all pass.

- [ ] **Step 5: Commit**

```bash
git add -A migrations src tests Cargo.toml Cargo.lock
git commit -m "Add users, TOTP, sessions, lockout and audit log"
git log -1 --format=%B
```

---

### Task 3: HTTP API, SSE and static files

**Files:**
- Modify: `src/web/mod.rs`, `src/tools/mod.rs` (`for_dashboard`), `src/cli.rs`, `Cargo.toml`, `tests/web.rs`, `README.md`

**Interfaces:**
- Produces:
  - `web::router(state: WebState) -> axum::Router`.
  - `WebState { app: Arc<App>, auth: Arc<AuthStore>, limiter: Arc<Mutex<Limiter>>, bus: broadcast::Sender<BusEvent>, cookie_secure: bool }`.
  - `web::serve_http(state, addr)`, which is async.
  - `TraderTools::for_dashboard(app)`, which bypasses the agent-only account filter.
- Routes (all JSON):

  | Method | Path | Result |
  | --- | --- | --- |
  | `POST` | `/api/auth/login` | Login (see Global Constraints) |
  | `POST` | `/api/auth/logout` | End the session |
  | `GET` | `/api/auth/me` | `{username, totp_enabled, mfa_pending}` |
  | `POST` | `/api/auth/totp/setup` | `{secret, otpauth_url}` |
  | `POST` | `/api/auth/totp/enable` | `{recovery_codes}` |
  | `POST` | `/api/auth/password` | `{current, new}` |
  | `GET` | `/api/auth/sessions` | Active sessions |
  | `DELETE` | `/api/auth/sessions/{id}` | Revoke a session |
  | `GET` | `/api/audit` | Audit log |
  | `GET` | `/api/overview` | Every account: summary, day PnL, total return |
  | `POST` | `/api/accounts` | `{id, name, agent_id?, cash}` |
  | `POST` | `/api/accounts/{id}/reset` | `{cash}` |
  | `GET` | `/api/accounts/{id}` | `{summary, positions, open_orders, performance_all}` |
  | `GET` | `/api/accounts/{id}/equity?range=1d\|1w\|1m\|all` | Equity series |
  | `GET` | `/api/accounts/{id}/fills?limit` | Fills with the order's reason |
  | `GET` | `/api/accounts/{id}/orders?open_only` | Orders |
  | `GET` | `/api/accounts/{id}/pnl` | `{daily: [{date, pnl_krw}], by_symbol: [{instrument, realized, fees}]}` |
  | `GET` | `/api/accounts/{id}/alerts` | Alerts |
  | `GET` | `/api/instruments/{id}/chart?interval&limit&account` | `{candles, markers: [{at, side, price, qty}], quote}` |
  | `GET` | `/api/health` | Feeds, subscription counts and journal state |
  | `GET` | `/api/stream` | SSE |
  | `GET` | any other path | Static SPA, falling back to `index.html` |

- [ ] **Step 1: Write the failing tests** (append to `tests/web.rs`)

The test rig reuses the `tests/tools.rs` fake feed. Move `Fake`, `rig` and the helpers into `tests/common/mod.rs` and `mod common;` them from both files.

```rust
mod common;

use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use tower::ServiceExt;

async fn web(pool: PgPool) -> (axum::Router, AuthStore, String) {
    let (app, _t) = common::rig(pool.clone()).await;
    let auth = AuthStore(pool);
    let secret = new_totp_secret();
    let uid = auth.create_user("ruma", &hash_password("long enough pass")).await.unwrap();
    auth.set_totp(uid, Some(&secret), true).await.unwrap();
    let state = atrader::web::WebState::new(app, auth_clone(&auth), false);
    (atrader::web::router(state), auth, secret)
}

fn auth_clone(a: &AuthStore) -> AuthStore {
    AuthStore(a.0.clone())
}

fn req(method: &str, uri: &str, cookie: Option<&str>, body: Option<serde_json::Value>) -> Request<Body> {
    let mut r = Request::builder().method(method).uri(uri).header("x-forwarded-for", "10.0.0.1");
    if method != "GET" {
        r = r.header("x-requested-with", "atrader");
    }
    if let Some(c) = cookie {
        r = r.header(header::COOKIE, format!("atrader_session={c}"));
    }
    match body {
        Some(b) => r.header(header::CONTENT_TYPE, "application/json").body(Body::from(b.to_string())).unwrap(),
        None => r.body(Body::empty()).unwrap(),
    }
}

async fn login(router: &axum::Router, secret: &str) -> String {
    let code = atrader::web::auth::current_code_for_tests(secret, Utc::now());
    let res = router.clone().oneshot(req("POST", "/api/auth/login", None, Some(serde_json::json!({"username": "ruma", "password": "long enough pass", "code": code})))).await.unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let set = res.headers()[header::SET_COOKIE].to_str().unwrap().to_string();
    assert!(set.contains("HttpOnly") && set.contains("SameSite=Strict") && set.contains("Path=/"), "{set}");
    set.split(';').next().unwrap().trim_start_matches("atrader_session=").to_string()
}

#[sqlx::test]
async fn api_requires_a_full_login(pool: PgPool) {
    let (r, _, secret) = web(pool).await;
    assert_eq!(r.clone().oneshot(req("GET", "/api/overview", None, None)).await.unwrap().status(), StatusCode::UNAUTHORIZED);
    let c = login(&r, &secret).await;
    let res = r.clone().oneshot(req("GET", "/api/overview", Some(&c), None)).await.unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    assert_eq!(res.headers()["x-frame-options"], "DENY");
    assert!(res.headers()["content-security-policy"].to_str().unwrap().contains("default-src 'self'"));
    // CSRF header required on mutations.
    let mut no_csrf = req("POST", "/api/auth/logout", Some(&c), None);
    no_csrf.headers_mut().remove("x-requested-with");
    assert_eq!(r.clone().oneshot(no_csrf).await.unwrap().status(), StatusCode::FORBIDDEN);
    assert_eq!(r.clone().oneshot(req("POST", "/api/auth/logout", Some(&c), None)).await.unwrap().status(), StatusCode::OK);
    assert_eq!(r.clone().oneshot(req("GET", "/api/overview", Some(&c), None)).await.unwrap().status(), StatusCode::UNAUTHORIZED);
}

#[sqlx::test]
async fn wrong_factors_are_indistinguishable_and_lock_out(pool: PgPool) {
    let (r, _, secret) = web(pool).await;
    let attempt = |pw: &str, code: &str| req("POST", "/api/auth/login", None, Some(serde_json::json!({"username": "ruma", "password": pw, "code": code})));
    let good_code = atrader::web::auth::current_code_for_tests(&secret, Utc::now());
    let a = r.clone().oneshot(attempt("wrong password!!", &good_code)).await.unwrap();
    let b = r.clone().oneshot(attempt("long enough pass", "000000")).await.unwrap();
    assert_eq!((a.status(), b.status()), (StatusCode::UNAUTHORIZED, StatusCode::UNAUTHORIZED));
    let body = |res: axum::response::Response| async { axum::body::to_bytes(res.into_body(), 10_000).await.unwrap() };
    assert_eq!(body(a).await, body(b).await);
    for _ in 0..3 {
        r.clone().oneshot(attempt("nope nope nope", "000000")).await.unwrap();
    }
    assert_eq!(r.clone().oneshot(attempt("long enough pass", &good_code)).await.unwrap().status(), StatusCode::TOO_MANY_REQUESTS);
}

#[sqlx::test]
async fn pending_totp_sessions_can_only_enrol(pool: PgPool) {
    let (app, _t) = common::rig(pool.clone()).await;
    let auth = AuthStore(pool.clone());
    auth.create_user("new", &hash_password("long enough pass")).await.unwrap();
    let r = atrader::web::router(atrader::web::WebState::new(app, AuthStore(pool), false));
    let res = r.clone().oneshot(req("POST", "/api/auth/login", None, Some(serde_json::json!({"username": "new", "password": "long enough pass"})))).await.unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let c = res.headers()[header::SET_COOKIE].to_str().unwrap().split(';').next().unwrap().trim_start_matches("atrader_session=").to_string();
    assert_eq!(r.clone().oneshot(req("GET", "/api/overview", Some(&c), None)).await.unwrap().status(), StatusCode::FORBIDDEN);
    let setup = r.clone().oneshot(req("POST", "/api/auth/totp/setup", Some(&c), None)).await.unwrap();
    let v: serde_json::Value = serde_json::from_slice(&axum::body::to_bytes(setup.into_body(), 10_000).await.unwrap()).unwrap();
    let code = atrader::web::auth::current_code_for_tests(v["secret"].as_str().unwrap(), Utc::now());
    let en = r.clone().oneshot(req("POST", "/api/auth/totp/enable", Some(&c), Some(serde_json::json!({"code": code})))).await.unwrap();
    let v: serde_json::Value = serde_json::from_slice(&axum::body::to_bytes(en.into_body(), 10_000).await.unwrap()).unwrap();
    assert_eq!(v["recovery_codes"].as_array().unwrap().len(), 10);
    assert_eq!(r.clone().oneshot(req("GET", "/api/overview", Some(&c), None)).await.unwrap().status(), StatusCode::OK);
}

#[sqlx::test]
async fn accounts_are_managed_over_http(pool: PgPool) {
    let (r, _, secret) = web(pool).await;
    let c = login(&r, &secret).await;
    let bad = r.clone().oneshot(req("POST", "/api/accounts", Some(&c), Some(serde_json::json!({"id": "Bad Id!", "name": "x", "cash": {"KRW": "1"}})))).await.unwrap();
    assert_eq!(bad.status(), StatusCode::BAD_REQUEST);
    let neg = r.clone().oneshot(req("POST", "/api/accounts", Some(&c), Some(serde_json::json!({"id": "neg", "name": "x", "cash": {"KRW": "-1"}})))).await.unwrap();
    assert_eq!(neg.status(), StatusCode::BAD_REQUEST);
    let ok = r.clone().oneshot(req("POST", "/api/accounts", Some(&c), Some(serde_json::json!({"id": "swing", "name": "Swing", "agent_id": "ag", "cash": {"KRW": "5000000"}})))).await.unwrap();
    assert_eq!(ok.status(), StatusCode::OK);
    assert_eq!(r.clone().oneshot(req("GET", "/api/accounts/swing", Some(&c), None)).await.unwrap().status(), StatusCode::OK);
    assert_eq!(r.clone().oneshot(req("GET", "/api/accounts/missing", Some(&c), None)).await.unwrap().status(), StatusCode::NOT_FOUND);
    let reset = r.clone().oneshot(req("POST", "/api/accounts/swing/reset", Some(&c), Some(serde_json::json!({"cash": {"KRW": "1000"}})))).await.unwrap();
    assert_eq!(reset.status(), StatusCode::OK);
    let chart = r.clone().oneshot(req("GET", "/api/instruments/UPBIT:KRW-BTC/chart?interval=1d&limit=10&account=bot", Some(&c), None)).await.unwrap();
    assert_eq!(chart.status(), StatusCode::OK);
    let audit = r.clone().oneshot(req("GET", "/api/audit", Some(&c), None)).await.unwrap();
    let v: serde_json::Value = serde_json::from_slice(&axum::body::to_bytes(audit.into_body(), 100_000).await.unwrap()).unwrap();
    assert!(v.as_array().unwrap().iter().any(|e| e["action"] == "account_reset"));
}
```

`current_code_for_tests` is `current_code` re-exported publicly under that name, with `#[doc(hidden)]`.

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test --test web`
Expected: compile errors.

- [ ] **Step 3: Implement**

```bash
cargo add axum@0.8
cargo add rust-embed@8 --features include-exclude
cargo add --dev tower@0.5 --features util
```

`TraderTools`:
1. Add a field `any_account: bool`, set to `false` in `new`.
2. Add `pub fn for_dashboard(app) -> Self` that sets `any_account: true`.
3. Add a private helper `fn account(&self, id: &str) -> zyris::Result<AccountRow>`. When `any_account` is set, it looks the account up in `app.store.list_accounts()` (async), otherwise it uses `app.agent_account`.
4. Replace every `self.app.agent_account(&x)?` in the tools with `self.account(&x).await?`. The `valuation` helper takes the row the same way.

`src/web/mod.rs` implements the routes above. The key pieces are below; the remaining handlers are thin adapters over `TraderTools::for_dashboard` and the store.

```rust
//! The dashboard's HTTP side: auth, JSON API, live stream, and the embedded SPA.

pub mod auth;

use std::net::SocketAddr;
use std::sync::{Arc, Mutex};

use axum::extract::{ConnectInfo, Path, Query, Request, State};
use axum::http::{HeaderMap, HeaderValue, Method, StatusCode, header};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response, sse::{Event, KeepAlive, Sse}};
use axum::routing::{delete, get, post};
use axum::{Json, Router};
use chrono::Utc;
use serde::Deserialize;
use serde_json::{Value, json};
use tokio::sync::broadcast;

use crate::app::App;
use crate::market::BusEvent;
use crate::tools::{Trader, TraderTools};
use auth::{AuthStore, Limiter, Session, User};

#[derive(Clone)]
pub struct WebState {
    pub app: Arc<App>,
    pub auth: Arc<AuthStore>,
    pub limiter: Arc<Mutex<Limiter>>,
    pub bus: broadcast::Sender<BusEvent>,
    /// `Secure` cookie flag; false only for tests over plain HTTP.
    pub cookie_secure: bool,
}

impl WebState {
    pub fn new(app: Arc<App>, auth: AuthStore, cookie_secure: bool) -> Self {
        let (bus, _) = broadcast::channel(16);
        WebState { app, auth: Arc::new(auth), limiter: Arc::default(), bus, cookie_secure }
    }

    pub fn with_bus(mut self, bus: broadcast::Sender<BusEvent>) -> Self {
        self.bus = bus;
        self
    }
}

pub struct ApiError(StatusCode, &'static str, String);

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (self.0, Json(json!({"error": self.1, "message": self.2}))).into_response()
    }
}

type ApiResult<T> = Result<Json<T>, ApiError>;

fn internal(e: impl std::fmt::Display) -> ApiError {
    tracing::error!(error = %e, "api error");
    ApiError(StatusCode::INTERNAL_SERVER_ERROR, "internal", "something went wrong".into())
}

/// Map a tool error onto HTTP.
fn tool_error(e: zyris::Error) -> ApiError {
    let code = match &e.code {
        zyris::ErrorCode::Other(c) => c.clone(),
        other => format!("{other:?}"),
    };
    let status = match code.as_str() {
        "UNKNOWN_ACCOUNT" | "UNKNOWN_INSTRUMENT" | "NOT_FOUND" => StatusCode::NOT_FOUND,
        "UPSTREAM_ERROR" => StatusCode::BAD_GATEWAY,
        _ => StatusCode::BAD_REQUEST,
    };
    ApiError(status, "request", e.message)
}

fn client_ip(headers: &HeaderMap, peer: Option<SocketAddr>) -> String {
    headers
        .get("x-forwarded-for")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.split(',').next())
        .map(|s| s.trim().to_string())
        .or_else(|| peer.map(|p| p.ip().to_string()))
        .unwrap_or_else(|| "unknown".into())
}

fn session_cookie(headers: &HeaderMap) -> Option<String> {
    headers.get_all(header::COOKIE).iter().filter_map(|v| v.to_str().ok()).flat_map(|v| v.split(';')).find_map(|kv| kv.trim().strip_prefix("atrader_session=").map(str::to_string))
}

/// Security headers on every response; CSRF header on every non-GET API call.
async fn guard(req: Request, next: Next) -> Response {
    let is_api = req.uri().path().starts_with("/api/");
    let unsafe_method = !matches!(*req.method(), Method::GET | Method::HEAD);
    if is_api && unsafe_method && req.headers().get("x-requested-with").and_then(|v| v.to_str().ok()) != Some("atrader") {
        return ApiError(StatusCode::FORBIDDEN, "csrf", "missing X-Requested-With header".into()).into_response();
    }
    let mut res = next.run(req).await;
    let h = res.headers_mut();
    h.insert("content-security-policy", HeaderValue::from_static("default-src 'self'; img-src 'self' data:; style-src 'self' 'unsafe-inline'; frame-ancestors 'none'"));
    h.insert("x-frame-options", HeaderValue::from_static("DENY"));
    h.insert("x-content-type-options", HeaderValue::from_static("nosniff"));
    h.insert("referrer-policy", HeaderValue::from_static("no-referrer"));
    res
}

/// The logged-in user; rejects missing, expired and (unless `allow_pending`) TOTP-pending sessions.
async fn authed(s: &WebState, headers: &HeaderMap, allow_pending: bool) -> Result<(User, Session), ApiError> {
    let unauth = || ApiError(StatusCode::UNAUTHORIZED, "unauthenticated", "log in first".into());
    let token = session_cookie(headers).ok_or_else(unauth)?;
    let session = s.auth.session(&auth::token_hash(&token), Utc::now()).await.map_err(internal)?.ok_or_else(unauth)?;
    if session.mfa_pending && !allow_pending {
        return Err(ApiError(StatusCode::FORBIDDEN, "mfa_required", "finish two-factor enrolment".into()));
    }
    let user = s.auth.user(session.user_id).await.map_err(internal)?.ok_or_else(unauth)?;
    Ok((user, session))
}
```

Login handler (the core of Review Focus 2):

```rust
#[derive(Deserialize)]
struct LoginBody {
    username: String,
    password: String,
    #[serde(default)]
    code: Option<String>,
}

async fn login(State(s): State<WebState>, ConnectInfo(peer): ConnectInfo<SocketAddr>, headers: HeaderMap, Json(b): Json<LoginBody>) -> Result<Response, ApiError> {
    let ip = client_ip(&headers, Some(peer));
    let (ukey, ikey) = (format!("u:{}", b.username), format!("ip:{ip}"));
    let now = Utc::now();
    {
        let l = s.limiter.lock().unwrap();
        if let Err(wait) = l.check(&ukey, now).and(l.check(&ikey, now)) {
            return Err(ApiError(StatusCode::TOO_MANY_REQUESTS, "locked", format!("too many attempts; try again in {wait} s")));
        }
    }
    let user = s.auth.user_by_name(&b.username).await.map_err(internal)?;
    let password_ok = user.as_ref().is_some_and(|u| auth::verify_password(&b.password, &u.password_hash));
    let second_ok = match (&user, password_ok) {
        (Some(u), true) if u.totp_enabled => {
            let code = b.code.as_deref().unwrap_or_default();
            match u.totp_secret.as_deref() {
                Some(sec) if s.auth.accept_totp(u.id, sec, code, now).await.map_err(internal)? => true,
                _ => s.auth.use_recovery_code(u.id, code).await.map_err(internal)?,
            }
        }
        (Some(_), true) => true, // not enrolled yet: the session starts pending
        _ => false,
    };
    let Some(user) = user.filter(|_| password_ok && second_ok) else {
        let mut l = s.limiter.lock().unwrap();
        l.fail(&ukey, now);
        l.fail(&ikey, now);
        drop(l);
        let _ = s.auth.audit(None, "login_failed", &b.username, &ip).await;
        return Err(ApiError(StatusCode::UNAUTHORIZED, "login_failed", "wrong username, password or code".into()));
    };
    {
        let mut l = s.limiter.lock().unwrap();
        l.succeed(&ukey);
        l.succeed(&ikey);
    }
    let token = auth::new_token();
    let ua = headers.get(header::USER_AGENT).and_then(|v| v.to_str().ok()).unwrap_or("");
    s.auth.create_session(user.id, &auth::token_hash(&token), !user.totp_enabled, &ip, ua, now).await.map_err(internal)?;
    let _ = s.auth.audit(Some(user.id), "login", "", &ip).await;
    let secure = if s.cookie_secure { "; Secure" } else { "" };
    let cookie = format!("atrader_session={token}; HttpOnly{secure}; SameSite=Strict; Path=/; Max-Age={}", auth::ABSOLUTE.num_seconds());
    Ok(([(header::SET_COOKIE, cookie)], Json(json!({"mfa_pending": !user.totp_enabled}))).into_response())
}
```

- **The other auth handlers** (`logout`, `me`, `totp_setup`, `totp_enable`, `password`, `sessions`, `revoke`, `audit`) follow the same pattern:
  - `authed(&s, &headers, allow_pending)` first. `allow_pending` is true only for `me`, `logout` and the two TOTP endpoints.
  - Then the `AuthStore` call.
  - Then `audit(...)` for state changes.
  - `totp_setup` stores the new secret with `enabled = false`.
  - `totp_enable` verifies the code with `accept_totp` against the stored secret, then calls `set_totp(.., true)` and `save_recovery_codes(new_recovery_codes())`. It then **rotates the session**: it deletes the pending session, creates a fresh full session with a new token and sets it as the cookie, so a token issued before the second factor never becomes a full session. It returns the codes.
  - `password` checks `current`, requires at least 12 characters, calls `set_password(hash_password(new))`, calls `delete_user_sessions`, and issues a fresh session cookie for the caller.
  - `revoke` takes the `id_hash` from `sessions`.
- **Account handlers:**
  - `create_account` validates the id against `^[a-z0-9_-]{1,32}$` (a manual char check) and `cash` as a map of `{KRW|USD|USDT: decimal ≥ 0}` (else 400). It calls `app.create_account`; a duplicate id (a unique violation) is 409. It audits `account_create`.
  - `reset_account` looks the account up (404 if unknown), validates the cash the same way, calls `app.reset_account` and audits `account_reset`.
- **Read handlers.** They call `TraderTools::for_dashboard(s.app.clone())`:
  - `overview` lists accounts from `app.store.list_accounts()`. For each it calls `get_account` and adds `day_pnl_krw`: current equity minus today's daily snapshot at 00:00 KST (the first `daily` snapshot from `store.snapshots(id, gen, Some(today_kst_midnight), true)`), or null. It also adds `total_return_pct` from `get_performance(id, "all")`.
  - `account` returns `{summary: get_account, positions: get_positions, open_orders: list_orders(open_only), performance_all: get_performance("all")}`.
  - `equity` maps `store.snapshots(id, gen, since, daily_only = range != "1d")` to `[{at, equity_krw}]`.
  - `fills` returns `list_fills` plus a `reason` looked up from `store.orders(id, gen, false, 1000)` by `order_id`.
  - `pnl` has two parts:
    - `daily`: the differences between consecutive daily snapshots.
    - `by_symbol`: the realized PnL and fees of `store.fills(…, 100_000)`, grouped by instrument.
  - `alerts` returns `list_alerts`.
  - `chart` returns `{candles: get_candles, markers: fills of `account` (if given) for that instrument, quote: get_quotes([id])[0]}`. It URL-decodes the `{id}` path segment, which axum does.
  - `health` returns `{feeds: [{venue, subscribed}], zyris_connected}`. `subscribed` comes from `app.market.subscribed(venue)`, a new `Market` accessor that returns `current().len()`. `zyris_connected` comes from an `Arc<AtomicBool>` in `WebState` that the cli's `on_connect` sets; it defaults to false.
- **`stream`** subscribes to `s.bus` and maps `BusEvent::Order` and `BusEvent::Fill` to named SSE events carrying JSON (`OrderView::from` and `FillView::from`). It merges in a 10 s interval that emits one `equity` event per account (`value_account`) and a 30 s `health` event. The combining is done with `futures_util::stream::select`, and the response uses `Sse::new(stream).keep_alive(KeepAlive::default())`.
- **Static files:**

```rust
#[derive(rust_embed::RustEmbed)]
#[folder = "web/dist"]
#[allow_missing = true]
struct Assets;

async fn static_file(uri: axum::http::Uri) -> Response {
    let path = uri.path().trim_start_matches('/');
    let (file, name) = match Assets::get(path) {
        Some(f) if !path.is_empty() => (f, path.to_string()),
        _ => match Assets::get("index.html") {
            Some(f) => (f, "index.html".into()),
            None => return (StatusCode::NOT_FOUND, "dashboard not built: run `npm run build` in web/").into_response(),
        },
    };
    let mime = match name.rsplit('.').next() {
        Some("html") => "text/html; charset=utf-8",
        Some("js") => "text/javascript",
        Some("css") => "text/css",
        Some("svg") => "image/svg+xml",
        Some("png") => "image/png",
        Some("woff2") => "font/woff2",
        Some("json") => "application/json",
        _ => "application/octet-stream",
    };
    ([(header::CONTENT_TYPE, mime)], file.data.into_owned()).into_response()
}
```

- **The router:**

```rust
pub fn router(state: WebState) -> Router {
    Router::new()
        .route("/api/auth/login", post(login))
        .route("/api/auth/logout", post(logout))
        .route("/api/auth/me", get(me))
        .route("/api/auth/totp/setup", post(totp_setup))
        .route("/api/auth/totp/enable", post(totp_enable))
        .route("/api/auth/password", post(password))
        .route("/api/auth/sessions", get(sessions))
        .route("/api/auth/sessions/{id}", delete(revoke))
        .route("/api/audit", get(audit))
        .route("/api/overview", get(overview))
        .route("/api/accounts", post(create_account))
        .route("/api/accounts/{id}", get(account))
        .route("/api/accounts/{id}/reset", post(reset_account))
        .route("/api/accounts/{id}/equity", get(equity))
        .route("/api/accounts/{id}/fills", get(fills))
        .route("/api/accounts/{id}/orders", get(orders))
        .route("/api/accounts/{id}/pnl", get(pnl))
        .route("/api/accounts/{id}/alerts", get(alerts))
        .route("/api/instruments/{id}/chart", get(chart))
        .route("/api/health", get(health))
        .route("/api/stream", get(stream))
        .fallback(static_file)
        .layer(middleware::from_fn(guard))
        .with_state(state)
}

pub async fn serve_http(state: WebState, addr: SocketAddr) -> anyhow::Result<()> {
    let listener = tokio::net::TcpListener::bind(addr).await?;
    tracing::info!(%addr, "dashboard listening");
    axum::serve(listener, router(state).into_make_service_with_connect_info::<SocketAddr>()).await?;
    Ok(())
}
```

The `oneshot` tests have no `ConnectInfo`. Handlers therefore take `Option<ConnectInfo<SocketAddr>>`: axum 0.8 implements `OptionalFromRequestParts` for `ConnectInfo`. The tests supply `x-forwarded-for`.

- **`cli.rs` `serve`:** after `app` is built, spawn the dashboard:

```rust
    let addr: std::net::SocketAddr = std::env::var("ATRADER_HTTP_ADDR").unwrap_or_else(|_| "127.0.0.1:8750".into()).parse().context("ATRADER_HTTP_ADDR")?;
    let web = crate::web::WebState::new(app.clone(), crate::web::auth::AuthStore(app.store.pool().clone()), true).with_bus(bus.clone());
    tokio::spawn(async move {
        if let Err(e) = crate::web::serve_http(web, addr).await {
            tracing::error!(error = %e, "dashboard stopped");
        }
    });
```

  Also add `ATRADER_HTTP_ADDR` to `USAGE`, and add the `user` commands to `USAGE`.

- **README:** add a "Dashboard" section:
  - create the user with `atrader user create <name>`;
  - open `http://127.0.0.1:8750` locally, or expose it on the tailnet with `tailscale serve --bg --https=443 http://127.0.0.1:8750`;
  - the first login enrols TOTP.

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test` (with `DATABASE_URL`)
Expected: all pass, including 4 web API tests and 1 web store test.

- [ ] **Step 5: Smoke**

Run `serve --no-zyris`, then:

```bash
curl -s -i 127.0.0.1:8750/api/overview | head -1   # 401
curl -s -i 127.0.0.1:8750/ | head -1               # 404 "dashboard not built" (until 7b)
```

- [ ] **Step 6: Commit**

```bash
git add -A src tests Cargo.toml Cargo.lock README.md
git commit -m "Serve the dashboard API with login, TOTP, SSE and the embedded SPA"
git log -1 --format=%B
```
