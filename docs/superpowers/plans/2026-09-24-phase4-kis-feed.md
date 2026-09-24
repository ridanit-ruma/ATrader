# ATrader Phase 4 (KIS Feed: KRX and US Stocks) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Trade Korean (KRX) and US stocks on paper using Korea Investment & Securities (KIS) Open API market data. This covers:
- the instrument lists from the KIS master files;
- REST snapshots, previous close and daily stats;
- a real-time WebSocket feed of order books and trades.

The feed is enabled when `KIS_APP_KEY`/`KIS_APP_SECRET` are set.

**Architecture:**
- **`KisClient`** owns the credentials. It caches the access token on disk, because issuance is limited to once a minute and every issuance sends a KakaoTalk notice. It paces REST calls and keeps the US symbol→exchange map and the KRX previous-close cache.
- **Feeds:** `KisKrxFeed` and `KisUsFeed` each implement `MarketFeed` on a shared `Arc<KisClient>`. Each has its own WebSocket session with 10 instruments at most, i.e. 20 registrations, which keeps the pair under KIS's 41-registration limit per appkey.
- **Closed sessions:** outside the venue's session a stream sleeps until the next open instead of reconnecting on idle.
- **Parsing:** everything is parsed by pure functions, tested on real master-file lines and on JSON and frames built from the documented field layouts.

**Tech Stack:** zip (deflate), encoding_rs (cp949), tempfile (dev). Everything else is from earlier phases.

**Spec:** `docs/superpowers/specs/2026-09-24-atrader-design.md`. This plan covers §15 step 4 and implements the `KisFeed` part of §4.

## Global Constraints

- Earlier phases' constraints still apply.
- Hosts:

  | | REST | WebSocket |
  | --- | --- | --- |
  | Real | `https://openapi.koreainvestment.com:9443` | `ws://ops.koreainvestment.com:21000` |
  | Mock (`KIS_ENV=mock`) | `https://openapivts.koreainvestment.com:29443` | `ws://ops.koreainvestment.com:31000` |

- Auth:
  - Token: `POST /oauth2/tokenP` with `{grant_type:"client_credentials", appkey, appsecret}`.
  - WebSocket approval key: `POST /oauth2/Approval` with `{grant_type, appkey, secretkey}`.
- REST headers: `authorization: Bearer`, `appkey`, `appsecret`, `tr_id`, `custtype: P`. Success is `rt_cd == "0"`.
- REST pacing: at least 60 ms between calls on real, 550 ms on mock.
- The token is cached in `$ATRADER_STATE_DIR/kis_token.json` with mode 0600. The default state dir is `$XDG_STATE_HOME/atrader`, falling back to `~/.local/state/atrader`. The token is reused until 10 minutes before `access_token_token_expired` (KST).
- WebSocket frames:
  - Data frames are `flag|tr_id|count|f0^f1^…`. Flag `1` means encrypted, which only applies to execution notices, so those are ignored.
  - JSON frames are acks or `PINGPONG`. A `PINGPONG` frame is echoed back as text.
- Field indices (0-based):
  - `H0STASP0` (KRX book): asks 3–12, bids 13–22, ask qty 23–32, bid qty 33–42.
  - `H0STCNT0` (KRX trade): price 2, trade qty 12.
  - US frames are read relative to `SYMB`, because an `RSYM` field may or may not come first:
    - `HDFSASP0`: bid 10, ask 11, bid qty 12, ask qty 13.
    - `HDFSCNT0`: last 10, per-trade qty 18.
  - US `tr_key` is `D` + EXCD (`NAS`/`NYS`/`AMS`) + symbol.
- US books have 1 level, as KIS provides. KRX books have 10.
- Master files:
  - Location: `https://new.real.download.dws.co.kr/common/master/{kospi_code.mst,kosdaq_code.mst,nasmst.cod,nysmst.cod,amsmst.cod}.zip`.
  - KRX: cp949, fixed width. The short code is bytes 0–9 (trimmed, 6 chars kept) and the name is bytes 21–61.
  - US: cp949, tab-separated, no header. Columns: 2 EXCD, 4 symbol, 6 Korean name, 7 English name, 8 type (keep 2 = stock and 3 = ETP), 9 currency (keep USD).
- Daily stats come from 21 trading days:
  - KRX: `FHKST03010100`, close `stck_clpr`, value `acml_tr_pbmn`.
  - US: `HHDFS76240000`, close `clos`, value `tamt`.
- KRX previous close is `stck_sdpr` from `FHKST01010100`, cached per KST date and attached to every KRX book (for the ±30% band).

## Review Focus

1. **The API token must be reused, not re-requested.** A restart, a second feed or a reconnect must reuse the cached token. Only an expired or missing token may trigger `tokenP`. Task 2 tests this.
2. **KRX books must carry the previous close.** A missing previous close silently disables the ±30% price limit, so KRX books from REST and WebSocket must both have `prev_close` once the stream has started. Task 4 tests this.
3. **Unexpected frames must not break the stream.** An ack, a PINGPONG, an encrypted frame, a record count that does not divide the fields, or a short record must be skipped or answered, never panic or end the stream. Task 3 tests this.
4. **US frames must parse either way.** Parsing must work whether or not `RSYM` is present (the docs disagree). Task 3 tests this.
5. **Credentials must not leak.** The appkey, appsecret, token or approval key must never appear in logs or errors, and the token file must be 0600. Task 2 tests this.

## File Structure

| File | Responsibility |
| --- | --- |
| `src/feed/kis/mod.rs` | `KisConfig`, `KisClient` (auth, token cache, paced REST), `KisKrxFeed`, `KisUsFeed` |
| `src/feed/kis/master.rs` | Master-file download and parsing |
| `src/feed/kis/ws.rs` | Frame parsing, record→event, subscribe messages |
| `src/feed/kis/rest.rs` | REST response parsing (books, previous close, daily stats) |
| `src/feed/mod.rs` | Modified: `pub mod kis;` |
| `src/cli.rs` | Modified: register KIS feeds when configured |
| `tests/fixtures/kis/*` | Real master-file lines |
| `tests/live.rs` | Modified: KIS live checks (ignored; skipped without keys) |

---

### Task 1: Master files

**Files:**
- Create: `src/feed/kis/master.rs`, `src/feed/kis/mod.rs` (just `pub mod master;` for now)
- Modify: `src/feed/mod.rs`, `Cargo.toml`

**Interfaces:**
- Produces:
  - `parse_krx_master(bytes: &[u8]) -> Vec<Instrument>`.
  - `parse_us_master(bytes: &[u8]) -> Vec<(Instrument, String)>`, where the `String` is the EXCD.
  - `unzip_first(bytes: &[u8]) -> anyhow::Result<Vec<u8>>`.
  - `MASTER_BASE: &str`.

- [ ] **Step 1: Add dependencies**

```bash
cargo add zip --no-default-features --features deflate
cargo add encoding_rs
cargo add --dev tempfile
```

- [ ] **Step 2: Write the failing tests**

Add `pub mod kis;` to `src/feed/mod.rs`. Create `src/feed/kis/mod.rs` containing `pub mod master;`. Create `src/feed/kis/master.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn krx_master_keeps_six_char_codes_with_korean_names() {
        let mut bytes = include_bytes!("../../../tests/fixtures/kis/kospi_sample.mst").to_vec();
        bytes.extend_from_slice(include_bytes!("../../../tests/fixtures/kis/kosdaq_sample.mst"));
        let list = parse_krx_master(&bytes);
        let names: Vec<(String, String)> = list.iter().map(|i| (i.id.to_string(), i.name.clone())).collect();
        assert_eq!(
            names,
            vec![
                ("KRX:005930".to_string(), "삼성전자".to_string()),
                ("KRX:000660".to_string(), "SK하이닉스".to_string()),
                ("KRX:900110".to_string(), "딥커머스".to_string()),
            ]
        );
        assert_eq!(list[0].tick, TickRule::Krx);
    }

    #[test]
    fn us_master_keeps_usd_stocks_and_etps_with_exchange() {
        let list = parse_us_master(include_bytes!("../../../tests/fixtures/kis/nas_sample.cod"));
        assert_eq!(list.len(), 3);
        assert_eq!(list[0].0.id.to_string(), "US:AAPL");
        assert_eq!(list[0].0.name, "애플 (APPLE INC)");
        assert_eq!(list[0].1, "NAS");
        assert_eq!(list[0].0.tick, TickRule::Us);
    }

    #[test]
    fn unzip_reads_the_first_member() {
        use std::io::Write;
        let mut buf = std::io::Cursor::new(Vec::new());
        {
            let mut z = zip::ZipWriter::new(&mut buf);
            z.start_file("kospi_code.mst", zip::write::SimpleFileOptions::default()).unwrap();
            z.write_all(b"hello").unwrap();
            z.finish().unwrap();
        }
        assert_eq!(unzip_first(buf.get_ref()).unwrap(), b"hello");
        assert!(unzip_first(b"not a zip").is_err());
    }
}
```

- [ ] **Step 3: Run the tests to verify they fail**

Run: `cargo test --lib kis::master`
Expected: compile errors (`parse_krx_master` not found).

- [ ] **Step 4: Implement** (prepend to `master.rs`)

```rust
//! KIS instrument master files: KRX (fixed width) and US (tab separated), both cp949.

use std::io::Read;

use encoding_rs::EUC_KR;

use crate::domain::{InstrumentId, Venue};
use crate::venue::{Instrument, TickRule, whole_shares};

pub const MASTER_BASE: &str = "https://new.real.download.dws.co.kr/common/master";

/// The first member of a zip archive.
pub fn unzip_first(bytes: &[u8]) -> anyhow::Result<Vec<u8>> {
    let mut archive = zip::ZipArchive::new(std::io::Cursor::new(bytes))?;
    let mut out = Vec::new();
    archive.by_index(0)?.read_to_end(&mut out)?;
    Ok(out)
}

/// KOSPI/KOSDAQ master lines: short code in bytes 0..9, name in bytes 21..61. Funds and other
/// non-6-character codes are dropped.
pub fn parse_krx_master(bytes: &[u8]) -> Vec<Instrument> {
    bytes
        .split(|b| *b == b'\n')
        .filter_map(|line| {
            let line = line.strip_suffix(b"\r").unwrap_or(line);
            if line.len() < 61 {
                return None;
            }
            let code = std::str::from_utf8(&line[0..9]).ok()?.trim();
            if code.len() != 6 || !code.bytes().all(|b| b.is_ascii_alphanumeric()) {
                return None;
            }
            let (name, _, _) = EUC_KR.decode(&line[21..61]);
            Some(Instrument {
                id: InstrumentId { venue: Venue::Krx, symbol: code.to_string() },
                name: name.trim().to_string(),
                tick: TickRule::Krx,
                lot: whole_shares(),
                tradable: true,
            })
        })
        .collect()
}

/// US master rows (NAS/NYS/AMS): keeps USD stocks (type 2) and ETPs (type 3), with the
/// exchange code KIS wants in requests.
pub fn parse_us_master(bytes: &[u8]) -> Vec<(Instrument, String)> {
    let (text, _, _) = EUC_KR.decode(bytes);
    text.lines()
        .filter_map(|line| {
            let c: Vec<&str> = line.split('\t').map(str::trim).collect();
            if c.len() < 10 || !matches!(c[8], "2" | "3") || c[9] != "USD" || c[4].is_empty() {
                return None;
            }
            let name = match (c[6], c[7]) {
                (ko, en) if !ko.is_empty() && ko != en => format!("{ko} ({en})"),
                (_, en) => en.to_string(),
            };
            let inst = Instrument {
                id: InstrumentId { venue: Venue::Us, symbol: c[4].to_string() },
                name,
                tick: TickRule::Us,
                lot: whole_shares(),
                tradable: true,
            };
            Some((inst, c[2].to_string()))
        })
        .collect()
}
```

- [ ] **Step 5: Run the tests to verify they pass**

Run: `cargo test --lib kis::master`
Expected: 3 passed.

If the QQQ row's name has the same Korean and English text, it becomes plain English, and `list.len()` stays 3.

- [ ] **Step 6: Commit**

```bash
git add Cargo.toml Cargo.lock src/feed tests/fixtures/kis
git commit -m "Parse KIS instrument master files"
git log -1 --format=%B
```

---

### Task 2: KIS client: config, token cache, paced REST

**Files:**
- Modify: `src/feed/kis/mod.rs`

**Interfaces:**
- Produces:
  - `KisConfig { app_key, app_secret, mock: bool, state_dir: PathBuf }` with `from_env() -> Option<KisConfig>`, `rest_base()` and `ws_url()`.
  - `CachedToken { token: String, expires_at: DateTime<Utc> }` with `load(path, now) -> Option<CachedToken>` (it returns `None` when the token is expired within 10 minutes or unreadable) and `save(&self, path) -> io::Result<()>` (mode 0600).
  - `KisClient::new(cfg)` with `token()`, `approval_key()` and `get(path, tr_id, query) -> anyhow::Result<serde_json::Value>`.
  - `impl Debug for KisConfig` redacts secrets.

- [ ] **Step 1: Write the failing tests** (append a `tests` module to `src/feed/kis/mod.rs`)

```rust
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
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test --lib kis::tests`
Expected: compile errors.

- [ ] **Step 3: Implement** (put this above `pub mod master;`'s tests module in `src/feed/kis/mod.rs`, after the `pub mod` lines)

```rust
//! Korea Investment & Securities (KIS) Open API: KRX and US stock market data.

pub mod master;

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
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test --lib kis::`
Expected: 8 passed (3 master tests + 5 client tests).

- [ ] **Step 5: Commit**

```bash
git add src/feed/kis
git commit -m "Add KIS client with cached token and paced REST"
git log -1 --format=%B
```

---

### Task 3: WebSocket frames

**Files:**
- Create: `src/feed/kis/ws.rs`
- Modify: `src/feed/kis/mod.rs` (add `pub mod ws;`)

**Interfaces:**
- Produces:
  - Constants `KRX_BOOK`, `KRX_TRADE`, `US_BOOK`, `US_TRADE`.
  - `enum Frame { Data { tr_id: String, records: Vec<Vec<String>> }, Ping(String), Ack { tr_id: String, ok: bool, msg: String }, Ignored }`.
  - `parse_frame(&str) -> anyhow::Result<Frame>`.
  - `record_event(tr_id, &[String], now) -> Option<MarketEvent>`, which returns `None` for short or garbage records.
  - `subscribe_message(approval_key, tr_id, tr_key) -> String`.
  - `us_tr_key(excd, symbol) -> String`.

- [ ] **Step 1: Write the failing tests** (create `src/feed/kis/ws.rs` holding only the tests)

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;
    use rust_decimal_macros::dec;

    fn now() -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 9, 23, 1, 0, 0).unwrap()
    }

    /// A KRX book record: 59 fields, asks 3..13, bids 13..23, ask qty 23..33, bid qty 33..43.
    fn krx_book_record(symbol: &str) -> Vec<String> {
        let mut f = vec!["0".to_string(); 59];
        f[0] = symbol.into();
        f[1] = "100000".into();
        for i in 0..10 {
            f[3 + i] = (70100 + 100 * i).to_string();
            f[13 + i] = (70000 - 100 * i).to_string();
            f[23 + i] = (10 + i).to_string();
            f[33 + i] = (20 + i).to_string();
        }
        f[12] = "0".into(); // an empty 10th ask level
        f
    }

    #[test]
    fn splits_multi_record_data_frames() {
        let rec = krx_book_record("005930");
        let text = format!("0|H0STASP0|002|{}^{}", rec.join("^"), krx_book_record("000660").join("^"));
        let Frame::Data { tr_id, records } = parse_frame(&text).unwrap() else { panic!("not data") };
        assert_eq!((tr_id.as_str(), records.len(), records[1][0].as_str()), ("H0STASP0", 2, "000660"));
    }

    #[test]
    fn krx_book_record_becomes_a_ten_level_book() {
        let Some(MarketEvent::Book(b)) = record_event(KRX_BOOK, &krx_book_record("005930"), now()) else { panic!("no book") };
        assert_eq!(b.instrument.to_string(), "KRX:005930");
        assert_eq!(b.asks.len(), 9); // zero-priced level dropped
        assert_eq!(b.asks[0], Level { price: dec!(70100), qty: dec!(10) });
        assert_eq!(b.bids[0], Level { price: dec!(70000), qty: dec!(20) });
        assert_eq!(b.received_at, now());
    }

    #[test]
    fn krx_trade_uses_per_trade_volume() {
        let mut f = vec!["0".to_string(); 46];
        f[0] = "005930".into();
        f[2] = "70100".into();
        f[12] = "37".into();
        f[13] = "999999".into();
        let Some(MarketEvent::Trade(t)) = record_event(KRX_TRADE, &f, now()) else { panic!("no trade") };
        assert_eq!((t.instrument.to_string(), t.price, t.qty), ("KRX:005930".to_string(), dec!(70100), dec!(37)));
    }

    fn us_book(with_rsym: bool) -> Vec<String> {
        let mut f: Vec<String> = ["AAPL", "4", "20260923", "093000", "20260923", "223000", "100", "200", "0", "0", "187.12", "187.15", "300", "400", "0", "0"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        if with_rsym {
            f.insert(0, "DNASAAPL".into());
        }
        f
    }

    #[test]
    fn us_book_parses_with_or_without_rsym() {
        for with in [true, false] {
            let Some(MarketEvent::Book(b)) = record_event(US_BOOK, &us_book(with), now()) else { panic!("no book ({with})") };
            assert_eq!(b.instrument.to_string(), "US:AAPL");
            assert_eq!(b.bids, vec![Level { price: dec!(187.12), qty: dec!(300) }]);
            assert_eq!(b.asks, vec![Level { price: dec!(187.15), qty: dec!(400) }]);
        }
    }

    #[test]
    fn us_trade_parses_with_or_without_rsym() {
        let mut f: Vec<String> = vec!["0".to_string(); 25];
        f[0] = "AAPL".into();
        f[10] = "187.13".into();
        f[18] = "5".into();
        for with in [true, false] {
            let mut rec = f.clone();
            if with {
                rec.insert(0, "DNASAAPL".into());
            }
            let Some(MarketEvent::Trade(t)) = record_event(US_TRADE, &rec, now()) else { panic!("no trade ({with})") };
            assert_eq!((t.instrument.to_string(), t.price, t.qty), ("US:AAPL".to_string(), dec!(187.13), dec!(5)));
        }
    }

    #[test]
    fn odd_frames_are_skipped_or_answered() {
        let ping = r#"{"header":{"tr_id":"PINGPONG","datetime":"20260923100000"}}"#;
        assert_eq!(parse_frame(ping).unwrap(), Frame::Ping(ping.to_string()));
        let ack = r#"{"header":{"tr_id":"H0STASP0","tr_key":"005930","encrypt":"N"},"body":{"rt_cd":"0","msg_cd":"OPSP0000","msg1":"SUBSCRIBE SUCCESS","output":{"iv":"x","key":"y"}}}"#;
        assert_eq!(parse_frame(ack).unwrap(), Frame::Ack { tr_id: "H0STASP0".into(), ok: true, msg: "SUBSCRIBE SUCCESS".into() });
        let again = r#"{"header":{"tr_id":"H0STASP0"},"body":{"rt_cd":"1","msg1":"ALREADY IN SUBSCRIBE"}}"#;
        assert!(matches!(parse_frame(again).unwrap(), Frame::Ack { ok: true, .. }));
        assert_eq!(parse_frame("1|H0STCNI0|001|encrypted").unwrap(), Frame::Ignored);
        assert!(parse_frame("0|H0STASP0|003|a^b").is_err());
        assert!(parse_frame("0|H0STASP0").is_err());
        assert_eq!(record_event(KRX_BOOK, &["005930".to_string()], now()), None);
        assert_eq!(record_event(KRX_TRADE, &vec!["x".to_string(); 46], now()), None);
        assert_eq!(record_event("H0XXXXX0", &vec!["1".to_string(); 60], now()), None);
    }

    #[test]
    fn subscribe_messages() {
        let v: serde_json::Value = serde_json::from_str(&subscribe_message("KEY", KRX_BOOK, "005930")).unwrap();
        assert_eq!(v["header"]["tr_type"], "1");
        assert_eq!(v["body"]["input"]["tr_id"], "H0STASP0");
        assert_eq!(v["body"]["input"]["tr_key"], "005930");
        assert_eq!(us_tr_key("NAS", "AAPL"), "DNASAAPL");
    }
}
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test --lib kis::ws`
Expected: compile errors.

- [ ] **Step 3: Implement** (prepend to `ws.rs`; add `pub mod ws;` to `kis/mod.rs`)

```rust
//! KIS real-time WebSocket frames: `flag|tr_id|count|f^f^…` data and JSON control messages.

use chrono::{DateTime, Utc};
use rust_decimal::Decimal;
use serde_json::{Value, json};

use crate::domain::{Book, InstrumentId, Level, Trade, Venue};
use crate::feed::MarketEvent;

pub const KRX_BOOK: &str = "H0STASP0";
pub const KRX_TRADE: &str = "H0STCNT0";
pub const US_BOOK: &str = "HDFSASP0";
pub const US_TRADE: &str = "HDFSCNT0";

#[derive(Debug, Clone, PartialEq)]
pub enum Frame {
    Data { tr_id: String, records: Vec<Vec<String>> },
    /// Must be echoed back.
    Ping(String),
    Ack { tr_id: String, ok: bool, msg: String },
    /// Encrypted execution notices: not market data.
    Ignored,
}

pub fn parse_frame(text: &str) -> anyhow::Result<Frame> {
    if text.starts_with('0') || text.starts_with('1') {
        let parts: Vec<&str> = text.splitn(4, '|').collect();
        let [flag, tr_id, count, data] = parts[..] else { anyhow::bail!("short data frame") };
        if flag == "1" {
            return Ok(Frame::Ignored);
        }
        let count: usize = count.parse()?;
        let fields: Vec<&str> = data.split('^').collect();
        if count == 0 || fields.len() % count != 0 {
            anyhow::bail!("{count} records do not divide {} fields", fields.len());
        }
        let per = fields.len() / count;
        let records = fields.chunks(per).map(|c| c.iter().map(|s| s.to_string()).collect()).collect();
        return Ok(Frame::Data { tr_id: tr_id.to_string(), records });
    }
    let v: Value = serde_json::from_str(text)?;
    let tr_id = v["header"]["tr_id"].as_str().unwrap_or_default().to_string();
    if tr_id == "PINGPONG" {
        return Ok(Frame::Ping(text.to_string()));
    }
    let msg = v["body"]["msg1"].as_str().unwrap_or_default().to_string();
    let ok = v["body"]["rt_cd"].as_str() == Some("0") || msg.contains("ALREADY IN SUBSCRIBE");
    Ok(Frame::Ack { tr_id, ok, msg })
}

fn num(s: &str) -> Option<Decimal> {
    s.trim().parse().ok()
}

fn levels(prices: &[String], qtys: &[String]) -> Option<Vec<Level>> {
    let mut out = Vec::new();
    for (p, q) in prices.iter().zip(qtys) {
        let (price, qty) = (num(p)?, num(q)?);
        if price > Decimal::ZERO && qty > Decimal::ZERO {
            out.push(Level { price, qty });
        }
    }
    Some(out)
}

/// US frames may or may not start with RSYM (`DNASAAPL`) before SYMB (`AAPL`); return the
/// fields from SYMB on.
fn from_symb(rec: &[String]) -> &[String] {
    match rec {
        [rsym, symb, ..] if rsym.len() > symb.len() && rsym.ends_with(symb.as_str()) => &rec[1..],
        _ => rec,
    }
}

/// One data record as a market event; `None` for unknown tr_ids and malformed records.
pub fn record_event(tr_id: &str, rec: &[String], now: DateTime<Utc>) -> Option<MarketEvent> {
    let id = |venue, symbol: &str| InstrumentId { venue, symbol: symbol.trim().to_string() };
    match tr_id {
        KRX_BOOK if rec.len() >= 43 => Some(MarketEvent::Book(Book {
            instrument: id(Venue::Krx, &rec[0]),
            asks: levels(&rec[3..13], &rec[23..33])?,
            bids: levels(&rec[13..23], &rec[33..43])?,
            prev_close: None,
            received_at: now,
        })),
        KRX_TRADE if rec.len() >= 13 => Some(MarketEvent::Trade(Trade {
            instrument: id(Venue::Krx, &rec[0]),
            price: num(&rec[2])?,
            qty: num(&rec[12])?,
            at: now,
        })),
        US_BOOK => {
            let r = from_symb(rec);
            (r.len() >= 14).then_some(())?;
            Some(MarketEvent::Book(Book {
                instrument: id(Venue::Us, &r[0]),
                bids: levels(&r[10..11], &r[12..13])?,
                asks: levels(&r[11..12], &r[13..14])?,
                prev_close: None,
                received_at: now,
            }))
        }
        US_TRADE => {
            let r = from_symb(rec);
            (r.len() >= 19).then_some(())?;
            Some(MarketEvent::Trade(Trade { instrument: id(Venue::Us, &r[0]), price: num(&r[10])?, qty: num(&r[18])?, at: now }))
        }
        _ => None,
    }
}

pub fn subscribe_message(approval_key: &str, tr_id: &str, tr_key: &str) -> String {
    json!({
        "header": {"approval_key": approval_key, "custtype": "P", "tr_type": "1", "content-type": "utf-8"},
        "body": {"input": {"tr_id": tr_id, "tr_key": tr_key}}
    })
    .to_string()
}

pub fn us_tr_key(excd: &str, symbol: &str) -> String {
    format!("D{excd}{symbol}")
}
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test --lib kis::ws`
Expected: 7 passed.

- [ ] **Step 5: Commit**

```bash
git add src/feed/kis
git commit -m "Parse KIS real-time frames"
git log -1 --format=%B
```

---

### Task 4: REST parsing and the two feeds

**Files:**
- Create: `src/feed/kis/rest.rs`
- Modify: `src/feed/kis/mod.rs` (feeds), `src/cli.rs` (registration), `tests/live.rs`

**Interfaces:**
- Produces:
  - `rest::{krx_book, krx_prev_close, krx_daily_stats, us_book, us_daily_stats}`, each taking `&Value` (and `now` where it builds a book).
  - `KisKrxFeed::new(Arc<KisClient>, Arc<dyn Clock>, Calendar)` and `KisUsFeed::new(...)`, both implementing `MarketFeed`. `KisKrxFeed` fills `prev_close` into every book, from both REST and WebSocket.

- [ ] **Step 1: Write the failing tests**

Create `src/feed/kis/rest.rs` holding only the tests:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;
    use rust_decimal_macros::dec;
    use serde_json::json;

    fn now() -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 9, 23, 1, 0, 0).unwrap()
    }

    #[test]
    fn krx_asking_price_levels() {
        let mut o = serde_json::Map::new();
        for i in 1..=10 {
            o.insert(format!("askp{i}"), json!((70000 + 100 * i).to_string()));
            o.insert(format!("bidp{i}"), json!((70000 - 100 * (i - 1)).to_string()));
            o.insert(format!("askp_rsqn{i}"), json!(i.to_string()));
            o.insert(format!("bidp_rsqn{i}"), json!((i * 2).to_string()));
        }
        o.insert("askp10".into(), json!("0"));
        let body = json!({"rt_cd": "0", "output1": o, "output2": {"stck_sdpr": "69800"}});
        let b = krx_book("005930", &body, now()).unwrap();
        assert_eq!(b.asks.len(), 9);
        assert_eq!(b.asks[0].price, dec!(70100));
        assert_eq!(b.bids[0].qty, dec!(2));
        assert_eq!(b.instrument.to_string(), "KRX:005930");
    }

    #[test]
    fn krx_prev_close_and_daily_stats() {
        assert_eq!(krx_prev_close(&json!({"output": {"stck_sdpr": "69800"}})).unwrap(), dec!(69800));
        assert!(krx_prev_close(&json!({"output": {}})).is_err());
        // Newest first, as KIS returns it.
        let body = json!({"output2": [
            {"stck_bsop_date": "20260923", "stck_clpr": "99", "acml_tr_pbmn": "30"},
            {"stck_bsop_date": "20260922", "stck_clpr": "110", "acml_tr_pbmn": "20"},
            {"stck_bsop_date": "20260921", "stck_clpr": "100", "acml_tr_pbmn": "10"}
        ]});
        let s = krx_daily_stats(&body).unwrap();
        assert!((s.sigma - 0.141895).abs() < 1e-5);
        assert_eq!(s.adv_notional, dec!(20));
    }

    #[test]
    fn us_one_level_book_and_daily_stats() {
        let body = json!({"output1": {"base": "185.00"}, "output2": {"pbid1": "187.12", "pask1": "187.15", "vbid1": "300", "vask1": "400"}});
        let b = us_book("AAPL", &body, now()).unwrap();
        assert_eq!((b.bids[0].price, b.asks[0].qty), (dec!(187.12), dec!(400)));
        let body = json!({"output2": [
            {"xymd": "20260923", "clos": "99", "tamt": "30"},
            {"xymd": "20260922", "clos": "110", "tamt": "20"},
            {"xymd": "20260921", "clos": "100", "tamt": "10"}
        ]});
        assert_eq!(us_daily_stats(&body).unwrap().adv_notional, dec!(20));
        assert!(us_book("AAPL", &json!({"output2": {}}), now()).unwrap().bids.is_empty());
    }
}
```

In `src/feed/kis/mod.rs` tests, add a feed-level test that `prev_close` is attached. It uses a pre-filled cache and no network:

```rust
    #[test]
    fn krx_books_get_the_cached_prev_close() {
        let dir = tempfile::tempdir().unwrap();
        let client = Arc::new(KisClient::new(cfg(dir.path())));
        let clock = Arc::new(crate::domain::ManualClock::new(Utc.with_ymd_and_hms(2026, 9, 23, 1, 0, 0).unwrap()));
        let feed = KisKrxFeed::new(client, clock, crate::venue::Calendar::default());
        let id: InstrumentId = "KRX:005930".parse().unwrap();
        feed.remember_prev_close(&id, rust_decimal_macros::dec!(69800));
        let mut book = Book { instrument: id, bids: vec![], asks: vec![], prev_close: None, received_at: Utc::now() };
        feed.attach_prev_close(&mut book);
        assert_eq!(book.prev_close, Some(rust_decimal_macros::dec!(69800)));
    }
```

(`Book`, `InstrumentId` and `Arc` come in through the feed implementation's imports below.)

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test --lib kis::`
Expected: compile errors.

- [ ] **Step 3: Implement**

Prepend this to `rest.rs`, and add `pub mod rest;` to `kis/mod.rs`:

```rust
//! KIS REST response parsing.

use anyhow::anyhow;
use chrono::{DateTime, Utc};
use rust_decimal::Decimal;
use serde_json::Value;

use crate::domain::{Book, InstrumentId, Level, Venue};
use crate::sim::DailyStats;
use crate::stats::daily_stats;

fn dec(v: &Value) -> Option<Decimal> {
    v.as_str()?.trim().parse().ok()
}

fn level(price: &Value, qty: &Value) -> Option<Level> {
    let (price, qty) = (dec(price)?, dec(qty)?);
    (price > Decimal::ZERO && qty > Decimal::ZERO).then_some(Level { price, qty })
}

/// `FHKST01010200` (inquire-asking-price-exp-ccn): 10 levels in `output1`.
pub fn krx_book(symbol: &str, body: &Value, now: DateTime<Utc>) -> anyhow::Result<Book> {
    let o = &body["output1"];
    let side = |p: &str, q: &str| (1..=10).filter_map(|i| level(&o[format!("{p}{i}")], &o[format!("{q}{i}")])).collect();
    Ok(Book {
        instrument: InstrumentId { venue: Venue::Krx, symbol: symbol.to_string() },
        asks: side("askp", "askp_rsqn"),
        bids: side("bidp", "bidp_rsqn"),
        prev_close: None,
        received_at: now,
    })
}

/// `FHKST01010100` (inquire-price): `stck_sdpr`, the reference (previous close) price.
pub fn krx_prev_close(body: &Value) -> anyhow::Result<Decimal> {
    dec(&body["output"]["stck_sdpr"]).ok_or_else(|| anyhow!("no stck_sdpr in inquire-price"))
}

fn stats_from(rows: &Value, close: &str, value: &str) -> anyhow::Result<DailyStats> {
    let rows = rows.as_array().ok_or_else(|| anyhow!("no daily rows"))?;
    // KIS returns newest first.
    let closes: Vec<Decimal> = rows.iter().rev().filter_map(|r| dec(&r[close])).collect();
    let values: Vec<Decimal> = rows.iter().rev().filter_map(|r| dec(&r[value])).collect();
    daily_stats(&closes, &values).ok_or_else(|| anyhow!("not enough daily history"))
}

/// `FHKST03010100` daily chart.
pub fn krx_daily_stats(body: &Value) -> anyhow::Result<DailyStats> {
    stats_from(&body["output2"], "stck_clpr", "acml_tr_pbmn")
}

/// `HHDFS76200100` (US inquire-asking-price): one level in `output2`.
pub fn us_book(symbol: &str, body: &Value, now: DateTime<Utc>) -> anyhow::Result<Book> {
    let o = &body["output2"];
    Ok(Book {
        instrument: InstrumentId { venue: Venue::Us, symbol: symbol.to_string() },
        bids: level(&o["pbid1"], &o["vbid1"]).into_iter().collect(),
        asks: level(&o["pask1"], &o["vask1"]).into_iter().collect(),
        prev_close: dec(&body["output1"]["base"]),
        received_at: now,
    })
}

/// `HHDFS76240000` (US dailyprice).
pub fn us_daily_stats(body: &Value) -> anyhow::Result<DailyStats> {
    stats_from(&body["output2"], "clos", "tamt")
}
```

Append the feeds to `kis/mod.rs` (above its tests module), with these imports added at the top:
`use std::collections::HashMap; use std::sync::{Arc, Mutex as StdMutex}; use async_trait::async_trait; use chrono::NaiveDate; use futures_util::SinkExt; use rust_decimal::Decimal; use tokio::sync::mpsc; use tokio_tungstenite::tungstenite::Message; use crate::domain::{Book, Clock, InstrumentId, Venue}; use crate::feed::{MarketEvent, MarketFeed, next_or_idle}; use crate::sim::DailyStats; use crate::venue::{Calendar, Instrument};`

```rust
/// Stream `subs` (tr_id, tr_key) pairs from the KIS WebSocket into `tx`, turning records into
/// events via `on_event`. Returns an error on disconnect or idle so the runner reconnects.
async fn stream_ws(
    client: &KisClient,
    subs: &[(&str, String)],
    tx: &mpsc::Sender<MarketEvent>,
    clock: &dyn Clock,
    mut on_event: impl FnMut(&mut MarketEvent),
) -> anyhow::Result<()> {
    let key = client.approval_key().await?;
    let (mut ws, _) = tokio_tungstenite::connect_async(client.config().ws_url()).await?;
    for (tr_id, tr_key) in subs {
        ws.send(Message::text(ws::subscribe_message(&key, tr_id, tr_key))).await?;
    }
    loop {
        let text = match next_or_idle(&mut ws, "kis").await?? {
            Message::Text(t) => t.as_str().to_string(),
            Message::Binary(b) => String::from_utf8_lossy(&b).into_owned(),
            Message::Close(_) => anyhow::bail!("kis websocket closed"),
            _ => continue,
        };
        match ws::parse_frame(&text) {
            Ok(ws::Frame::Ping(raw)) => ws.send(Message::text(raw)).await?,
            Ok(ws::Frame::Ack { ok: false, tr_id, msg }) => tracing::warn!(%tr_id, %msg, "KIS subscription refused"),
            Ok(ws::Frame::Data { tr_id, records }) => {
                for rec in records {
                    if let Some(mut ev) = ws::record_event(&tr_id, &rec, clock.now()) {
                        on_event(&mut ev);
                        if tx.send(ev).await.is_err() {
                            return Ok(());
                        }
                    }
                }
            }
            Ok(_) => {}
            Err(e) => tracing::debug!(error = %e, "skipping unreadable KIS frame"),
        }
    }
}

/// Outside the venue's session there is nothing to stream: wait for the next open.
async fn wait_for_session(calendar: &Calendar, clock: &dyn Clock, venue: Venue) {
    let now = clock.now();
    if calendar.is_open(venue, now) {
        return;
    }
    if let Some(open) = calendar.next_open(venue, now) {
        let wait = (open - now).to_std().unwrap_or_default();
        tracing::info!(venue = venue.tag(), %open, "market closed; KIS stream waits for the open");
        tokio::time::sleep(wait).await;
    }
}

async fn download_master(client: &reqwest::Client, file: &str) -> anyhow::Result<Vec<u8>> {
    let zip = client.get(format!("{}/{file}.zip", master::MASTER_BASE)).send().await?.error_for_status()?.bytes().await?;
    master::unzip_first(&zip)
}

pub struct KisKrxFeed {
    client: Arc<KisClient>,
    clock: Arc<dyn Clock>,
    calendar: Calendar,
    prev_close: StdMutex<HashMap<InstrumentId, (NaiveDate, Decimal)>>,
}

impl KisKrxFeed {
    pub fn new(client: Arc<KisClient>, clock: Arc<dyn Clock>, calendar: Calendar) -> Self {
        KisKrxFeed { client, clock, calendar, prev_close: StdMutex::new(HashMap::new()) }
    }

    fn today(&self) -> NaiveDate {
        self.clock.now().with_timezone(&chrono_tz::Asia::Seoul).date_naive()
    }

    pub fn remember_prev_close(&self, id: &InstrumentId, price: Decimal) {
        self.prev_close.lock().unwrap().insert(id.clone(), (self.today(), price));
    }

    pub fn attach_prev_close(&self, book: &mut Book) {
        let today = self.today();
        if let Some((day, p)) = self.prev_close.lock().unwrap().get(&book.instrument) {
            if *day == today {
                book.prev_close = Some(*p);
            }
        }
    }

    /// Today's previous close for `id`, fetched once per KST day.
    async fn ensure_prev_close(&self, id: &InstrumentId) -> anyhow::Result<()> {
        let fresh = self.prev_close.lock().unwrap().get(id).is_some_and(|(d, _)| *d == self.today());
        if !fresh {
            let body = self
                .client
                .get("/uapi/domestic-stock/v1/quotations/inquire-price", "FHKST01010100", &[("FID_COND_MRKT_DIV_CODE", "J"), ("FID_INPUT_ISCD", &id.symbol)])
                .await?;
            self.remember_prev_close(id, rest::krx_prev_close(&body)?);
        }
        Ok(())
    }
}

#[async_trait]
impl MarketFeed for KisKrxFeed {
    fn venue(&self) -> Venue {
        Venue::Krx
    }

    async fn instruments(&self) -> anyhow::Result<Vec<Instrument>> {
        let http = reqwest::Client::new();
        let mut out = master::parse_krx_master(&download_master(&http, "kospi_code.mst").await?);
        out.extend(master::parse_krx_master(&download_master(&http, "kosdaq_code.mst").await?));
        Ok(out)
    }

    async fn snapshot(&self, id: &InstrumentId) -> anyhow::Result<Book> {
        self.ensure_prev_close(id).await?;
        let body = self
            .client
            .get(
                "/uapi/domestic-stock/v1/quotations/inquire-asking-price-exp-ccn",
                "FHKST01010200",
                &[("FID_COND_MRKT_DIV_CODE", "J"), ("FID_INPUT_ISCD", &id.symbol)],
            )
            .await?;
        let mut book = rest::krx_book(&id.symbol, &body, self.clock.now())?;
        self.attach_prev_close(&mut book);
        Ok(book)
    }

    async fn daily_stats(&self, id: &InstrumentId) -> anyhow::Result<DailyStats> {
        let end = self.today();
        let start = end - chrono::Duration::days(45);
        let (s, e) = (start.format("%Y%m%d").to_string(), end.format("%Y%m%d").to_string());
        let body = self
            .client
            .get(
                "/uapi/domestic-stock/v1/quotations/inquire-daily-itemchartprice",
                "FHKST03010100",
                &[
                    ("FID_COND_MRKT_DIV_CODE", "J"),
                    ("FID_INPUT_ISCD", &id.symbol),
                    ("FID_INPUT_DATE_1", &s),
                    ("FID_INPUT_DATE_2", &e),
                    ("FID_PERIOD_DIV_CODE", "D"),
                    ("FID_ORG_ADJ_PRC", "0"),
                ],
            )
            .await?;
        rest::krx_daily_stats(&body)
    }

    async fn stream(&self, ids: &[InstrumentId], tx: &mpsc::Sender<MarketEvent>) -> anyhow::Result<()> {
        wait_for_session(&self.calendar, self.clock.as_ref(), Venue::Krx).await;
        for id in ids {
            self.ensure_prev_close(id).await?;
        }
        let subs: Vec<(&str, String)> =
            ids.iter().flat_map(|i| [(ws::KRX_BOOK, i.symbol.clone()), (ws::KRX_TRADE, i.symbol.clone())]).collect();
        stream_ws(&self.client, &subs, tx, self.clock.as_ref(), |ev| {
            if let MarketEvent::Book(b) = ev {
                self.attach_prev_close(b);
            }
        })
        .await
    }
}

pub struct KisUsFeed {
    client: Arc<KisClient>,
    clock: Arc<dyn Clock>,
    calendar: Calendar,
    exchanges: StdMutex<HashMap<String, String>>,
}

impl KisUsFeed {
    pub fn new(client: Arc<KisClient>, clock: Arc<dyn Clock>, calendar: Calendar) -> Self {
        KisUsFeed { client, clock, calendar, exchanges: StdMutex::new(HashMap::new()) }
    }

    fn excd(&self, id: &InstrumentId) -> anyhow::Result<String> {
        self.exchanges.lock().unwrap().get(&id.symbol).cloned().ok_or_else(|| anyhow!("no exchange known for {id}"))
    }
}

#[async_trait]
impl MarketFeed for KisUsFeed {
    fn venue(&self) -> Venue {
        Venue::Us
    }

    async fn instruments(&self) -> anyhow::Result<Vec<Instrument>> {
        let http = reqwest::Client::new();
        let mut out = Vec::new();
        let mut map = HashMap::new();
        for file in ["nasmst.cod", "nysmst.cod", "amsmst.cod"] {
            for (inst, excd) in master::parse_us_master(&download_master(&http, file).await?) {
                if map.insert(inst.id.symbol.clone(), excd).is_none() {
                    out.push(inst);
                }
            }
        }
        *self.exchanges.lock().unwrap() = map;
        Ok(out)
    }

    async fn snapshot(&self, id: &InstrumentId) -> anyhow::Result<Book> {
        let excd = self.excd(id)?;
        let body = self
            .client
            .get("/uapi/overseas-price/v1/quotations/inquire-asking-price", "HHDFS76200100", &[("AUTH", ""), ("EXCD", &excd), ("SYMB", &id.symbol)])
            .await?;
        rest::us_book(&id.symbol, &body, self.clock.now())
    }

    async fn daily_stats(&self, id: &InstrumentId) -> anyhow::Result<DailyStats> {
        let excd = self.excd(id)?;
        let body = self
            .client
            .get(
                "/uapi/overseas-price/v1/quotations/dailyprice",
                "HHDFS76240000",
                &[("AUTH", ""), ("EXCD", &excd), ("SYMB", &id.symbol), ("GUBN", "0"), ("BYMD", ""), ("MODP", "1")],
            )
            .await?;
        rest::us_daily_stats(&body)
    }

    async fn stream(&self, ids: &[InstrumentId], tx: &mpsc::Sender<MarketEvent>) -> anyhow::Result<()> {
        wait_for_session(&self.calendar, self.clock.as_ref(), Venue::Us).await;
        let mut subs = Vec::new();
        for id in ids {
            let key = ws::us_tr_key(&self.excd(id)?, &id.symbol);
            subs.push((ws::US_BOOK, key.clone()));
            subs.push((ws::US_TRADE, key));
        }
        stream_ws(&self.client, &subs, tx, self.clock.as_ref(), |_| {}).await
    }
}
```

In `src/cli.rs` `serve`, change `feeds` to be mutable. Then, after the crypto feeds and before the loop that spawns them, add:

```rust
    match crate::feed::kis::KisConfig::from_env() {
        Some(cfg) => {
            let client = Arc::new(crate::feed::kis::KisClient::new(cfg));
            let calendar = Calendar::from_toml(include_str!("../holidays.toml"))?;
            // 41 real-time registrations per appkey; book + trade = 2 per instrument.
            feeds.push((Arc::new(crate::feed::kis::KisKrxFeed::new(client.clone(), clock.clone(), calendar.clone())), 10));
            feeds.push((Arc::new(crate::feed::kis::KisUsFeed::new(client, clock.clone(), calendar)), 10));
        }
        None => tracing::info!("KIS_APP_KEY/KIS_APP_SECRET not set; KRX and US stocks are disabled"),
    }
```

Add these lines to the CLI `USAGE` ENVIRONMENT list:
```
  KIS_APP_KEY, KIS_APP_SECRET  KIS Open API keys (enable KRX and US stocks)
  KIS_ENV               `mock` for KIS mock-trading hosts (default: real)
  ATRADER_STATE_DIR     where the KIS token is cached (default: ~/.local/state/atrader)
```

Append this to `tests/live.rs`. It skips unless the KIS environment variables are set:

```rust
fn kis() -> Option<Arc<atrader::feed::kis::KisClient>> {
    atrader::feed::kis::KisConfig::from_env().map(|c| Arc::new(atrader::feed::kis::KisClient::new(c)))
}

#[tokio::test]
#[ignore]
async fn kis_krx_live() {
    let Some(client) = kis() else { return eprintln!("KIS keys not set; skipped") };
    let cal = atrader::venue::Calendar::from_toml(include_str!("../holidays.toml")).unwrap();
    let feed = Arc::new(atrader::feed::kis::KisKrxFeed::new(client, Arc::new(SystemClock), cal));
    let id: InstrumentId = "KRX:005930".parse().unwrap();
    assert!(feed.instruments().await.unwrap().iter().any(|i| i.id == id));
    let book = feed.snapshot(&id).await.unwrap();
    assert!(book.prev_close.is_some() && !book.asks.is_empty(), "{book:?}");
    assert!(feed.daily_stats(&id).await.unwrap().sigma > 0.0);
}

#[tokio::test]
#[ignore]
async fn kis_us_live() {
    let Some(client) = kis() else { return eprintln!("KIS keys not set; skipped") };
    let cal = atrader::venue::Calendar::from_toml(include_str!("../holidays.toml")).unwrap();
    let feed = Arc::new(atrader::feed::kis::KisUsFeed::new(client, Arc::new(SystemClock), cal));
    let id: InstrumentId = "US:AAPL".parse().unwrap();
    assert!(feed.instruments().await.unwrap().iter().any(|i| i.id == id));
    let book = feed.snapshot(&id).await.unwrap();
    assert!(!book.bids.is_empty() || !book.asks.is_empty(), "{book:?}");
    assert!(feed.daily_stats(&id).await.unwrap().sigma > 0.0);
}
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test` (with `DATABASE_URL`)
Expected: everything passes. The KIS live tests are ignored.

Run: `cargo test --test live -- --ignored`
Expected: the Upbit, Binance and FX tests pass. Without keys, the KIS tests print "skipped" and pass.

- [ ] **Step 5: Smoke**

Run `serve --no-zyris` for 20 s without KIS keys.
Expected: the log contains "KRX and US stocks are disabled" and "state restored", and the process exits 0 on SIGTERM.

- [ ] **Step 6: Commit**

```bash
git add src/feed/kis src/cli.rs tests/live.rs
git commit -m "Add KIS feeds for KRX and US stocks"
git log -1 --format=%B
```
