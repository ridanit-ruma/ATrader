//! The dashboard's HTTP side: auth, JSON API, live stream, and the embedded SPA.

pub mod auth;

use std::collections::{BTreeMap, HashMap};
use std::convert::Infallible;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, LazyLock, Mutex};

use axum::extract::{ConnectInfo, Path, Query, Request, State};
use axum::http::{HeaderMap, HeaderValue, Method, StatusCode, header};
use axum::middleware::{self, Next};
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::{IntoResponse, Response};
use axum::routing::{delete, get, post, put};
use axum::{Extension, Json, Router};
use chrono::{DateTime, Duration, TimeZone, Utc};
use futures_util::{Stream, StreamExt, stream};
use rust_decimal::Decimal;
use serde::Deserialize;
use serde_json::{Value, json};
use tokio::sync::broadcast;

use crate::app::{App, value_account};
use crate::domain::Currency;
use crate::market::BusEvent;
use crate::store::AccountRow;
use crate::tools::{FillView, OrderView, Trader, TraderTools};
use auth::{AuthStore, Limiter, Session, User};

const COOKIE: &str = "atrader_session";

#[derive(Clone)]
pub struct WebState {
    pub app: Arc<App>,
    pub auth: Arc<AuthStore>,
    pub limiter: Arc<Mutex<Limiter>>,
    pub bus: broadcast::Sender<BusEvent>,
    /// `Secure` cookie flag; false only for tests over plain HTTP.
    pub cookie_secure: bool,
    /// Set by the zyris link's `on_connect`.
    pub zyris_connected: Arc<AtomicBool>,
    /// Signalled when a saved setting needs a restart to take effect (see `cli::RESTART_EXIT_CODE`).
    pub restart: Arc<tokio::sync::Notify>,
    /// Where dashboard-entered keys are stored (`<dir>/settings/NAME`).
    pub settings_dir: std::path::PathBuf,
    pub enrollment: Arc<Mutex<EnrollView>>,
    /// The live Attacca connection (set by the zyris link), for listing conversations.
    pub attacca: crate::alerts::deliver::ConnSlot,
}

/// Where the dashboard's Attacca enrollment stands.
#[derive(Debug, Clone, serde::Serialize)]
pub struct EnrollView {
    /// `idle`, `pending`, `granted`, `denied`, `expired` or `error`.
    pub status: &'static str,
    pub user_code: Option<String>,
    pub verification_uri: Option<String>,
    pub expires_at: Option<DateTime<Utc>>,
    pub message: Option<String>,
}

impl Default for EnrollView {
    fn default() -> Self {
        EnrollView { status: "idle", user_code: None, verification_uri: None, expires_at: None, message: None }
    }
}

impl WebState {
    pub fn new(app: Arc<App>, auth: AuthStore, cookie_secure: bool) -> Self {
        let (bus, _) = broadcast::channel(16);
        WebState {
            app,
            auth: Arc::new(auth),
            limiter: Arc::default(),
            bus,
            cookie_secure,
            zyris_connected: Arc::default(),
            restart: Arc::default(),
            settings_dir: crate::settings::state_dir(),
            enrollment: Arc::default(),
            attacca: Default::default(),
        }
    }

    pub fn with_bus(mut self, bus: broadcast::Sender<BusEvent>) -> Self {
        self.bus = bus;
        self
    }

    fn tools(&self) -> TraderTools {
        TraderTools::new(self.app.clone())
    }
}

pub struct ApiError(StatusCode, &'static str, String);

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (self.0, Json(json!({"error": self.1, "message": self.2}))).into_response()
    }
}

type ApiResult<T = Value> = Result<Json<T>, ApiError>;

fn internal(e: impl std::fmt::Display) -> ApiError {
    tracing::error!(error = %e, "api error");
    ApiError(StatusCode::INTERNAL_SERVER_ERROR, "internal", "something went wrong".into())
}

fn bad_request(msg: impl Into<String>) -> ApiError {
    ApiError(StatusCode::BAD_REQUEST, "bad_request", msg.into())
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

fn to_json(v: impl serde::Serialize) -> Result<Value, ApiError> {
    serde_json::to_value(v).map_err(internal)
}

/// The caller's address. `X-Forwarded-For` is trusted only from loopback (`tailscale serve` proxies
/// from there), so a direct client cannot pick its own rate-limit key.
fn client_ip(headers: &HeaderMap, peer: Option<SocketAddr>) -> String {
    let forwarded = || headers.get("x-forwarded-for")?.to_str().ok()?.split(',').next().map(|s| s.trim().to_string());
    match peer {
        Some(p) if !p.ip().is_loopback() => p.ip().to_string(),
        _ => forwarded().or_else(|| peer.map(|p| p.ip().to_string())).unwrap_or_else(|| "unknown".into()),
    }
}

fn session_cookie(headers: &HeaderMap) -> Option<String> {
    headers
        .get_all(header::COOKIE)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|v| v.split(';'))
        .find_map(|kv| kv.trim().strip_prefix("atrader_session=").map(str::to_string))
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
    h.insert(
        "content-security-policy",
        HeaderValue::from_static("default-src 'self'; img-src 'self' data:; style-src 'self' 'unsafe-inline'; frame-ancestors 'none'"),
    );
    h.insert("x-frame-options", HeaderValue::from_static("DENY"));
    h.insert("x-content-type-options", HeaderValue::from_static("nosniff"));
    h.insert("referrer-policy", HeaderValue::from_static("no-referrer"));
    if is_api {
        h.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    }
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

/// Start a session and return its `Set-Cookie` value.
async fn issue_session(s: &WebState, user_id: i64, pending: bool, headers: &HeaderMap, ip: &str) -> Result<String, ApiError> {
    let token = auth::new_token();
    let ua = headers.get(header::USER_AGENT).and_then(|v| v.to_str().ok()).unwrap_or("");
    s.auth.create_session(user_id, &auth::token_hash(&token), pending, ip, ua, Utc::now()).await.map_err(internal)?;
    let secure = if s.cookie_secure { "; Secure" } else { "" };
    Ok(format!("{COOKIE}={token}; HttpOnly{secure}; SameSite=Strict; Path=/; Max-Age={}", auth::ABSOLUTE.num_seconds()))
}

fn with_cookie(cookie: String, body: Value) -> Response {
    ([(header::SET_COOKIE, cookie)], Json(body)).into_response()
}

type Peer = Option<Extension<ConnectInfo<SocketAddr>>>;

fn peer_ip(headers: &HeaderMap, peer: Peer) -> String {
    client_ip(headers, peer.map(|p| p.0.0))
}

// ---------------------------------------------------------------------------------------------
// Auth

#[derive(Deserialize)]
struct LoginBody {
    username: String,
    password: String,
    #[serde(default)]
    code: Option<String>,
}

/// Verified against when the username is unknown, so both cases cost one Argon2 run.
static DUMMY_HASH: LazyLock<String> = LazyLock::new(|| auth::hash_password("not a real password"));

async fn login(State(s): State<WebState>, peer: Peer, headers: HeaderMap, Json(b): Json<LoginBody>) -> Result<Response, ApiError> {
    let ip = peer_ip(&headers, peer);
    let name: String = b.username.chars().take(64).collect();
    let (ukey, ikey) = (format!("u:{name}"), format!("ip:{ip}"));
    let now = Utc::now();
    {
        let l = s.limiter.lock().unwrap();
        if let Err(wait) = l.check(&ukey, now).and(l.check(&ikey, now)) {
            return Err(ApiError(StatusCode::TOO_MANY_REQUESTS, "locked", format!("too many attempts; try again in {wait} s")));
        }
    }
    let user = s.auth.user_by_name(&b.username).await.map_err(internal)?;
    let hash = user.as_ref().map_or(DUMMY_HASH.as_str(), |u| u.password_hash.as_str());
    let password_ok = auth::verify_password(&b.password, hash) && user.is_some();
    let second_ok = match (&user, password_ok) {
        (Some(u), true) if u.totp_enabled => {
            let code = b.code.as_deref().unwrap_or_default().trim();
            match u.totp_secret.as_deref() {
                Some(sec) if s.auth.accept_totp(u.id, sec, code, now).await.map_err(internal)? => true,
                _ => s.auth.use_recovery_code(u.id, code).await.map_err(internal)?,
            }
        }
        (Some(_), true) => true, // not enrolled yet: the session starts pending
        _ => false,
    };
    let Some(user) = user.filter(|_| password_ok && second_ok) else {
        {
            let mut l = s.limiter.lock().unwrap();
            l.fail(&ukey, now);
            l.fail(&ikey, now);
        }
        let _ = s.auth.audit(None, "login_failed", &name, &ip).await;
        return Err(ApiError(StatusCode::UNAUTHORIZED, "login_failed", "wrong username, password or code".into()));
    };
    {
        let mut l = s.limiter.lock().unwrap();
        l.succeed(&ukey);
        l.succeed(&ikey);
    }
    let cookie = issue_session(&s, user.id, !user.totp_enabled, &headers, &ip).await?;
    let _ = s.auth.audit(Some(user.id), "login", "", &ip).await;
    Ok(with_cookie(cookie, json!({"mfa_pending": !user.totp_enabled})))
}

async fn logout(State(s): State<WebState>, peer: Peer, headers: HeaderMap) -> Result<Response, ApiError> {
    let (user, session) = authed(&s, &headers, true).await?;
    s.auth.delete_session(user.id, &session.id_hash).await.map_err(internal)?;
    let _ = s.auth.audit(Some(user.id), "logout", "", &peer_ip(&headers, peer)).await;
    let secure = if s.cookie_secure { "; Secure" } else { "" };
    Ok(with_cookie(format!("{COOKIE}=; HttpOnly{secure}; SameSite=Strict; Path=/; Max-Age=0"), json!({})))
}

async fn me(State(s): State<WebState>, headers: HeaderMap) -> ApiResult {
    let (user, session) = authed(&s, &headers, true).await?;
    Ok(Json(json!({"username": user.username, "totp_enabled": user.totp_enabled, "mfa_pending": session.mfa_pending})))
}

async fn totp_setup(State(s): State<WebState>, headers: HeaderMap) -> ApiResult {
    let (user, _) = authed(&s, &headers, true).await?;
    // Re-enrolling would switch the second factor off; that goes through `atrader user reset-2fa`.
    if user.totp_enabled {
        return Err(ApiError(StatusCode::CONFLICT, "already_enrolled", "two-factor is already on; reset it with `atrader user reset-2fa`".into()));
    }
    let secret = auth::new_totp_secret();
    s.auth.set_totp(user.id, Some(&secret), false).await.map_err(internal)?;
    Ok(Json(json!({"otpauth_url": auth::totp_url(&secret, &user.username), "secret": secret})))
}

#[derive(Deserialize)]
struct CodeBody {
    code: String,
}

async fn totp_enable(State(s): State<WebState>, peer: Peer, headers: HeaderMap, Json(b): Json<CodeBody>) -> Result<Response, ApiError> {
    let (user, session) = authed(&s, &headers, true).await?;
    let secret = match (&user.totp_secret, user.totp_enabled) {
        (Some(sec), false) => sec.clone(),
        (_, true) => return Err(ApiError(StatusCode::CONFLICT, "already_enrolled", "two-factor is already on".into())),
        (None, _) => return Err(bad_request("call /api/auth/totp/setup first")),
    };
    if !s.auth.accept_totp(user.id, &secret, b.code.trim(), Utc::now()).await.map_err(internal)? {
        return Err(bad_request("that code is not valid; check the authenticator's clock"));
    }
    s.auth.set_totp(user.id, Some(&secret), true).await.map_err(internal)?;
    let codes = auth::new_recovery_codes();
    s.auth.save_recovery_codes(user.id, &codes).await.map_err(internal)?;
    // Rotate: the token issued before the second factor never becomes a full session.
    s.auth.delete_session(user.id, &session.id_hash).await.map_err(internal)?;
    let ip = peer_ip(&headers, peer);
    let cookie = issue_session(&s, user.id, false, &headers, &ip).await?;
    let _ = s.auth.audit(Some(user.id), "totp_enabled", "", &ip).await;
    Ok(with_cookie(cookie, json!({"recovery_codes": codes})))
}

#[derive(Deserialize)]
struct PasswordBody {
    current: String,
    new: String,
}

async fn password(State(s): State<WebState>, peer: Peer, headers: HeaderMap, Json(b): Json<PasswordBody>) -> Result<Response, ApiError> {
    let (user, _) = authed(&s, &headers, false).await?;
    let ip = peer_ip(&headers, peer);
    let (key, now) = (format!("u:{}", user.username), Utc::now());
    if let Err(wait) = s.limiter.lock().unwrap().check(&key, now) {
        return Err(ApiError(StatusCode::TOO_MANY_REQUESTS, "locked", format!("too many attempts; try again in {wait} s")));
    }
    if !auth::verify_password(&b.current, &user.password_hash) {
        s.limiter.lock().unwrap().fail(&key, now);
        return Err(ApiError(StatusCode::UNAUTHORIZED, "wrong_password", "the current password is wrong".into()));
    }
    if b.new.chars().count() < 12 {
        return Err(bad_request("the new password needs at least 12 characters"));
    }
    s.auth.set_password(user.id, &auth::hash_password(&b.new)).await.map_err(internal)?;
    s.auth.delete_user_sessions(user.id).await.map_err(internal)?;
    let cookie = issue_session(&s, user.id, false, &headers, &ip).await?;
    let _ = s.auth.audit(Some(user.id), "password_changed", "", &ip).await;
    Ok(with_cookie(cookie, json!({})))
}

async fn sessions(State(s): State<WebState>, headers: HeaderMap) -> ApiResult {
    let (user, current) = authed(&s, &headers, false).await?;
    let rows = s.auth.sessions(user.id).await.map_err(internal)?;
    Ok(Json(Value::Array(
        rows.into_iter()
            .map(|(id, ip, ua, created, seen)| json!({"id": id, "ip": ip, "user_agent": ua, "created_at": created, "last_seen": seen, "current": id == current.id_hash}))
            .collect(),
    )))
}

async fn revoke(State(s): State<WebState>, peer: Peer, headers: HeaderMap, Path(id): Path<String>) -> ApiResult {
    let (user, _) = authed(&s, &headers, false).await?;
    if !s.auth.delete_session(user.id, &id).await.map_err(internal)? {
        return Err(ApiError(StatusCode::NOT_FOUND, "not_found", "no such session".into()));
    }
    let _ = s.auth.audit(Some(user.id), "session_revoked", "", &peer_ip(&headers, peer)).await;
    Ok(Json(json!({})))
}

async fn audit(State(s): State<WebState>, headers: HeaderMap) -> ApiResult {
    authed(&s, &headers, false).await?;
    let rows = s.auth.audit_log(500).await.map_err(internal)?;
    Ok(Json(Value::Array(rows.into_iter().map(|(at, action, detail, ip)| json!({"at": at, "action": action, "detail": detail, "ip": ip})).collect())))
}

// ---------------------------------------------------------------------------------------------
// Accounts

fn row(s: &WebState, id: &str) -> Result<AccountRow, ApiError> {
    s.app.account(id).ok_or_else(|| ApiError(StatusCode::NOT_FOUND, "not_found", format!("no account {id:?}")))
}

fn parse_cash(cash: &HashMap<String, Decimal>) -> Result<Vec<(Currency, Decimal)>, ApiError> {
    if cash.is_empty() {
        return Err(bad_request("cash needs at least one currency"));
    }
    cash.iter()
        .map(|(c, v)| {
            let cur = Currency::from_code(c).ok_or_else(|| bad_request(format!("unknown currency {c:?}; use KRW, USD or USDT")))?;
            if v.is_sign_negative() {
                return Err(bad_request(format!("{c} cash cannot be negative")));
            }
            Ok((cur, *v))
        })
        .collect()
}

fn valid_account_id(id: &str) -> bool {
    (1..=32).contains(&id.len()) && id.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_' || c == '-')
}

#[derive(Deserialize)]
struct NewAccount {
    /// Left out by the dashboard: one is made from the name.
    #[serde(default)]
    id: Option<String>,
    name: String,
    cash: HashMap<String, Decimal>,
}

/// An id from `name` (`Swing bot` → `swing-bot`; `account` when nothing ASCII is left), made
/// unique with `-2`, `-3`, ….
fn account_id_for(name: &str, taken: impl Fn(&str) -> bool) -> String {
    let mut slug = String::new();
    for c in name.to_ascii_lowercase().chars() {
        if c.is_ascii_alphanumeric() {
            slug.push(c);
        } else if !slug.is_empty() && !slug.ends_with('-') {
            slug.push('-');
        }
    }
    let mut base: String = slug.trim_end_matches('-').chars().take(24).collect();
    if base.is_empty() {
        base = "account".into();
    }
    (1..).map(|n| if n == 1 { base.clone() } else { format!("{base}-{n}") }).find(|id| !taken(id)).expect("some suffix is free")
}

async fn create_account(State(s): State<WebState>, peer: Peer, headers: HeaderMap, Json(b): Json<NewAccount>) -> ApiResult {
    let (user, _) = authed(&s, &headers, false).await?;
    let name = b.name.trim();
    if name.is_empty() || name.chars().count() > 100 {
        return Err(bad_request("name must be 1-100 characters"));
    }
    let id = match b.id.as_deref().map(str::trim).filter(|i| !i.is_empty()) {
        Some(id) if !valid_account_id(id) => return Err(bad_request("id must be 1-32 characters of a-z, 0-9, _ and -")),
        Some(id) if s.app.account(id).is_some() => return Err(ApiError(StatusCode::CONFLICT, "exists", format!("account {id:?} already exists"))),
        Some(id) => id.to_string(),
        None => account_id_for(name, |id| s.app.account(id).is_some()),
    };
    let cash = parse_cash(&b.cash)?;
    s.app.create_account(&id, name, &cash).await.map_err(internal)?;
    let _ = s.auth.audit(Some(user.id), "account_create", &id, &peer_ip(&headers, peer)).await;
    Ok(Json(json!({"id": id})))
}

#[derive(Deserialize)]
struct ResetBody {
    cash: HashMap<String, Decimal>,
}

async fn reset_account(State(s): State<WebState>, peer: Peer, headers: HeaderMap, Path(id): Path<String>, Json(b): Json<ResetBody>) -> ApiResult {
    let (user, _) = authed(&s, &headers, false).await?;
    row(&s, &id)?;
    let cash = parse_cash(&b.cash)?;
    let generation = s.app.reset_account(&id, &cash).await.map_err(internal)?;
    let _ = s.auth.audit(Some(user.id), "account_reset", &id, &peer_ip(&headers, peer)).await;
    Ok(Json(json!({"id": id, "generation": generation})))
}

/// The Attacca project whose conversations can receive alerts.
const ALERT_PROJECT: &str = "ATrader";

/// Conversations in the "ATrader" project (created when missing), newest first as Attacca lists them.
async fn attacca_sessions(State(s): State<WebState>, headers: HeaderMap) -> ApiResult {
    use zyris_attacca::{AttaccaApi, AttaccaApiClient, ZNewProject, ZSessionFilter};
    authed(&s, &headers, false).await?;
    let unavailable = |m: String| ApiError(StatusCode::SERVICE_UNAVAILABLE, "not_connected", m);
    let upstream = |e: zyris::Error| ApiError(StatusCode::BAD_GATEWAY, "upstream", e.message);
    let conn = s.attacca.get().ok_or_else(|| unavailable("not connected to Attacca".into()))?;
    let api = conn.wait_capability::<AttaccaApiClient>(std::time::Duration::from_secs(5)).await.map_err(|e| unavailable(e.to_string()))?;
    let project = match api.list_projects().await.map_err(upstream)?.into_iter().find(|p| p.name == ALERT_PROJECT) {
        Some(p) => p,
        None => api.create_project(ZNewProject { name: ALERT_PROJECT.into(), description: Some("ATrader alerts".into()) }).await.map_err(upstream)?,
    };
    let sessions = api.list_sessions(ZSessionFilter { project_id: Some(project.id.clone()), limit: Some(100) }).await.map_err(upstream)?;
    Ok(Json(json!({
        "project": {"id": project.id, "name": project.name},
        "sessions": sessions.into_iter().map(|x| json!({"id": x.id, "title": x.title, "running": x.running})).collect::<Vec<_>>(),
    })))
}

#[derive(Deserialize)]
struct AlertSessionBody {
    session_id: Option<String>,
}

async fn set_alert_session(State(s): State<WebState>, peer: Peer, headers: HeaderMap, Path(id): Path<String>, Json(b): Json<AlertSessionBody>) -> ApiResult {
    let (user, _) = authed(&s, &headers, false).await?;
    row(&s, &id)?;
    let session = b.session_id.as_deref().map(str::trim).filter(|x| !x.is_empty());
    if session.is_some_and(|x| x.len() > 128 || x.chars().any(char::is_control)) {
        return Err(bad_request("session id is too long or has control characters"));
    }
    s.app.store.set_alert_session(&id, session).await.map_err(internal)?;
    let _ = s.auth.audit(Some(user.id), "alert_session", &format!("{id} -> {}", session.unwrap_or("-")), &peer_ip(&headers, peer)).await;
    Ok(Json(json!({"id": id, "session_id": session})))
}

fn kst_midnight(now: DateTime<Utc>) -> DateTime<Utc> {
    let day = now.with_timezone(&chrono_tz::Asia::Seoul).date_naive();
    chrono_tz::Asia::Seoul.from_local_datetime(&day.and_hms_opt(0, 0, 0).expect("midnight")).single().map_or(now, |t| t.with_timezone(&Utc))
}

async fn overview(State(s): State<WebState>, headers: HeaderMap) -> ApiResult {
    authed(&s, &headers, false).await?;
    let t = s.tools();
    let midnight = kst_midnight(Utc::now());
    let mut out = Vec::new();
    for r in s.app.store.list_accounts().await.map_err(internal)? {
        let summary = t.get_account(Some(r.id.clone())).await.map_err(tool_error)?;
        let open = s.app.store.snapshots(&r.id, r.generation, Some(midnight), true).await.map_err(internal)?;
        let day_pnl = open.first().map(|o| summary.equity_krw - o.equity_krw);
        let total = t.get_performance(Some(r.id.clone()), "all".into()).await.ok().map(|p| p.return_pct);
        let alert_session = s.app.store.alert_session(&r.id).await.map_err(internal)?;
        out.push(json!({"id": r.id, "alert_session_id": alert_session, "generation": r.generation, "summary": summary, "day_pnl_krw": day_pnl, "total_return_pct": total}));
    }
    Ok(Json(Value::Array(out)))
}

async fn account(State(s): State<WebState>, headers: HeaderMap, Path(id): Path<String>) -> ApiResult {
    authed(&s, &headers, false).await?;
    let r = row(&s, &id)?;
    let t = s.tools();
    let alert_session = s.app.store.alert_session(&id).await.map_err(internal)?;
    Ok(Json(json!({
        "generation": r.generation,
        "alert_session_id": alert_session,
        "summary": t.get_account(Some(id.clone())).await.map_err(tool_error)?,
        "positions": t.get_positions(Some(id.clone())).await.map_err(tool_error)?,
        "open_orders": t.list_orders(Some(id.clone()), Some(true), Some(200)).await.map_err(tool_error)?,
        "performance_all": t.get_performance(Some(id), "all".into()).await.ok(),
    })))
}

#[derive(Deserialize)]
struct RangeQuery {
    range: Option<String>,
}

async fn equity(State(s): State<WebState>, headers: HeaderMap, Path(id): Path<String>, Query(q): Query<RangeQuery>) -> ApiResult {
    authed(&s, &headers, false).await?;
    let r = row(&s, &id)?;
    let range = q.range.as_deref().unwrap_or("1m");
    let since = match range {
        "1d" => Some(Duration::days(1)),
        "1w" => Some(Duration::weeks(1)),
        "1m" => Some(Duration::days(31)),
        "all" => None,
        other => return Err(bad_request(format!("unknown range {other:?}; use 1d, 1w, 1m or all"))),
    }
    .map(|d| Utc::now() - d);
    let snaps = s.app.store.snapshots(&id, r.generation, since, range != "1d").await.map_err(internal)?;
    Ok(Json(Value::Array(snaps.iter().map(|p| json!({"at": p.at, "equity_krw": p.equity_krw})).collect())))
}

#[derive(Deserialize)]
struct LimitQuery {
    limit: Option<u32>,
    open_only: Option<bool>,
}

async fn fills(State(s): State<WebState>, headers: HeaderMap, Path(id): Path<String>, Query(q): Query<LimitQuery>) -> ApiResult {
    authed(&s, &headers, false).await?;
    let r = row(&s, &id)?;
    let fills = s.tools().list_fills(Some(id.clone()), None, q.limit).await.map_err(tool_error)?;
    let reasons: HashMap<u64, String> =
        s.app.store.orders(&id, r.generation, false, 1000).await.map_err(internal)?.into_iter().map(|o| (o.id, o.req.reason)).collect();
    let mut out = Vec::with_capacity(fills.len());
    for f in fills {
        let reason = reasons.get(&f.order_id).cloned();
        let mut v = to_json(f)?;
        v["reason"] = json!(reason);
        out.push(v);
    }
    Ok(Json(Value::Array(out)))
}

async fn orders(State(s): State<WebState>, headers: HeaderMap, Path(id): Path<String>, Query(q): Query<LimitQuery>) -> ApiResult {
    authed(&s, &headers, false).await?;
    Ok(Json(to_json(s.tools().list_orders(Some(id), q.open_only, q.limit).await.map_err(tool_error)?)?))
}

async fn pnl(State(s): State<WebState>, headers: HeaderMap, Path(id): Path<String>) -> ApiResult {
    authed(&s, &headers, false).await?;
    let r = row(&s, &id)?;
    let seoul = |t: DateTime<Utc>| t.with_timezone(&chrono_tz::Asia::Seoul).date_naive();
    let days = s.app.store.snapshots(&id, r.generation, None, true).await.map_err(internal)?;
    let now_equity = s.tools().get_account(Some(id.clone())).await.map_err(tool_error)?.equity_krw;
    // Each daily snapshot opens its day; the next one (or the live equity, for today) closes it.
    let closes = days.iter().skip(1).map(|d| d.equity_krw).chain([now_equity]);
    let daily: Vec<Value> = days.iter().zip(closes).map(|(open, close)| json!({"date": seoul(open.at), "pnl_krw": close - open.equity_krw})).collect();
    let mut by: BTreeMap<String, (Decimal, Decimal)> = BTreeMap::new();
    for f in s.app.store.fills(&id, r.generation, None, 100_000).await.map_err(internal)? {
        let e = by.entry(f.instrument.to_string()).or_default();
        e.0 += f.realized_pnl.unwrap_or_default();
        e.1 += f.fee + f.tax;
    }
    let by_symbol: Vec<Value> = by.into_iter().map(|(i, (realized, fees))| json!({"instrument": i, "realized": realized, "fees": fees})).collect();
    Ok(Json(json!({"daily": daily, "by_symbol": by_symbol})))
}

async fn alerts(State(s): State<WebState>, headers: HeaderMap, Path(id): Path<String>) -> ApiResult {
    authed(&s, &headers, false).await?;
    Ok(Json(to_json(s.tools().list_alerts(Some(id)).await.map_err(tool_error)?)?))
}

#[derive(Deserialize)]
struct ChartQuery {
    interval: Option<String>,
    limit: Option<u32>,
    account: Option<String>,
}

async fn chart(State(s): State<WebState>, headers: HeaderMap, Path(id): Path<String>, Query(q): Query<ChartQuery>) -> ApiResult {
    authed(&s, &headers, false).await?;
    let t = s.tools();
    let candles = t.get_candles(id.clone(), q.interval.unwrap_or_else(|| "1d".into()), q.limit).await.map_err(tool_error)?;
    let markers = match q.account.as_deref() {
        Some(acc) => {
            let r = row(&s, acc)?;
            let fills = s.app.store.fills(acc, r.generation, None, 100_000).await.map_err(internal)?;
            fills
                .iter()
                .filter(|f| f.instrument.to_string() == id)
                .map(|f| json!({"at": f.at, "side": f.side.code(), "price": f.price, "qty": f.qty}))
                .collect()
        }
        None => Vec::new(),
    };
    let quote = t.get_quotes(vec![id]).await.ok().and_then(|q| q.into_iter().next());
    Ok(Json(json!({"candles": candles, "markers": markers, "quote": quote})))
}

fn health_json(s: &WebState) -> Value {
    let feeds: Vec<Value> = s.app.market.subscriptions().into_iter().map(|(v, n)| json!({"venue": v.tag(), "subscribed": n})).collect();
    json!({"feeds": feeds, "zyris_connected": s.zyris_connected.load(Ordering::Relaxed)})
}

async fn health(State(s): State<WebState>, headers: HeaderMap) -> ApiResult {
    authed(&s, &headers, false).await?;
    Ok(Json(health_json(&s)))
}

// ---------------------------------------------------------------------------------------------
// Settings: data keys and the Attacca link

async fn keys(State(s): State<WebState>, headers: HeaderMap) -> ApiResult {
    authed(&s, &headers, false).await?;
    let rows = crate::settings::EDITABLE
        .iter()
        .map(|k| {
            let source = crate::settings::source_in(&s.settings_dir, k.name);
            let value = (!k.secret).then(|| crate::settings::get_in(&s.settings_dir, k.name)).flatten();
            json!({"name": k.name, "secret": k.secret, "configured": source.is_some(), "source": source, "value": value})
        })
        .collect();
    Ok(Json(Value::Array(rows)))
}

async fn save_keys(State(s): State<WebState>, peer: Peer, headers: HeaderMap, Json(b): Json<HashMap<String, String>>) -> ApiResult {
    let (user, _) = authed(&s, &headers, false).await?;
    if b.is_empty() {
        return Err(bad_request("nothing to save"));
    }
    for (name, value) in &b {
        if !crate::settings::EDITABLE.iter().any(|k| k.name == name) {
            return Err(bad_request(format!("{name} cannot be set from the dashboard")));
        }
        if matches!(crate::settings::source_in(&s.settings_dir, name), Some(crate::settings::Source::Env | crate::settings::Source::File)) {
            return Err(ApiError(StatusCode::CONFLICT, "set_by_operator", format!("{name} is set in the server's environment; change it there")));
        }
        let v = value.trim();
        if v.len() > 4096 || v.contains(['\n', '\r']) {
            return Err(bad_request(format!("{name} must be one line")));
        }
        if name == "KIS_ENV" && !matches!(v, "" | "real" | "mock") {
            return Err(bad_request("KIS_ENV is real or mock"));
        }
    }
    for (name, value) in &b {
        crate::settings::save_in(&s.settings_dir, name, value).map_err(internal)?;
    }
    let mut names: Vec<&str> = b.keys().map(String::as_str).collect();
    names.sort_unstable();
    let _ = s.auth.audit(Some(user.id), "settings_changed", &names.join(","), &peer_ip(&headers, peer)).await;
    s.restart.notify_one();
    Ok(Json(json!({"restarting": true})))
}

async fn zyris_status(State(s): State<WebState>, headers: HeaderMap) -> ApiResult {
    authed(&s, &headers, false).await?;
    let source = crate::settings::source_in(&s.settings_dir, "ZYRIS_CREDENTIAL");
    let enrollment = s.enrollment.lock().unwrap().clone();
    Ok(Json(json!({
        "connected": s.zyris_connected.load(Ordering::Relaxed),
        "enrolled": source.is_some(),
        "source": source,
        "enrollment": enrollment,
    })))
}

/// Start a device-grant enrollment (or return the one in progress). Approval stores the
/// credential and restarts the server so it connects with it.
async fn zyris_enroll(State(s): State<WebState>, peer: Peer, headers: HeaderMap) -> ApiResult {
    let (user, _) = authed(&s, &headers, false).await?;
    if matches!(crate::settings::source_in(&s.settings_dir, "ZYRIS_CREDENTIAL"), Some(crate::settings::Source::Env | crate::settings::Source::File)) {
        return Err(ApiError(StatusCode::CONFLICT, "set_by_operator", "the Attacca credential is set in the server's environment; change it there".into()));
    }
    {
        let current = s.enrollment.lock().unwrap();
        if current.status == "pending" && current.expires_at.is_some_and(|t| t > Utc::now()) {
            return Ok(Json(to_json(&*current)?));
        }
    }
    let mut enrollment = zyris::enroll(&crate::cli::zyris_server(), crate::cli::enroll_request())
        .await
        .map_err(|e| ApiError(StatusCode::BAD_GATEWAY, "upstream", format!("Attacca did not issue a code: {e}")))?;
    let code = enrollment.code().clone();
    let view = EnrollView {
        status: "pending",
        user_code: Some(code.user_code),
        verification_uri: Some(code.verification_uri),
        expires_at: Some(DateTime::<Utc>::from(code.expires_at)),
        message: None,
    };
    *s.enrollment.lock().unwrap() = view.clone();
    let ip = peer_ip(&headers, peer);
    let st = s.clone();
    tokio::spawn(async move {
        let outcome = enrollment.wait().await;
        let mut v = st.enrollment.lock().unwrap().clone();
        v.user_code = None;
        match outcome {
            Ok(credential) => match crate::settings::save_in(&st.settings_dir, "ZYRIS_CREDENTIAL", credential.secret()) {
                Ok(()) => {
                    v.status = "granted";
                    *st.enrollment.lock().unwrap() = v;
                    let _ = st.auth.audit(Some(user.id), "zyris_enrolled", "", &ip).await;
                    st.restart.notify_one();
                    return;
                }
                Err(e) => {
                    tracing::error!(error = %e, "could not store the zyris credential");
                    (v.status, v.message) = ("error", Some("could not store the credential".into()));
                }
            },
            Err(zyris::EnrollError::Lapsed) => v.status = "expired",
            Err(zyris::EnrollError::Denied) => v.status = "denied",
            Err(e) => (v.status, v.message) = ("error", Some(e.to_string())),
        }
        *st.enrollment.lock().unwrap() = v;
    });
    Ok(Json(to_json(view)?))
}

// ---------------------------------------------------------------------------------------------
// Live stream

fn event(name: &str, v: impl serde::Serialize) -> Option<Event> {
    Event::default().event(name).json_data(v).ok()
}

async fn equity_events(s: &WebState) -> Vec<Event> {
    let Ok(usd_krw) = s.app.fx.usd_krw().await else { return Vec::new() };
    let now = s.app.broker.now();
    s.app
        .broker
        .generations()
        .into_keys()
        .filter_map(|account| {
            let pf = s.app.broker.portfolio(&account)?;
            let v = value_account(&s.app.broker, &pf, usd_krw);
            event("equity", json!({"account": account, "at": now, "equity_krw": v.equity_krw.round_dp(0)}))
        })
        .collect()
}

async fn stream(State(s): State<WebState>, headers: HeaderMap) -> Result<Sse<impl Stream<Item = Result<Event, Infallible>>>, ApiError> {
    let (_, session) = authed(&s, &headers, false).await?;
    let bus = stream::unfold(s.bus.subscribe(), |mut rx| async move {
        loop {
            match rx.recv().await {
                Ok(BusEvent::Order(o)) => return Some((event("order", OrderView::from(&o)), rx)),
                Ok(BusEvent::Fill(f)) => return Some((event("fill", FillView::from(&f)), rx)),
                Ok(BusEvent::Market(_)) | Err(broadcast::error::RecvError::Lagged(_)) => continue,
                Err(broadcast::error::RecvError::Closed) => return None,
            }
        }
    })
    .filter_map(|e| async { e });
    let ticks = stream::unfold((s.clone(), 0u64), |(s, n)| async move {
        tokio::time::sleep(std::time::Duration::from_secs(10)).await;
        let mut events = equity_events(&s).await;
        if n % 3 == 0 {
            events.extend(event("health", health_json(&s)));
        }
        Some((stream::iter(events), (s, n + 1)))
    })
    .flatten();
    // End the stream once the session is revoked or expires.
    let ended = {
        let s = s.clone();
        async move {
            loop {
                tokio::time::sleep(std::time::Duration::from_secs(30)).await;
                if !matches!(s.auth.session(&session.id_hash, Utc::now()).await, Ok(Some(_))) {
                    return;
                }
            }
        }
    };
    let events = stream::select(bus, ticks).take_until(ended).map(Ok);
    Ok(Sse::new(events).keep_alive(KeepAlive::default()))
}

// ---------------------------------------------------------------------------------------------
// Static SPA

#[derive(rust_embed::RustEmbed)]
#[folder = "web/dist"]
#[allow_missing = true]
struct Assets;

async fn static_file(uri: axum::http::Uri) -> Response {
    let path = uri.path().trim_start_matches('/');
    if path.starts_with("api/") {
        return ApiError(StatusCode::NOT_FOUND, "not_found", "no such endpoint".into()).into_response();
    }
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
        .route("/api/accounts/{id}/alert-session", put(set_alert_session))
        .route("/api/attacca/sessions", get(attacca_sessions))
        .route("/api/accounts/{id}/equity", get(equity))
        .route("/api/accounts/{id}/fills", get(fills))
        .route("/api/accounts/{id}/orders", get(orders))
        .route("/api/accounts/{id}/pnl", get(pnl))
        .route("/api/accounts/{id}/alerts", get(alerts))
        .route("/api/instruments/{id}/chart", get(chart))
        .route("/api/health", get(health))
        .route("/api/settings/keys", get(keys).put(save_keys))
        .route("/api/zyris", get(zyris_status))
        .route("/api/zyris/enroll", post(zyris_enroll))
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

#[cfg(test)]
mod tests {
    #[test]
    fn account_ids_come_from_names() {
        let none = |_: &str| false;
        assert_eq!(super::account_id_for("Swing bot!", none), "swing-bot");
        assert_eq!(super::account_id_for("가상 계좌", none), "account");
        assert_eq!(super::account_id_for("가상 계좌", |id| id == "account" || id == "account-2"), "account-3");
    }
}
