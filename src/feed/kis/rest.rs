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
