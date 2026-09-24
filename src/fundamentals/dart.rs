//! OpenDART: corp codes, disclosures, key accounts, share counts and filing documents.

use std::collections::HashMap;
use std::io::Read;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use anyhow::{Context, anyhow};
use chrono::{Datelike, NaiveDate};
use rust_decimal::Decimal;
use serde_json::Value;

use super::{Filing, Fundamentals, PeriodFinancials};
use crate::domain::Currency;

const BASE: &str = "https://opendart.fss.or.kr/api";

/// `Ok(true)` on data, `Ok(false)` on "no data" (013), `Err` on anything else.
pub fn check_status(body: &Value) -> anyhow::Result<bool> {
    match body["status"].as_str() {
        Some("000") => Ok(true),
        Some("013") => Ok(false),
        s => Err(anyhow!("DART {}: {}", s.unwrap_or("?"), body["message"].as_str().unwrap_or(""))),
    }
}

/// Comma-formatted amount; `-`, blank and missing are `None`.
pub fn amount(v: &Value) -> Option<Decimal> {
    let s = v.as_str()?.trim().replace(',', "");
    if s.is_empty() || s == "-" {
        return None;
    }
    s.parse().ok()
}

fn tag<'a>(block: &'a str, name: &str) -> Option<&'a str> {
    let open = format!("<{name}>");
    let start = block.find(&open)? + open.len();
    let end = block[start..].find(&format!("</{name}>"))? + start;
    Some(block[start..end].trim())
}

/// `stock_code` → `corp_code` for listed companies.
pub fn parse_corp_codes(xml: &str) -> HashMap<String, String> {
    xml.split("<list>")
        .skip(1)
        .filter_map(|b| {
            let stock = tag(b, "stock_code")?;
            if stock.is_empty() {
                return None;
            }
            Some((stock.to_string(), tag(b, "corp_code")?.to_string()))
        })
        .collect()
}

pub fn parse_list(body: &Value) -> Vec<Filing> {
    body["list"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|r| {
            let id = r["rcept_no"].as_str()?.to_string();
            let title = r["report_nm"].as_str()?.trim().to_string();
            Some(Filing {
                form: report_form(&title),
                url: format!("https://dart.fss.or.kr/dsaf001/main.do?rcpNo={id}"),
                date: NaiveDate::parse_from_str(r["rcept_dt"].as_str()?, "%Y%m%d").ok()?,
                title,
                id,
            })
        })
        .collect()
}

/// `[기재정정]사업보고서 (2025.12)` → `사업보고서`.
fn report_form(title: &str) -> String {
    let t = title.trim();
    let t = match t.strip_prefix('[').and_then(|r| r.split_once(']')) {
        Some((_, rest)) => rest,
        None => t,
    };
    t.split(" (").next().unwrap_or(t).trim().to_string()
}

#[derive(Clone, Copy)]
enum Line {
    Revenue,
    Operating,
    Net,
    Equity,
}

fn line(account: &str) -> Option<Line> {
    match account.replace("(손실)", "").replace(' ', "").as_str() {
        "매출액" | "수익(매출액)" => Some(Line::Revenue),
        "영업이익" => Some(Line::Operating),
        "당기순이익" => Some(Line::Net),
        "자본총계" => Some(Line::Equity),
        _ => None,
    }
}

/// Rows of the preferred statement: consolidated (CFS) when present, else separate (OFS).
fn statement_rows(body: &Value) -> Vec<&Value> {
    let rows: Vec<&Value> = body["list"].as_array().into_iter().flatten().collect();
    let fs = if rows.iter().any(|r| r["fs_div"] == "CFS") { "CFS" } else { "OFS" };
    rows.into_iter().filter(|r| r["fs_div"] == fs).collect()
}

fn set(p: &mut PeriodFinancials, l: Line, v: Option<Decimal>) {
    match l {
        Line::Revenue => p.revenue = v,
        Line::Operating => p.operating_income = v,
        Line::Net => p.net_income = v,
        Line::Equity => p.equity = v,
    }
}

/// An annual report (`reprt_code 11011`) carries this and the two prior years.
pub fn parse_annual(body: &Value) -> Vec<PeriodFinancials> {
    let rows = statement_rows(body);
    let Some(year) = rows.first().and_then(|r| r["bsns_year"].as_str()).and_then(|y| y.parse::<i32>().ok()) else { return Vec::new() };
    let mut out: Vec<PeriodFinancials> = (0..3)
        .map(|k| PeriodFinancials {
            label: format!("FY{}", year - k),
            end: NaiveDate::from_ymd_opt(year - k, 12, 31).expect("valid date"),
            revenue: None,
            operating_income: None,
            net_income: None,
            eps: None,
            equity: None,
        })
        .collect();
    for r in rows {
        let Some(l) = r["account_nm"].as_str().and_then(line) else { continue };
        for (k, key) in ["thstrm_amount", "frmtrm_amount", "bfefrmtrm_amount"].iter().enumerate() {
            set(&mut out[k], l, amount(&r[*key]));
        }
    }
    out
}

/// The current-period column of a quarterly or half-year report.
pub fn parse_quarter(body: &Value, label: String, end: NaiveDate) -> Option<PeriodFinancials> {
    let rows = statement_rows(body);
    if rows.is_empty() {
        return None;
    }
    let mut p = PeriodFinancials { label, end, revenue: None, operating_income: None, net_income: None, eps: None, equity: None };
    for r in rows {
        if let Some(l) = r["account_nm"].as_str().and_then(line) {
            set(&mut p, l, amount(&r["thstrm_amount"]));
        }
    }
    Some(p)
}

/// Shares outstanding (`distb_stock_co`) from the total row, else the first row.
pub fn parse_shares(body: &Value) -> Option<Decimal> {
    let rows: Vec<&Value> = body["list"].as_array().into_iter().flatten().collect();
    let total = rows.iter().find(|r| r["se"].as_str().is_some_and(|s| s.contains("합계"))).or(rows.first())?;
    amount(&total["distb_stock_co"])
}

/// DART zip endpoints answer errors with an XML `<result>` instead of a zip.
pub fn zip_or_error(bytes: &[u8]) -> anyhow::Result<Vec<u8>> {
    if bytes.starts_with(b"PK") {
        return Ok(bytes.to_vec());
    }
    let text = String::from_utf8_lossy(bytes);
    Err(anyhow!("DART {}: {}", tag(&text, "status").unwrap_or("?"), tag(&text, "message").unwrap_or("unexpected response")))
}

/// Every file in a filing's document zip, as one text.
pub fn document_text(zip_bytes: &[u8]) -> anyhow::Result<String> {
    let mut archive = zip::ZipArchive::new(std::io::Cursor::new(zip_bytes))?;
    let mut parts = Vec::new();
    for i in 0..archive.len() {
        let mut raw = Vec::new();
        archive.by_index(i)?.read_to_end(&mut raw)?;
        let head = String::from_utf8_lossy(&raw[..raw.len().min(200)]).to_lowercase();
        let text = if head.contains("euc-kr") { encoding_rs::EUC_KR.decode(&raw).0.into_owned() } else { String::from_utf8_lossy(&raw).into_owned() };
        parts.push(super::html_to_text(&text));
    }
    Ok(parts.join("\n\n"))
}

pub struct DartClient {
    http: reqwest::Client,
    key: String,
    corp_codes: tokio::sync::Mutex<Option<(Instant, HashMap<String, String>)>>,
    cache: Mutex<HashMap<String, (Instant, Value)>>,
}

impl DartClient {
    pub fn new(key: String) -> Self {
        crate::init_tls();
        DartClient {
            http: reqwest::Client::builder().timeout(Duration::from_secs(60)).build().expect("http client"),
            key,
            corp_codes: tokio::sync::Mutex::new(None),
            cache: Mutex::new(HashMap::new()),
        }
    }

    async fn bytes(&self, path: &str, query: &[(&str, &str)]) -> anyhow::Result<Vec<u8>> {
        let mut q = vec![("crtfc_key", self.key.as_str())];
        q.extend_from_slice(query);
        // reqwest errors embed the request URL, which carries the API key: strip it.
        let fetch = async { self.http.get(format!("{BASE}/{path}")).query(&q).send().await?.error_for_status()?.bytes().await };
        Ok(fetch.await.map_err(reqwest::Error::without_url)?.to_vec())
    }

    /// A cached (1 h) JSON call; `None` when DART has no data.
    async fn json(&self, path: &str, query: &[(&str, &str)]) -> anyhow::Result<Option<Value>> {
        let cache_key = format!("{path}?{query:?}");
        if let Some((at, v)) = self.cache.lock().unwrap().get(&cache_key) {
            if at.elapsed() < Duration::from_secs(3600) {
                return Ok(Some(v.clone()));
            }
        }
        let body: Value = serde_json::from_slice(&self.bytes(path, query).await?).context("DART response")?;
        if !check_status(&body)? {
            return Ok(None);
        }
        self.cache.lock().unwrap().insert(cache_key, (Instant::now(), body.clone()));
        Ok(Some(body))
    }

    async fn corp_code(&self, stock_code: &str) -> anyhow::Result<String> {
        let mut slot = self.corp_codes.lock().await;
        if slot.as_ref().is_none_or(|(at, _)| at.elapsed() > Duration::from_secs(86400)) {
            let zip = zip_or_error(&self.bytes("corpCode.xml", &[]).await?)?;
            let xml = crate::feed::kis::master::unzip_first(&zip)?;
            *slot = Some((Instant::now(), parse_corp_codes(&String::from_utf8_lossy(&xml))));
        }
        let map = &slot.as_ref().expect("filled above").1;
        map.get(stock_code).cloned().ok_or_else(|| anyhow!("{stock_code} has no DART corp code"))
    }

    pub async fn fundamentals(&self, stock_code: &str, today: NaiveDate) -> anyhow::Result<Fundamentals> {
        let corp = self.corp_code(stock_code).await?;
        let accounts = |year: i32, reprt: &'static str| {
            let corp = corp.clone();
            async move { self.json("fnlttSinglAcnt.json", &[("corp_code", &corp), ("bsns_year", &year.to_string()), ("reprt_code", reprt)]).await }
        };
        // The latest annual report: last year's if filed, else the year before.
        let mut annual = Vec::new();
        let mut annual_year = today.year() - 1;
        for year in [today.year() - 1, today.year() - 2] {
            if let Some(body) = accounts(year, "11011").await? {
                annual = parse_annual(&body);
                annual_year = year;
                break;
            }
        }
        let mut latest_quarter = None;
        for (reprt, label, month, day) in [("11014", "Q3", 9, 30), ("11012", "Q2", 6, 30), ("11013", "Q1", 3, 31)] {
            let year = today.year();
            if let Some(body) = accounts(year, reprt).await? {
                latest_quarter = parse_quarter(&body, format!("{year}{label}"), NaiveDate::from_ymd_opt(year, month, day).expect("valid date"));
                break;
            }
        }
        let shares = match self.json("stockTotqySttus.json", &[("corp_code", &corp), ("bsns_year", &annual_year.to_string()), ("reprt_code", "11011")]).await? {
            Some(body) => parse_shares(&body),
            None => None,
        };
        // DART has no EPS; derive it from net income and shares outstanding.
        if let Some(sh) = shares.filter(|s| *s > Decimal::ZERO) {
            for p in annual.iter_mut().chain(latest_quarter.iter_mut()) {
                p.eps = p.net_income.map(|n| (n / sh).round_dp(2));
            }
        }
        Ok(Fundamentals { currency: Currency::Krw, annual, latest_quarter, shares_outstanding: shares, eps_computed: true })
    }

    pub async fn filings(&self, stock_code: &str, since: NaiveDate, limit: usize) -> anyhow::Result<Vec<Filing>> {
        let corp = self.corp_code(stock_code).await?;
        let body = self
            .json("list.json", &[("corp_code", &corp), ("bgn_de", &since.format("%Y%m%d").to_string()), ("page_count", &limit.clamp(1, 100).to_string())])
            .await?;
        Ok(body.map(|b| parse_list(&b)).unwrap_or_default())
    }

    pub async fn filing_text(&self, rcept_no: &str) -> anyhow::Result<String> {
        if rcept_no.len() != 14 || !rcept_no.bytes().all(|b| b.is_ascii_digit()) {
            return Err(anyhow!("a DART filing id is the 14-digit rcept_no"));
        }
        let zip = zip_or_error(&self.bytes("document.xml", &[("rcept_no", rcept_no)]).await?)?;
        document_text(&zip)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rust_decimal_macros::dec;
    use serde_json::json;

    fn row(fs: &str, account: &str, cur: &str, prev: &str, prev2: &str) -> Value {
        json!({"fs_div": fs, "sj_div": "IS", "account_nm": account, "bsns_year": "2025",
               "thstrm_nm": "제 57 기", "thstrm_dt": "2025.01.01 ~ 2025.12.31", "thstrm_amount": cur,
               "frmtrm_amount": prev, "bfefrmtrm_amount": prev2})
    }

    #[test]
    fn statuses_and_amounts() {
        assert!(check_status(&json!({"status": "000"})).unwrap());
        assert!(!check_status(&json!({"status": "013", "message": "조회된 데이타가 없습니다."})).unwrap());
        let e = check_status(&json!({"status": "020", "message": "요청 제한을 초과하였습니다."})).unwrap_err().to_string();
        assert!(e.contains("020") && e.contains("요청 제한"), "{e}");
        assert_eq!(amount(&json!("9,999,999")), Some(dec!(9999999)));
        assert_eq!(amount(&json!("-1,234")), Some(dec!(-1234)));
        assert_eq!(amount(&json!("-")), None);
        assert_eq!(amount(&json!("")), None);
        assert_eq!(amount(&Value::Null), None);
    }

    #[test]
    fn corp_codes_map_listed_stocks_only() {
        let xml = "<?xml version=\"1.0\" encoding=\"UTF-8\"?><result>\
            <list><corp_code>00126380</corp_code><corp_name>삼성전자</corp_name><stock_code>005930</stock_code><modify_date>20240101</modify_date></list>\
            <list><corp_code>00999999</corp_code><corp_name>비상장</corp_name><stock_code> </stock_code><modify_date>20240101</modify_date></list></result>";
        let m = parse_corp_codes(xml);
        assert_eq!(m.get("005930").map(String::as_str), Some("00126380"));
        assert_eq!(m.len(), 1);
    }

    #[test]
    fn annual_accounts_prefer_consolidated_and_tolerate_blanks() {
        let body = json!({"status": "000", "list": [
            row("OFS", "매출액", "1", "1", "1"),
            row("CFS", "수익(매출액)", "300,870,903,000,000", "258,935,494,000,000", "302,231,360,000,000"),
            row("CFS", "영업이익(손실)", "-", "6,566,976,000,000", "43,376,630,000,000"),
            row("CFS", "당기순이익(손실)", "34,451,351,000,000", "15,487,100,000,000", ""),
            row("CFS", "자본총계", "402,192,070,000,000", "368,640,190,000,000", "354,749,604,000,000")
        ]});
        let a = parse_annual(&body);
        assert_eq!(a.iter().map(|p| p.label.as_str()).collect::<Vec<_>>(), vec!["FY2025", "FY2024", "FY2023"]);
        assert_eq!(a[0].revenue, Some(dec!(300870903000000)));
        assert_eq!(a[0].operating_income, None);
        assert_eq!(a[2].net_income, None);
        assert_eq!(a[1].equity, Some(dec!(368640190000000)));
        assert_eq!(a[0].end, NaiveDate::from_ymd_opt(2025, 12, 31).unwrap());
    }

    #[test]
    fn shares_list_and_errors() {
        let shares = json!({"status": "000", "list": [
            {"se": "보통주", "distb_stock_co": "5,919,637,922"},
            {"se": "합계", "distb_stock_co": "6,735,000,000"}]});
        assert_eq!(parse_shares(&shares), Some(dec!(6735000000)));
        let list = json!({"status": "000", "list": [{"corp_name": "삼성전자", "report_nm": "사업보고서 (2025.12)", "rcept_no": "20260310000123", "rcept_dt": "20260310"}]});
        let f = parse_list(&list);
        assert_eq!((f[0].id.as_str(), f[0].form.as_str()), ("20260310000123", "사업보고서"));
        assert_eq!(f[0].url, "https://dart.fss.or.kr/dsaf001/main.do?rcpNo=20260310000123");
        assert_eq!(report_form("[기재정정]사업보고서 (2025.12)"), "사업보고서");
        let xml_error = "<?xml version=\"1.0\"?><result><status>010</status><message>등록되지 않은 인증키입니다.</message></result>";
        let e = zip_or_error(xml_error.as_bytes()).unwrap_err().to_string();
        assert!(e.contains("010") && e.contains("인증키"), "{e}");
    }

    #[test]
    fn document_zip_becomes_text() {
        use std::io::Write;
        let mut buf = std::io::Cursor::new(Vec::new());
        {
            let mut z = zip::ZipWriter::new(&mut buf);
            z.start_file("20260310000123.xml", zip::write::SimpleFileOptions::default()).unwrap();
            z.write_all("<DOCUMENT><BODY><P>매출액&amp;영업이익</P></BODY></DOCUMENT>".as_bytes()).unwrap();
            z.finish().unwrap();
        }
        assert_eq!(document_text(buf.get_ref()).unwrap(), "매출액&영업이익");
    }

    #[tokio::test]
    async fn transport_errors_do_not_reveal_the_key() {
        let mut c = DartClient::new("SECRETKEY1234567890".into());
        c.http = reqwest::Client::builder().timeout(Duration::from_millis(1)).build().unwrap();
        let e = c.bytes("list.json", &[]).await.unwrap_err();
        assert!(!format!("{e:#}").contains("SECRETKEY"), "{e:#}");
    }
}
