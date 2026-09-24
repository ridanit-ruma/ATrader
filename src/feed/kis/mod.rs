//! Korea Investment & Securities (KIS) Open API: KRX and US stock market data.

pub mod master;
pub mod rest;
pub mod ws;

use std::collections::HashMap;
use std::sync::{Arc, Mutex as StdMutex};

use async_trait::async_trait;
use chrono::NaiveDate;
use futures_util::SinkExt;
use rust_decimal::Decimal;
use tokio::sync::mpsc;
use tokio_tungstenite::tungstenite::Message;

use crate::domain::{Book, Clock, InstrumentId, Venue};
use crate::feed::{MarketEvent, MarketFeed, next_or_idle};
use crate::sim::DailyStats;
use crate::venue::{Calendar, Instrument};
use crate::candles::{Candle, Interval};
use crate::screen::{Ranking, ScreenRow};
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
    /// `fingerprint` of the appkey that issued it, so a rotated key never reuses it.
    pub key: String,
}

/// A stable, non-reversible tag for an appkey (FNV-1a 64).
pub fn fingerprint(app_key: &str) -> String {
    let hash = app_key.bytes().fold(0xcbf29ce484222325u64, |h, b| (h ^ u64::from(b)).wrapping_mul(0x100000001b3));
    format!("{hash:016x}")
}

impl CachedToken {
    /// A cached token for this appkey still good for at least 10 minutes.
    pub fn load(path: &Path, now: DateTime<Utc>, key: &str) -> Option<CachedToken> {
        let t: CachedToken = serde_json::from_slice(&std::fs::read(path).ok()?).ok()?;
        (t.key == key && t.expires_at - now > chrono::Duration::minutes(10)).then_some(t)
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
pub fn parse_token_response(body: &Value, app_key: &str) -> anyhow::Result<CachedToken> {
    let token = body["access_token"].as_str().ok_or_else(|| anyhow!("token response without access_token"))?;
    let expiry = body["access_token_token_expired"].as_str().ok_or_else(|| anyhow!("token response without expiry"))?;
    let local = NaiveDateTime::parse_from_str(expiry, "%Y-%m-%d %H:%M:%S")?;
    let expires_at = chrono_tz::Asia::Seoul
        .from_local_datetime(&local)
        .single()
        .ok_or_else(|| anyhow!("ambiguous expiry {expiry}"))?
        .with_timezone(&Utc);
    Ok(CachedToken { token: token.to_string(), expires_at, key: fingerprint(app_key) })
}

/// The token was revoked or expired early (e.g. re-issued elsewhere).
pub fn is_token_error(body: &Value) -> bool {
    matches!(body["msg_cd"].as_str(), Some("EGW00121" | "EGW00123"))
}

/// KIS rolls the reference (previous close) price over before the session; values fetched
/// earlier in the KST morning may still be yesterday's.
pub fn prev_close_cacheable(now: DateTime<Utc>) -> bool {
    let t = now.with_timezone(&chrono_tz::Asia::Seoul).time();
    t >= chrono::NaiveTime::from_hms_opt(8, 30, 0).expect("valid time")
}

/// KIS PINGPONG frames are answered with a WebSocket pong carrying the same payload.
pub fn ping_reply(raw: String) -> Message {
    Message::Pong(raw.into_bytes().into())
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
        if let Some(t) = CachedToken::load(&self.token_path(), now, &fingerprint(&self.cfg.app_key)) {
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
        let t = parse_token_response(&body, &self.cfg.app_key).map_err(|e| anyhow!("KIS token issuance failed: {e} ({})", body["error_description"].as_str().unwrap_or("")))?;
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
        if is_token_error(&body) {
            // Drop the dead token so the next call issues a fresh one.
            self.token.lock().await.take();
            let _ = std::fs::remove_file(self.token_path());
        }
        check_rt(&body, tr_id)?;
        Ok(body)
    }
}

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
            Ok(ws::Frame::Ping(raw)) => ws.send(ping_reply(raw)).await?,
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
    /// Today's previous close for `id`, fetched once per KST day (always when `force`). Values
    /// fetched before the morning rollover are not cached.
    async fn ensure_prev_close(&self, id: &InstrumentId, force: bool) -> anyhow::Result<()> {
        let fresh = self.prev_close.lock().unwrap().get(id).is_some_and(|(d, _)| *d == self.today());
        if force || !fresh {
            let body = self
                .client
                .get("/uapi/domestic-stock/v1/quotations/inquire-price", "FHKST01010100", &[("FID_COND_MRKT_DIV_CODE", "J"), ("FID_INPUT_ISCD", &id.symbol)])
                .await?;
            let price = rest::krx_prev_close(&body)?;
            if prev_close_cacheable(self.clock.now()) {
                self.remember_prev_close(id, price);
            }
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
        let http = reqwest::Client::builder().timeout(StdDuration::from_secs(60)).build()?;
        let mut out = master::parse_krx_master(&download_master(&http, "kospi_code.mst").await?);
        out.extend(master::parse_krx_master(&download_master(&http, "kosdaq_code.mst").await?));
        Ok(out)
    }

    async fn snapshot(&self, id: &InstrumentId) -> anyhow::Result<Book> {
        self.ensure_prev_close(id, false).await?;
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

    async fn candles(&self, id: &InstrumentId, interval: Interval, limit: usize) -> anyhow::Result<Vec<Candle>> {
        let period = match interval {
            Interval::D1 => "D",
            Interval::W1 => "W",
            other => anyhow::bail!("unsupported: KRX {} candles come from stored bars", other.code()),
        };
        let end = self.today();
        let days = if interval == Interval::W1 { 7 * limit as i64 } else { (limit as i64 * 7) / 5 + 10 };
        let start = end - chrono::Duration::days(days);
        let (s, e) = (start.format("%Y%m%d").to_string(), end.format("%Y%m%d").to_string());
        let body = self
            .client
            .get(
                "/uapi/domestic-stock/v1/quotations/inquire-daily-itemchartprice",
                "FHKST03010100",
                &[("FID_COND_MRKT_DIV_CODE", "J"), ("FID_INPUT_ISCD", &id.symbol), ("FID_INPUT_DATE_1", &s), ("FID_INPUT_DATE_2", &e), ("FID_PERIOD_DIV_CODE", period), ("FID_ORG_ADJ_PRC", "0")],
            )
            .await?;
        let mut c = rest::krx_candles(&body)?;
        let skip = c.len().saturating_sub(limit);
        Ok(c.split_off(skip))
    }

    // ponytail: KRX losers sort code "0001" and the US ranking parameters are unverified without KIS keys.
    async fn screen(&self, ranking: Ranking, limit: usize) -> anyhow::Result<Vec<ScreenRow>> {
        let body = match ranking {
            Ranking::Gainers | Ranking::Losers => {
                let sort = if ranking == Ranking::Gainers { "0000" } else { "0001" };
                self.client
                    .get(
                        "/uapi/domestic-stock/v1/ranking/fluctuation",
                        "FHPST01700000",
                        &[
                            ("fid_cond_mrkt_div_code", "J"),
                            ("fid_cond_scr_div_code", "20170"),
                            ("fid_input_iscd", "0000"),
                            ("fid_rank_sort_cls_code", sort),
                            ("fid_input_cnt_1", "0"),
                            ("fid_prc_cls_code", "0"),
                            ("fid_input_price_1", ""),
                            ("fid_input_price_2", ""),
                            ("fid_vol_cnt", ""),
                            ("fid_trgt_cls_code", "0"),
                            ("fid_trgt_exls_cls_code", "0"),
                            ("fid_div_cls_code", "0"),
                            ("fid_rsfl_rate1", ""),
                            ("fid_rsfl_rate2", ""),
                        ],
                    )
                    .await?
            }
            Ranking::Volume | Ranking::Value => {
                let by = if ranking == Ranking::Volume { "0" } else { "3" };
                self.client
                    .get(
                        "/uapi/domestic-stock/v1/quotations/volume-rank",
                        "FHPST01710000",
                        &[
                            ("FID_COND_MRKT_DIV_CODE", "J"),
                            ("FID_COND_SCR_DIV_CODE", "20171"),
                            ("FID_INPUT_ISCD", "0000"),
                            ("FID_DIV_CLS_CODE", "0"),
                            ("FID_BLNG_CLS_CODE", by),
                            ("FID_TRGT_CLS_CODE", "111111111"),
                            ("FID_TRGT_EXLS_CLS_CODE", "0000000000"),
                            ("FID_INPUT_PRICE_1", ""),
                            ("FID_INPUT_PRICE_2", ""),
                            ("FID_VOL_CNT", ""),
                            ("FID_INPUT_DATE_1", ""),
                        ],
                    )
                    .await?
            }
        };
        Ok(crate::screen::rank(rest::krx_rank_rows(&body), ranking, limit))
    }

    async fn stream(&self, ids: &[InstrumentId], tx: &mpsc::Sender<MarketEvent>) -> anyhow::Result<()> {
        wait_for_session(&self.calendar, self.clock.as_ref(), Venue::Krx).await;
        for id in ids {
            self.ensure_prev_close(id, true).await?;
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
        let http = reqwest::Client::builder().timeout(StdDuration::from_secs(60)).build()?;
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

    async fn candles(&self, id: &InstrumentId, interval: Interval, limit: usize) -> anyhow::Result<Vec<Candle>> {
        let gubn = match interval {
            Interval::D1 => "0",
            Interval::W1 => "1",
            other => anyhow::bail!("unsupported: US {} candles come from stored bars", other.code()),
        };
        let excd = self.excd(id)?;
        let body = self
            .client
            .get("/uapi/overseas-price/v1/quotations/dailyprice", "HHDFS76240000", &[("AUTH", ""), ("EXCD", &excd), ("SYMB", &id.symbol), ("GUBN", gubn), ("BYMD", ""), ("MODP", "1")])
            .await?;
        let mut c = rest::us_candles(&body)?;
        let skip = c.len().saturating_sub(limit);
        Ok(c.split_off(skip))
    }

    async fn screen(&self, ranking: Ranking, limit: usize) -> anyhow::Result<Vec<ScreenRow>> {
        let (path, tr_id, extra): (&str, &str, &[(&str, &str)]) = match ranking {
            Ranking::Gainers => ("/uapi/overseas-stock/v1/ranking/updown-rate", "HHDFS76290000", &[("GUBN", "1")]),
            Ranking::Losers => ("/uapi/overseas-stock/v1/ranking/updown-rate", "HHDFS76290000", &[("GUBN", "0")]),
            Ranking::Volume => ("/uapi/overseas-stock/v1/ranking/trade-vol", "HHDFS76310010", &[("PRC1", ""), ("PRC2", "")]),
            Ranking::Value => ("/uapi/overseas-stock/v1/ranking/trade-pbmn", "HHDFS76320010", &[("PRC1", ""), ("PRC2", "")]),
        };
        let mut rows = Vec::new();
        for excd in ["NAS", "NYS"] {
            let mut q: Vec<(&str, &str)> = vec![("EXCD", excd), ("NDAY", "0"), ("VOL_RANG", "0"), ("AUTH", ""), ("KEYB", "")];
            q.extend_from_slice(extra);
            rows.extend(rest::us_rank_rows(&self.client.get(path, tr_id, &q).await?));
        }
        Ok(crate::screen::rank(rows, ranking, limit))
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
        let t = CachedToken { token: "tok".into(), expires_at: now + chrono::Duration::hours(20), key: fingerprint("APPKEY123") };
        t.save(&path).unwrap();
        assert_eq!(CachedToken::load(&path, now, &fingerprint("APPKEY123")), Some(t.clone()));
        assert_eq!(CachedToken::load(&path, now, &fingerprint("OTHERKEY")), None); // rotated appkey
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(std::fs::metadata(&path).unwrap().permissions().mode() & 0o777, 0o600);
    }

    #[test]
    fn nearly_expired_or_garbage_tokens_are_not_reused() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("kis_token.json");
        let now = Utc.with_ymd_and_hms(2026, 9, 24, 0, 0, 0).unwrap();
        CachedToken { token: "tok".into(), expires_at: now + chrono::Duration::minutes(5), key: fingerprint("k") }.save(&path).unwrap();
        assert_eq!(CachedToken::load(&path, now, &fingerprint("k")), None);
        std::fs::write(&path, "{").unwrap();
        assert_eq!(CachedToken::load(&path, now, &fingerprint("k")), None);
        assert_eq!(CachedToken::load(&dir.path().join("missing"), now, &fingerprint("k")), None);
    }

    #[test]
    fn parses_token_response_expiry_in_kst() {
        let body = serde_json::json!({"access_token": "abc", "token_type": "Bearer", "expires_in": 86400, "access_token_token_expired": "2026-09-25 09:00:00"});
        let t = parse_token_response(&body, "k").unwrap();
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

    #[test]
    fn invalid_token_codes_are_recognised() {
        assert!(is_token_error(&serde_json::json!({"rt_cd": "1", "msg_cd": "EGW00123", "msg1": "기간이 만료된 token 입니다."})));
        assert!(is_token_error(&serde_json::json!({"rt_cd": "1", "msg_cd": "EGW00121"})));
        assert!(!is_token_error(&serde_json::json!({"rt_cd": "1", "msg_cd": "EGW00201"})));
    }

    #[test]
    fn prev_close_is_cached_only_after_the_morning_rollover() {
        let kst = |h, m| chrono_tz::Asia::Seoul.with_ymd_and_hms(2026, 9, 23, h, m, 0).unwrap().with_timezone(&Utc);
        assert!(!prev_close_cacheable(kst(0, 30)));
        assert!(!prev_close_cacheable(kst(8, 29)));
        assert!(prev_close_cacheable(kst(8, 30)));
        assert!(prev_close_cacheable(kst(15, 0)));
    }

    #[test]
    fn pingpong_is_answered_with_a_pong_frame() {
        assert_eq!(ping_reply("{\"header\":{\"tr_id\":\"PINGPONG\"}}".into()), Message::Pong("{\"header\":{\"tr_id\":\"PINGPONG\"}}".as_bytes().to_vec().into()));
    }
}
