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
        // Field 2 is the hour class: "0" is the regular session; others are auction or
        // after-hours expected books, which never trade at the prices they show.
        KRX_BOOK if rec.len() >= 43 && rec[2].trim() == "0" => Some(MarketEvent::Book(Book {
            instrument: id(Venue::Krx, &rec[0]),
            asks: levels(&rec[3..13], &rec[23..33])?,
            bids: levels(&rec[13..23], &rec[33..43])?,
            prev_close: None,
            received_at: now,
        })),
        KRX_TRADE if rec.len() >= 13 && rec.get(43).is_none_or(|h| h.trim() == "0") => Some(MarketEvent::Trade(Trade {
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

    #[test]
    fn only_regular_session_records_become_events() {
        let mut book = krx_book_record("005930");
        book[2] = "A".into(); // closing-auction expected book
        assert_eq!(record_event(KRX_BOOK, &book, now()), None);
        let mut trade = vec!["0".to_string(); 46];
        trade[0] = "005930".into();
        trade[2] = "70100".into();
        trade[12] = "1".into();
        trade[43] = "1".into(); // not the regular session
        assert_eq!(record_event(KRX_TRADE, &trade, now()), None);
    }
}
