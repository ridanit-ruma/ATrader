//! USD/KRW reference rate (ECB via Frankfurter), cached.

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

pub struct FxCache {
    http: reqwest::Client,
    cached: Mutex<Option<(Decimal, Instant)>>,
    fixed: bool,
}

impl FxCache {
    pub fn new() -> Self {
        crate::init_tls();
        FxCache { http: reqwest::Client::new(), cached: Mutex::new(None), fixed: false }
    }

    /// A cache that always answers `rate` (tests, and offline runs).
    pub fn fixed(rate: Decimal) -> Self {
        crate::init_tls();
        FxCache { http: reqwest::Client::new(), cached: Mutex::new(Some((rate, Instant::now()))), fixed: true }
    }

    /// KRW per USD. Refreshes after an hour; if the refresh fails, the last good rate is used.
    pub async fn usd_krw(&self) -> anyhow::Result<Decimal> {
        if self.fixed {
            if let Some((rate, _)) = *self.cached.lock().unwrap() {
                return Ok(rate);
            }
        }
        let cached = *self.cached.lock().unwrap();
        if let Some((rate, at)) = cached {
            if at.elapsed() < MAX_AGE {
                return Ok(rate);
            }
        }
        let fetched = async {
            let body = self.http.get(URL).send().await?.error_for_status()?.bytes().await?;
            parse_frankfurter(&body)
        }
        .await;
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

    #[test]
    fn parses_frankfurter_rate_exactly() {
        let body = br#"{"amount":1.0,"base":"USD","date":"2026-09-23","rates":{"KRW":1365.35}}"#;
        assert_eq!(parse_frankfurter(body).unwrap(), dec!(1365.35));
        assert!(parse_frankfurter(br#"{"rates":{}}"#).is_err());
    }
}
