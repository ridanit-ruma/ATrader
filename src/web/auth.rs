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

/// The current code for `secret` (tests and scripted logins).
#[doc(hidden)]
pub fn current_code_for_tests(secret: &str, now: DateTime<Utc>) -> String {
    totp(secret, "u").expect("valid secret").generate(now.timestamp() as u64)
}

/// The 30 s time step `code` belongs to (current, previous or next), if it is valid.
pub fn totp_step(secret: &str, code: &str, now: DateTime<Utc>) -> Option<i64> {
    let code = code.trim();
    if code.len() != 6 || !code.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let t = totp(secret, "u")?;
    let step = now.timestamp() / 30;
    [step, step - 1, step + 1].into_iter().find(|s| t.generate((*s * 30) as u64) == code)
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
    /// Map size that triggers the next sweep of stale keys; doubles so sweeps stay amortised O(1).
    sweep_at: usize,
}

impl Limiter {
    pub fn check(&self, key: &str, now: DateTime<Utc>) -> Result<(), i64> {
        match self.failures.get(key).and_then(|(_, until)| *until) {
            Some(until) if until > now => Err((until - now).num_seconds().max(1)),
            _ => Ok(()),
        }
    }

    pub fn fail(&mut self, key: &str, now: DateTime<Utc>) {
        // Keys come from client input; forget the ones whose window and lockout have both passed
        // so a stream of made-up usernames cannot grow the map without bound.
        if self.failures.len() >= self.sweep_at.max(1000) {
            self.failures.retain(|_, (times, until)| until.is_some_and(|u| u > now) || times.iter().any(|t| now - *t < Duration::minutes(15)));
            self.sweep_at = self.failures.len() * 2;
        }
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

    pub fn len(&self) -> usize {
        self.failures.len()
    }

    pub fn is_empty(&self) -> bool {
        self.failures.is_empty()
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

    /// Accept a TOTP code once: its time step must be newer than the last accepted one.
    pub async fn accept_totp(&self, user_id: i64, secret: &str, code: &str, now: DateTime<Utc>) -> sqlx::Result<bool> {
        let Some(step) = totp_step(secret, code, now) else { return Ok(false) };
        let done = sqlx::query("UPDATE users SET totp_last_step = $2 WHERE id = $1 AND totp_last_step < $2")
            .bind(user_id)
            .bind(step)
            .execute(&self.0)
            .await?;
        Ok(done.rows_affected() == 1)
    }

    /// End every session of a user (after a password or second-factor reset).
    pub async fn delete_user_sessions(&self, user_id: i64) -> sqlx::Result<()> {
        sqlx::query("DELETE FROM user_sessions WHERE user_id = $1").bind(user_id).execute(&self.0).await?;
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
        let code = current_code_for_tests(&secret, now);
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

    #[test]
    fn limiter_forgets_stale_keys() {
        let mut l = Limiter::default();
        let t = Utc.with_ymd_and_hms(2026, 9, 25, 0, 0, 0).unwrap();
        for i in 0..5000 {
            l.fail(&format!("u:guess{i}"), t);
        }
        // Once their window has passed, the old keys are swept as new ones arrive.
        for i in 0..5000 {
            l.fail(&format!("u:later{i}"), t + chrono::Duration::minutes(16));
        }
        assert!(l.len() <= 5000, "{}", l.len());
    }
}
