mod common;

use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use tower::ServiceExt;
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

#[sqlx::test]
async fn totp_codes_are_single_use_and_resets_end_sessions(pool: PgPool) {
    let s = AuthStore(pool);
    let uid = s.create_user("ruma", &hash_password("long enough pass")).await.unwrap();
    let secret = new_totp_secret();
    s.set_totp(uid, Some(&secret), true).await.unwrap();
    let now = Utc::now();
    let code = current_code_for_tests(&secret, now);
    assert!(s.accept_totp(uid, &secret, &code, now).await.unwrap());
    assert!(!s.accept_totp(uid, &secret, &code, now).await.unwrap()); // replay
    let tok = new_token();
    s.create_session(uid, &token_hash(&tok), false, "ip", "ua", now).await.unwrap();
    s.delete_user_sessions(uid).await.unwrap();
    assert!(s.session(&token_hash(&tok), now).await.unwrap().is_none());
}

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
    let full = cookie_of(&en);
    let v: serde_json::Value = serde_json::from_slice(&axum::body::to_bytes(en.into_body(), 10_000).await.unwrap()).unwrap();
    assert_eq!(v["recovery_codes"].as_array().unwrap().len(), 10);
    // Enrolment rotates the session: the pre-2FA token is dead, the new one is a full session.
    assert_eq!(r.clone().oneshot(req("GET", "/api/overview", Some(&c), None)).await.unwrap().status(), StatusCode::UNAUTHORIZED);
    assert_eq!(r.clone().oneshot(req("GET", "/api/overview", Some(&full), None)).await.unwrap().status(), StatusCode::OK);
}

fn cookie_of(res: &axum::response::Response) -> String {
    res.headers()[header::SET_COOKIE].to_str().unwrap().split(';').next().unwrap().trim_start_matches("atrader_session=").to_string()
}

#[sqlx::test]
async fn password_change_ends_every_other_session(pool: PgPool) {
    let (r, _, secret) = web(pool).await;
    let a = login(&r, &secret).await;
    let short = r.clone().oneshot(req("POST", "/api/auth/password", Some(&a), Some(serde_json::json!({"current": "long enough pass", "new": "short"})))).await.unwrap();
    assert_eq!(short.status(), StatusCode::BAD_REQUEST);
    let wrong = r.clone().oneshot(req("POST", "/api/auth/password", Some(&a), Some(serde_json::json!({"current": "not the password", "new": "another long pass"})))).await.unwrap();
    assert_eq!(wrong.status(), StatusCode::UNAUTHORIZED);
    let ok = r.clone().oneshot(req("POST", "/api/auth/password", Some(&a), Some(serde_json::json!({"current": "long enough pass", "new": "another long pass"})))).await.unwrap();
    assert_eq!(ok.status(), StatusCode::OK);
    let b = cookie_of(&ok);
    assert_eq!(r.clone().oneshot(req("GET", "/api/overview", Some(&a), None)).await.unwrap().status(), StatusCode::UNAUTHORIZED);
    assert_eq!(r.clone().oneshot(req("GET", "/api/overview", Some(&b), None)).await.unwrap().status(), StatusCode::OK);
}

#[sqlx::test]
async fn accounts_are_managed_over_http(pool: PgPool) {
    let (r, _, secret) = web(pool).await;
    let c = login(&r, &secret).await;
    let bad = r.clone().oneshot(req("POST", "/api/accounts", Some(&c), Some(serde_json::json!({"id": "Bad Id!", "name": "x", "cash": {"KRW": "1"}})))).await.unwrap();
    assert_eq!(bad.status(), StatusCode::BAD_REQUEST);
    let neg = r.clone().oneshot(req("POST", "/api/accounts", Some(&c), Some(serde_json::json!({"id": "neg", "name": "x", "cash": {"KRW": "-1"}})))).await.unwrap();
    assert_eq!(neg.status(), StatusCode::BAD_REQUEST);
    let ok = r.clone().oneshot(req("POST", "/api/accounts", Some(&c), Some(serde_json::json!({"id": "swing", "name": "Swing", "cash": {"KRW": "5000000"}})))).await.unwrap();
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

#[sqlx::test]
async fn keys_are_write_only_and_saving_asks_for_a_restart(pool: PgPool) {
    let (app, _t) = common::rig(pool.clone()).await;
    let auth = AuthStore(pool.clone());
    let secret = new_totp_secret();
    let uid = auth.create_user("ruma", &hash_password("long enough pass")).await.unwrap();
    auth.set_totp(uid, Some(&secret), true).await.unwrap();
    let dir = std::env::temp_dir().join(format!("atrader-web-keys-{}", std::process::id()));
    let mut state = atrader::web::WebState::new(app, AuthStore(pool), false);
    state.settings_dir = dir.clone();
    let restart = state.restart.clone();
    let r = atrader::web::router(state);
    let c = login(&r, &secret).await;

    let bad = r.clone().oneshot(req("PUT", "/api/settings/keys", Some(&c), Some(serde_json::json!({"DATABASE_URL": "x"})))).await.unwrap();
    assert_eq!(bad.status(), StatusCode::BAD_REQUEST);
    let mock = r.clone().oneshot(req("PUT", "/api/settings/keys", Some(&c), Some(serde_json::json!({"KIS_ENV": "paper"})))).await.unwrap();
    assert_eq!(mock.status(), StatusCode::BAD_REQUEST);

    let ok = r.clone().oneshot(req("PUT", "/api/settings/keys", Some(&c), Some(serde_json::json!({"KIS_APP_KEY": "PSabc123", "KIS_ENV": "mock"})))).await.unwrap();
    assert_eq!(ok.status(), StatusCode::OK);
    tokio::time::timeout(std::time::Duration::from_secs(1), restart.notified()).await.expect("restart requested");
    assert_eq!(std::fs::read_to_string(dir.join("settings/KIS_APP_KEY")).unwrap(), "PSabc123");

    let list = r.clone().oneshot(req("GET", "/api/settings/keys", Some(&c), None)).await.unwrap();
    let body = axum::body::to_bytes(list.into_body(), 10_000).await.unwrap();
    assert!(!String::from_utf8_lossy(&body).contains("PSabc123"), "secrets are never returned");
    let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
    let key = v.as_array().unwrap().iter().find(|k| k["name"] == "KIS_APP_KEY").unwrap();
    assert_eq!((key["configured"].clone(), key["source"].clone()), (serde_json::json!(true), serde_json::json!("dashboard")));
    let env = v.as_array().unwrap().iter().find(|k| k["name"] == "KIS_ENV").unwrap();
    assert_eq!(env["value"], "mock", "non-secret settings are shown");

    let zyris = r.clone().oneshot(req("GET", "/api/zyris", Some(&c), None)).await.unwrap();
    let v: serde_json::Value = serde_json::from_slice(&axum::body::to_bytes(zyris.into_body(), 10_000).await.unwrap()).unwrap();
    assert_eq!(v["enrollment"]["status"], "idle");
    std::fs::remove_dir_all(dir).unwrap();
}

#[sqlx::test]
async fn new_accounts_need_only_a_name(pool: PgPool) {
    let (r, _, secret) = web(pool).await;
    let c = login(&r, &secret).await;
    let mk = |name: &str| req("POST", "/api/accounts", Some(&c), Some(serde_json::json!({"name": name, "cash": {"KRW": "1000000"}})));
    let ids: Vec<String> = {
        let mut out = Vec::new();
        for name in ["가상 계좌", "가상 계좌", "Swing bot"] {
            let res = r.clone().oneshot(mk(name)).await.unwrap();
            assert_eq!(res.status(), StatusCode::OK);
            let v: serde_json::Value = serde_json::from_slice(&axum::body::to_bytes(res.into_body(), 10_000).await.unwrap()).unwrap();
            out.push(v["id"].as_str().unwrap().to_string());
        }
        out
    };
    assert_eq!(ids, vec!["account", "account-2", "swing-bot"]);
    let agents = r.clone().oneshot(req("GET", "/api/agents", Some(&c), None)).await.unwrap();
    assert_eq!(agents.status(), StatusCode::NOT_FOUND, "agent ids are gone");
}

#[sqlx::test]
async fn alerts_go_to_a_chosen_conversation(pool: PgPool) {
    let (r, _, secret) = web(pool).await;
    let c = login(&r, &secret).await;
    // Not connected to Attacca in tests: the session list says so plainly.
    let list = r.clone().oneshot(req("GET", "/api/attacca/sessions", Some(&c), None)).await.unwrap();
    assert_eq!(list.status(), StatusCode::SERVICE_UNAVAILABLE);

    let put = |body: serde_json::Value| req("PUT", "/api/accounts/bot/alert-session", Some(&c), Some(body));
    assert_eq!(r.clone().oneshot(put(serde_json::json!({"session_id": "s-7"}))).await.unwrap().status(), StatusCode::OK);
    let detail = r.clone().oneshot(req("GET", "/api/accounts/bot", Some(&c), None)).await.unwrap();
    let v: serde_json::Value = serde_json::from_slice(&axum::body::to_bytes(detail.into_body(), 100_000).await.unwrap()).unwrap();
    assert_eq!(v["alert_session_id"], "s-7");
    assert_eq!(r.clone().oneshot(put(serde_json::json!({"session_id": null}))).await.unwrap().status(), StatusCode::OK);
    let detail = r.clone().oneshot(req("GET", "/api/accounts/bot", Some(&c), None)).await.unwrap();
    let v: serde_json::Value = serde_json::from_slice(&axum::body::to_bytes(detail.into_body(), 100_000).await.unwrap()).unwrap();
    assert_eq!(v["alert_session_id"], serde_json::Value::Null);
    let missing = req("PUT", "/api/accounts/nope/alert-session", Some(&c), Some(serde_json::json!({"session_id": "s"})));
    assert_eq!(r.clone().oneshot(missing).await.unwrap().status(), StatusCode::NOT_FOUND);
}

#[sqlx::test]
async fn briefings_are_set_per_account(pool: PgPool) {
    let (r, _, secret) = web(pool).await;
    let c = login(&r, &secret).await;
    let set = |v: &str| req("PUT", "/api/accounts/bot/briefing", Some(&c), Some(serde_json::json!({"briefing": v})));
    assert_eq!(r.clone().oneshot(set("hourly")).await.unwrap().status(), StatusCode::BAD_REQUEST);
    assert_eq!(r.clone().oneshot(set("4h")).await.unwrap().status(), StatusCode::OK);
    let o = r.clone().oneshot(req("GET", "/api/overview", Some(&c), None)).await.unwrap();
    let v: serde_json::Value = serde_json::from_slice(&axum::body::to_bytes(o.into_body(), 100_000).await.unwrap()).unwrap();
    let bot = v.as_array().unwrap().iter().find(|a| a["id"] == "bot").unwrap();
    assert_eq!(bot["briefing"], "4h");
    // Sending one now needs a chosen conversation and a live Attacca link.
    let send = req("POST", "/api/accounts/bot/briefing/send", Some(&c), None);
    assert_eq!(r.clone().oneshot(send).await.unwrap().status(), StatusCode::CONFLICT);
}

#[sqlx::test]
async fn cash_can_be_converted_from_the_dashboard(pool: PgPool) {
    let (r, _, secret) = web(pool).await;
    let c = login(&r, &secret).await;
    let convert = |body: serde_json::Value| req("POST", "/api/accounts/bot/convert", Some(&c), Some(body));
    let ok = r.clone().oneshot(convert(serde_json::json!({"from": "KRW", "to": "USD", "amount": "1400000"}))).await.unwrap();
    assert_eq!(ok.status(), StatusCode::OK);
    let v: serde_json::Value = serde_json::from_slice(&axum::body::to_bytes(ok.into_body(), 10_000).await.unwrap()).unwrap();
    let credit: rust_decimal::Decimal = v["credit"].as_str().unwrap().parse().unwrap();
    assert_eq!(credit, rust_decimal::Decimal::from(999), "₩1.4m at 1400 less the 0.1% spread: {v}");
    let too_much = r.clone().oneshot(convert(serde_json::json!({"from": "USD", "to": "KRW", "amount": "5000"}))).await.unwrap();
    assert_eq!(too_much.status(), StatusCode::BAD_REQUEST);
    let audit = r.clone().oneshot(req("GET", "/api/audit", Some(&c), None)).await.unwrap();
    let a: serde_json::Value = serde_json::from_slice(&axum::body::to_bytes(audit.into_body(), 100_000).await.unwrap()).unwrap();
    assert!(a.as_array().unwrap().iter().any(|e| e["action"] == "convert"));
}
