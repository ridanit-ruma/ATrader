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
