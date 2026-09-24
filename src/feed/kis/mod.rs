//! Korea Investment & Securities (KIS) Open API: KRX and US stock market data.

pub mod master;
pub mod ws;

use std::path::{Path, PathBuf};
use std::time::{Duration as StdDuration, Instant};

use anyhow::{Context, anyhow};
use chrono::{DateTime, NaiveDateTime, TimeZone, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

#[derive(Clone)]
pub struct KisConfig {
    pub app_key: String,
    pub app_secret: String,
    /// Mock (모의투자) hosts instead of real ones.
    pub mock: bool,
    /// Where the access token is cached between runs.
    pub state_dir: PathBuf,
}

impl std::fmt::Debug for KisConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("KisConfig").field("mock", &self.mock).field("state_dir", &self.state_dir).finish_non_exhaustive()
    }
}

impl KisConfig {
    /// `KIS_APP_KEY` + `KIS_APP_SECRET` (both required), `KIS_ENV=mock` for mock hosts,
    /// `ATRADER_STATE_DIR` for the token cache.
    pub fn from_env() -> Option<KisConfig> {
        let app_key = std::env::var("KIS_APP_KEY").ok().filter(|s| !s.trim().is_empty())?;
        let app_secret = std::env::var("KIS_APP_SECRET").ok().filter(|s| !s.trim().is_empty())?;
        let mock = std::env::var("KIS_ENV").is_ok_and(|v| v.eq_ignore_ascii_case("mock"));
        Some(KisConfig { app_key: app_key.trim().into(), app_secret: app_secret.trim().into(), mock, state_dir: state_dir() })
    }

    pub fn rest_base(&self) -> &'static str {
        if self.mock { "https://openapivts.koreainvestment.com:29443" } else { "https://openapi.koreainvestment.com:9443" }
    }

    pub fn ws_url(&self) -> &'static str {
        if self.mock { "ws://ops.koreainvestment.com:31000" } else { "ws://ops.koreainvestment.com:21000" }
    }

    fn min_interval(&self) -> StdDuration {
        StdDuration::from_millis(if self.mock { 550 } else { 60 })
    }
}

/// `$ATRADER_STATE_DIR`, else `$XDG_STATE_HOME/atrader`, else `~/.local/state/atrader`.
pub fn state_dir() -> PathBuf {
    if let Ok(d) = std::env::var("ATRADER_STATE_DIR") {
        return d.into();
    }
    if let Ok(d) = std::env::var("XDG_STATE_HOME") {
        return Path::new(&d).join("atrader");
    }
    Path::new(&std::env::var("HOME").unwrap_or_else(|_| ".".into())).join(".local/state/atrader")
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CachedToken {
    pub token: String,
    pub expires_at: DateTime<Utc>,
}

impl CachedToken {
    /// A cached token still good for at least 10 minutes.
    pub fn load(path: &Path, now: DateTime<Utc>) -> Option<CachedToken> {
        let t: CachedToken = serde_json::from_slice(&std::fs::read(path).ok()?).ok()?;
        (t.expires_at - now > chrono::Duration::minutes(10)).then_some(t)
    }

    pub fn save(&self, path: &Path) -> std::io::Result<()> {
        use std::io::Write;
        use std::os::unix::fs::OpenOptionsExt;
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let mut f = std::fs::OpenOptions::new().write(true).create(true).truncate(true).mode(0o600).open(path)?;
        f.write_all(&serde_json::to_vec(self).expect("token serializes"))
    }
}

/// `access_token_token_expired` is KST wall time.
pub fn parse_token_response(body: &Value) -> anyhow::Result<CachedToken> {
    let token = body["access_token"].as_str().ok_or_else(|| anyhow!("token response without access_token"))?;
    let expiry = body["access_token_token_expired"].as_str().ok_or_else(|| anyhow!("token response without expiry"))?;
    let local = NaiveDateTime::parse_from_str(expiry, "%Y-%m-%d %H:%M:%S")?;
    let expires_at = chrono_tz::Asia::Seoul
        .from_local_datetime(&local)
        .single()
        .ok_or_else(|| anyhow!("ambiguous expiry {expiry}"))?
        .with_timezone(&Utc);
    Ok(CachedToken { token: token.to_string(), expires_at })
}

/// KIS reports failures in the body: `rt_cd != "0"` with `msg_cd`/`msg1`.
pub fn check_rt(body: &Value, tr_id: &str) -> anyhow::Result<()> {
    match body["rt_cd"].as_str() {
        Some("0") => Ok(()),
        _ => Err(anyhow!("KIS {tr_id} failed: {} {}", body["msg_cd"].as_str().unwrap_or("?"), body["msg1"].as_str().unwrap_or(""))),
    }
}

pub struct KisClient {
    cfg: KisConfig,
    http: reqwest::Client,
    token: tokio::sync::Mutex<Option<CachedToken>>,
    approval: tokio::sync::Mutex<Option<(String, Instant)>>,
    last_call: tokio::sync::Mutex<Instant>,
}

impl KisClient {
    pub fn new(cfg: KisConfig) -> Self {
        crate::init_tls();
        KisClient {
            cfg,
            http: reqwest::Client::new(),
            token: tokio::sync::Mutex::new(None),
            approval: tokio::sync::Mutex::new(None),
            last_call: tokio::sync::Mutex::new(Instant::now() - StdDuration::from_secs(1)),
        }
    }

    pub fn config(&self) -> &KisConfig {
        &self.cfg
    }

    fn token_path(&self) -> PathBuf {
        self.cfg.state_dir.join(if self.cfg.mock { "kis_token_mock.json" } else { "kis_token.json" })
    }

    /// The access token: memory, then the disk cache, and only then `/oauth2/tokenP`
    /// (limited to one issuance per minute, and each one notifies the user).
    pub async fn token(&self) -> anyhow::Result<String> {
        let mut slot = self.token.lock().await;
        let now = Utc::now();
        if let Some(t) = slot.as_ref().filter(|t| t.expires_at - now > chrono::Duration::minutes(10)) {
            return Ok(t.token.clone());
        }
        if let Some(t) = CachedToken::load(&self.token_path(), now) {
            *slot = Some(t.clone());
            return Ok(t.token);
        }
        let body: Value = self
            .http
            .post(format!("{}/oauth2/tokenP", self.cfg.rest_base()))
            .json(&json!({"grant_type": "client_credentials", "appkey": self.cfg.app_key, "appsecret": self.cfg.app_secret}))
            .send()
            .await?
            .json()
            .await
            .context("KIS token response")?;
        let t = parse_token_response(&body).map_err(|e| anyhow!("KIS token issuance failed: {e} ({})", body["error_description"].as_str().unwrap_or("")))?;
        if let Err(e) = t.save(&self.token_path()) {
            tracing::warn!(error = %e, "could not cache the KIS token; the next start will issue another");
        }
        tracing::info!(expires_at = %t.expires_at, "issued a KIS access token");
        *slot = Some(t.clone());
        Ok(t.token)
    }

    /// WebSocket approval key, reused for 12 h.
    pub async fn approval_key(&self) -> anyhow::Result<String> {
        let mut slot = self.approval.lock().await;
        if let Some((k, _)) = slot.as_ref().filter(|(_, at)| at.elapsed() < StdDuration::from_secs(12 * 3600)) {
            return Ok(k.clone());
        }
        let body: Value = self
            .http
            .post(format!("{}/oauth2/Approval", self.cfg.rest_base()))
            .json(&json!({"grant_type": "client_credentials", "appkey": self.cfg.app_key, "secretkey": self.cfg.app_secret}))
            .send()
            .await?
            .json()
            .await
            .context("KIS approval response")?;
        let key = body["approval_key"].as_str().ok_or_else(|| anyhow!("KIS approval failed"))?.to_string();
        *slot = Some((key.clone(), Instant::now()));
        Ok(key)
    }

    /// A paced GET returning the JSON body once `rt_cd` says success.
    pub async fn get(&self, path: &str, tr_id: &str, query: &[(&str, &str)]) -> anyhow::Result<Value> {
        {
            let mut last = self.last_call.lock().await;
            let wait = self.cfg.min_interval().saturating_sub(last.elapsed());
            tokio::time::sleep(wait).await;
            *last = Instant::now();
        }
        let token = self.token().await?;
        let body: Value = self
            .http
            .get(format!("{}{path}", self.cfg.rest_base()))
            .query(query)
            .header("content-type", "application/json; charset=utf-8")
            .header("authorization", format!("Bearer {token}"))
            .header("appkey", &self.cfg.app_key)
            .header("appsecret", &self.cfg.app_secret)
            .header("tr_id", tr_id)
            .header("custtype", "P")
            .send()
            .await?
            .json()
            .await
            .with_context(|| format!("KIS {tr_id} response"))?;
        check_rt(&body, tr_id)?;
        Ok(body)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn cfg(dir: &std::path::Path) -> KisConfig {
        KisConfig { app_key: "APPKEY123".into(), app_secret: "SECRET456".into(), mock: false, state_dir: dir.into() }
    }

    #[test]
    fn token_cache_round_trips_with_private_permissions() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("kis_token.json");
        let now = Utc.with_ymd_and_hms(2026, 9, 24, 0, 0, 0).unwrap();
        let t = CachedToken { token: "tok".into(), expires_at: now + chrono::Duration::hours(20) };
        t.save(&path).unwrap();
        assert_eq!(CachedToken::load(&path, now), Some(t));
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(std::fs::metadata(&path).unwrap().permissions().mode() & 0o777, 0o600);
    }

    #[test]
    fn nearly_expired_or_garbage_tokens_are_not_reused() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("kis_token.json");
        let now = Utc.with_ymd_and_hms(2026, 9, 24, 0, 0, 0).unwrap();
        CachedToken { token: "tok".into(), expires_at: now + chrono::Duration::minutes(5) }.save(&path).unwrap();
        assert_eq!(CachedToken::load(&path, now), None);
        std::fs::write(&path, "{").unwrap();
        assert_eq!(CachedToken::load(&path, now), None);
        assert_eq!(CachedToken::load(&dir.path().join("missing"), now), None);
    }

    #[test]
    fn parses_token_response_expiry_in_kst() {
        let body = serde_json::json!({"access_token": "abc", "token_type": "Bearer", "expires_in": 86400, "access_token_token_expired": "2026-09-25 09:00:00"});
        let t = parse_token_response(&body).unwrap();
        assert_eq!(t.token, "abc");
        assert_eq!(t.expires_at, Utc.with_ymd_and_hms(2026, 9, 25, 0, 0, 0).unwrap());
    }

    #[test]
    fn config_hosts_and_debug_redaction() {
        let dir = tempfile::tempdir().unwrap();
        let mut c = cfg(dir.path());
        assert_eq!(c.rest_base(), "https://openapi.koreainvestment.com:9443");
        assert_eq!(c.ws_url(), "ws://ops.koreainvestment.com:21000");
        c.mock = true;
        assert_eq!(c.rest_base(), "https://openapivts.koreainvestment.com:29443");
        assert_eq!(c.ws_url(), "ws://ops.koreainvestment.com:31000");
        let shown = format!("{c:?}");
        assert!(!shown.contains("APPKEY123") && !shown.contains("SECRET456"), "{shown}");
    }

    #[test]
    fn api_errors_name_the_message_but_not_the_key() {
        let body = serde_json::json!({"rt_cd": "1", "msg_cd": "EGW00201", "msg1": "초당 거래건수를 초과하였습니다."});
        let e = check_rt(&body, "FHKST01010100").unwrap_err().to_string();
        assert!(e.contains("EGW00201") && e.contains("FHKST01010100"), "{e}");
        assert!(check_rt(&serde_json::json!({"rt_cd": "0"}), "x").is_ok());
    }
}
