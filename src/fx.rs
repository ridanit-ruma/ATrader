//! USD/KRW, cached: from a live [`FxSource`] when one is plugged in, else (or when it fails) the
//! ECB reference rate via Frankfurter, which moves once per business day.

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use anyhow::anyhow;
use rust_decimal::Decimal;
use serde::Deserialize;

const URL: &str = "https://api.frankfurter.dev/v1/latest?base=USD&symbols=KRW";
const MAX_AGE: Duration = Duration::from_secs(3600);

pub fn parse_frankfurter(bytes: &[u8]) -> anyhow::Result<Decimal> {
    #[derive(Deserialize)]
    struct Resp {
        rates: HashMap<String, Decimal>,
    }
    let r: Resp = serde_json::from_slice(bytes)?;
    r.rates.get("KRW").copied().ok_or_else(|| anyhow!("no KRW rate in response"))
}

/// A live USD/KRW quote, tried before the reference rate.
#[async_trait::async_trait]
pub trait FxSource: Send + Sync {
    async fn usd_krw(&self) -> anyhow::Result<Decimal>;
    /// How long one answer stays good.
    fn max_age(&self) -> Duration;
}

pub struct FxCache {
    http: reqwest::Client,
    cached: Mutex<Option<(Decimal, Instant)>>,
    fixed: bool,
    source: Option<std::sync::Arc<dyn FxSource>>,
}

impl FxCache {
    pub fn new() -> Self {
        crate::init_tls();
        FxCache { http: reqwest::Client::builder().timeout(Duration::from_secs(10)).build().expect("http client"), cached: Mutex::new(None), fixed: false, source: None }
    }

    /// A cache that always answers `rate` (tests, and offline runs).
    pub fn fixed(rate: Decimal) -> Self {
        crate::init_tls();
        FxCache { http: reqwest::Client::new(), cached: Mutex::new(Some((rate, Instant::now()))), fixed: true, source: None }
    }

    pub fn with_source(mut self, source: std::sync::Arc<dyn FxSource>) -> Self {
        self.source = Some(source);
        self
    }

    /// KRW per USD. Refreshes once the rate is older than the source allows (an hour for the
    /// reference rate); if every refresh fails, the last good rate is used.
    pub async fn usd_krw(&self) -> anyhow::Result<Decimal> {
        if self.fixed {
            if let Some((rate, _)) = *self.cached.lock().unwrap() {
                return Ok(rate);
            }
        }
        let cached = *self.cached.lock().unwrap();
        let max_age = self.source.as_ref().map_or(MAX_AGE, |s| s.max_age());
        if let Some((rate, at)) = cached {
            if at.elapsed() < max_age {
                return Ok(rate);
            }
        }
        let live = match &self.source {
            Some(s) => s.usd_krw().await.map_err(|e| tracing::warn!(error = %e, "live fx failed; using the reference rate")).ok(),
            None => None,
        };
        let fetched = match live {
            Some(rate) => Ok(rate),
            None => async {
                let body = self.http.get(URL).send().await?.error_for_status()?.bytes().await?;
                parse_frankfurter(&body)
            }
            .await,
        };
        match (fetched, cached) {
            (Ok(rate), _) => {
                *self.cached.lock().unwrap() = Some((rate, Instant::now()));
                Ok(rate)
            }
            (Err(e), Some((rate, _))) => {
                tracing::warn!(error = %e, "fx refresh failed; using the last rate");
                Ok(rate)
            }
            (Err(e), None) => Err(e),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rust_decimal_macros::dec;

    struct Flaky {
        rate: std::sync::Mutex<Option<Decimal>>,
        calls: std::sync::atomic::AtomicUsize,
    }

    #[async_trait::async_trait]
    impl FxSource for Flaky {
        async fn usd_krw(&self) -> anyhow::Result<Decimal> {
            self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            (*self.rate.lock().unwrap()).ok_or_else(|| anyhow!("down"))
        }
        fn max_age(&self) -> Duration {
            Duration::ZERO
        }
    }

    #[tokio::test]
    async fn a_live_source_comes_first_and_its_last_rate_covers_an_outage() {
        let src = std::sync::Arc::new(Flaky { rate: std::sync::Mutex::new(Some(dec!(1359))), calls: Default::default() });
        let fx = FxCache::new().with_source(src.clone());
        assert_eq!(fx.usd_krw().await.unwrap(), dec!(1359));
        *src.rate.lock().unwrap() = Some(dec!(1361.5));
        assert_eq!(fx.usd_krw().await.unwrap(), dec!(1361.5), "max_age zero: asked every time");
        assert_eq!(src.calls.load(std::sync::atomic::Ordering::SeqCst), 2);
    }

    #[test]
    fn parses_frankfurter_rate_exactly() {
        let body = br#"{"amount":1.0,"base":"USD","date":"2026-09-23","rates":{"KRW":1365.35}}"#;
        assert_eq!(parse_frankfurter(body).unwrap(), dec!(1365.35));
        assert!(parse_frankfurter(br#"{"rates":{}}"#).is_err());
    }
}
