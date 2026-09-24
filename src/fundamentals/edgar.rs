//! SEC EDGAR: ticker → CIK, filings, and XBRL company facts.

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use anyhow::{Context, anyhow};
use chrono::NaiveDate;
use rust_decimal::Decimal;
use serde::Deserialize;
use serde_json::Value;

use super::{Filing, Fundamentals, PeriodFinancials};
use crate::domain::Currency;

#[derive(Deserialize)]
struct TickerRow {
    cik_str: u64,
    ticker: String,
}

pub fn parse_tickers(bytes: &[u8]) -> anyhow::Result<HashMap<String, u64>> {
    let rows: HashMap<String, TickerRow> = serde_json::from_slice(bytes)?;
    Ok(rows.into_values().map(|r| (r.ticker.to_uppercase(), r.cik_str)).collect())
}

/// `filings.recent` is columnar: parallel arrays.
pub fn parse_filings(bytes: &[u8], cik: u64) -> anyhow::Result<Vec<Filing>> {
    let v: Value = serde_json::from_slice(bytes)?;
    let r = &v["filings"]["recent"];
    let col = |k: &str| r[k].as_array().cloned().unwrap_or_default();
    let (acc, date, form, doc, desc) = (col("accessionNumber"), col("filingDate"), col("form"), col("primaryDocument"), col("primaryDocDescription"));
    let mut out: Vec<Filing> = (0..acc.len())
        .filter_map(|i| {
            let accession = acc[i].as_str()?;
            let primary = doc.get(i)?.as_str()?;
            let form = form.get(i)?.as_str()?.to_string();
            let title = desc.get(i).and_then(Value::as_str).filter(|s| !s.is_empty()).unwrap_or(&form).to_string();
            Some(Filing {
                id: format!("{accession}:{primary}"),
                url: format!("https://www.sec.gov/Archives/edgar/data/{cik}/{}/{primary}", accession.replace('-', "")),
                date: NaiveDate::parse_from_str(date.get(i)?.as_str()?, "%Y-%m-%d").ok()?,
                title,
                form,
            })
        })
        .collect();
    out.sort_by(|a, b| b.date.cmp(&a.date));
    Ok(out)
}

#[derive(Debug, Clone, Deserialize)]
struct FactRow {
    start: Option<NaiveDate>,
    end: NaiveDate,
    val: Decimal,
    form: String,
    filed: NaiveDate,
}

fn rows(facts: &Value, taxonomy: &str, concept: &str, unit: &str) -> Vec<FactRow> {
    serde_json::from_value(facts[taxonomy][concept]["units"][unit].clone()).unwrap_or_default()
}

/// Duration rows of `days_lo..=days_hi`, one per (start, end), latest filing wins, newest first.
fn periods(rows: &[FactRow], days_lo: i64, days_hi: i64, annual_only: bool) -> Vec<FactRow> {
    let mut best: HashMap<(NaiveDate, NaiveDate), FactRow> = HashMap::new();
    for r in rows {
        let Some(start) = r.start else { continue };
        let days = (r.end - start).num_days();
        if days < days_lo || days > days_hi || (annual_only && r.form != "10-K") {
            continue;
        }
        let e = best.entry((start, r.end)).or_insert_with(|| r.clone());
        if r.filed > e.filed {
            *e = r.clone();
        }
    }
    let mut out: Vec<FactRow> = best.into_values().collect();
    out.sort_by(|a, b| b.end.cmp(&a.end));
    out
}

fn value_at(rows: &[FactRow], end: NaiveDate) -> Option<Decimal> {
    rows.iter().filter(|r| r.end == end).max_by_key(|r| r.filed).map(|r| r.val)
}

pub fn parse_facts(bytes: &[u8]) -> anyhow::Result<Fundamentals> {
    let v: Value = serde_json::from_slice(bytes)?;
    let facts = &v["facts"];
    let gaap = |c: &str, unit: &str| rows(facts, "us-gaap", c, unit);
    // The revenue concept a company uses changed over time: take the one reporting most recently.
    let revenue = ["RevenueFromContractWithCustomerExcludingAssessedTax", "Revenues", "SalesRevenueNet"]
        .into_iter()
        .map(|c| gaap(c, "USD"))
        .max_by_key(|r| r.iter().map(|x| x.end).max())
        .unwrap_or_default();
    let (op, net, eps, equity) = (gaap("OperatingIncomeLoss", "USD"), gaap("NetIncomeLoss", "USD"), gaap("EarningsPerShareDiluted", "USD/shares"), gaap("StockholdersEquity", "USD"));
    let build = |end: NaiveDate, start: NaiveDate, label: String, lo: i64, hi: i64, annual: bool| {
        let pick = |rs: &[FactRow]| periods(rs, lo, hi, annual).into_iter().find(|r| r.end == end && r.start == Some(start)).map(|r| r.val);
        PeriodFinancials { label, end, revenue: pick(&revenue), operating_income: pick(&op), net_income: pick(&net), eps: pick(&eps), equity: value_at(&equity, end) }
    };
    // Periods are defined by net income, which every filer reports.
    let annual = periods(&net, 335, 395, true)
        .into_iter()
        .take(3)
        .map(|r| build(r.end, r.start.expect("duration row"), format!("FY{}", r.end.format("%Y")), 335, 395, true))
        .collect();
    let latest_quarter = periods(&net, 80, 100, false).into_iter().next().map(|r| {
        let q = (r.end.format("%m").to_string().parse::<u32>().unwrap_or(1) - 1) / 3 + 1;
        build(r.end, r.start.expect("duration row"), format!("{}Q{q}", r.end.format("%Y")), 80, 100, false)
    });
    let shares: Vec<FactRow> = rows(facts, "dei", "EntityCommonStockSharesOutstanding", "shares");
    Ok(Fundamentals {
        currency: Currency::Usd,
        annual,
        latest_quarter,
        shares_outstanding: shares.iter().max_by_key(|r| (r.end, r.filed)).map(|r| r.val),
        eps_computed: false,
    })
}

pub struct EdgarClient {
    http: reqwest::Client,
    user_agent: String,
    last: tokio::sync::Mutex<Instant>,
    cache: Mutex<HashMap<String, (Instant, std::sync::Arc<Vec<u8>>)>>,
}

impl EdgarClient {
    pub fn new(user_agent: String) -> Self {
        crate::init_tls();
        EdgarClient {
            http: reqwest::Client::builder().timeout(Duration::from_secs(30)).build().expect("http client"),
            user_agent,
            last: tokio::sync::Mutex::new(Instant::now() - Duration::from_secs(1)),
            cache: Mutex::new(HashMap::new()),
        }
    }

    /// A paced, cached (1 h) GET.
    async fn get(&self, url: &str) -> anyhow::Result<std::sync::Arc<Vec<u8>>> {
        if let Some((at, body)) = self.cache.lock().unwrap().get(url) {
            if at.elapsed() < Duration::from_secs(3600) {
                return Ok(body.clone());
            }
        }
        {
            let mut last = self.last.lock().await;
            tokio::time::sleep(Duration::from_millis(150).saturating_sub(last.elapsed())).await;
            *last = Instant::now();
        }
        let body = self.http.get(url).header("user-agent", &self.user_agent).send().await?.error_for_status()?.bytes().await?.to_vec();
        let body = std::sync::Arc::new(body);
        self.cache.lock().unwrap().insert(url.to_string(), (Instant::now(), body.clone()));
        Ok(body)
    }

    async fn cik(&self, symbol: &str) -> anyhow::Result<u64> {
        let map = parse_tickers(&self.get("https://www.sec.gov/files/company_tickers.json").await?)?;
        map.get(&symbol.to_uppercase()).copied().ok_or_else(|| anyhow!("{symbol} is not an SEC filer"))
    }

    pub async fn fundamentals(&self, symbol: &str) -> anyhow::Result<Fundamentals> {
        let cik = self.cik(symbol).await?;
        parse_facts(&self.get(&format!("https://data.sec.gov/api/xbrl/companyfacts/CIK{cik:010}.json")).await?).context("company facts")
    }

    pub async fn filings(&self, symbol: &str, since: Option<NaiveDate>, limit: usize) -> anyhow::Result<Vec<Filing>> {
        let cik = self.cik(symbol).await?;
        let all = parse_filings(&self.get(&format!("https://data.sec.gov/submissions/CIK{cik:010}.json")).await?, cik)?;
        Ok(all.into_iter().filter(|f| since.is_none_or(|s| f.date >= s)).take(limit).collect())
    }

    /// Text of a filing's primary document; `filing_id` is `{accession}:{primaryDocument}`.
    pub async fn filing_text(&self, symbol: &str, filing_id: &str) -> anyhow::Result<String> {
        let (accession, doc) = filing_id.split_once(':').ok_or_else(|| anyhow!("filing id must look like 0000320193-26-000020:aapl-20260627.htm"))?;
        if !accession.chars().all(|c| c.is_ascii_digit() || c == '-') || doc.contains('/') {
            return Err(anyhow!("malformed filing id"));
        }
        let cik = self.cik(symbol).await?;
        let html = self.get(&format!("https://www.sec.gov/Archives/edgar/data/{cik}/{}/{doc}", accession.replace('-', ""))).await?;
        Ok(super::html_to_text(&String::from_utf8_lossy(&html)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rust_decimal_macros::dec;

    #[test]
    fn tickers_map_to_ciks() {
        let m = parse_tickers(include_bytes!("../../tests/fixtures/edgar/company_tickers.json")).unwrap();
        assert_eq!(m.get("AAPL"), Some(&320193));
    }

    #[test]
    fn filings_are_listed_with_urls() {
        let f = parse_filings(include_bytes!("../../tests/fixtures/edgar/submissions_aapl.json"), 320193).unwrap();
        assert!(!f.is_empty());
        assert!(f.windows(2).all(|w| w[0].date >= w[1].date));
        let q = f.iter().find(|x| x.form == "10-Q").unwrap();
        assert!(q.url.starts_with("https://www.sec.gov/Archives/edgar/data/320193/"), "{}", q.url);
        assert!(q.id.contains(':'));
    }

    #[test]
    fn annual_figures_come_from_full_year_10k_rows() {
        let f = parse_facts(include_bytes!("../../tests/fixtures/edgar/companyfacts_aapl.json")).unwrap();
        let fy = &f.annual[0];
        assert_eq!(fy.end, NaiveDate::from_ymd_opt(2025, 9, 27).unwrap());
        assert_eq!(fy.label, "FY2025");
        assert_eq!(fy.revenue, Some(dec!(416161000000)));
        assert_eq!(fy.operating_income, Some(dec!(133050000000)));
        assert_eq!(fy.net_income, Some(dec!(112010000000)));
        assert_eq!(fy.eps, Some(dec!(7.46)));
        assert_eq!(fy.equity, Some(dec!(73733000000)));
        assert_eq!(f.annual[1].revenue, Some(dec!(391035000000)));
        assert!(f.annual.len() <= 3);
        let q = f.latest_quarter.as_ref().unwrap();
        assert!(q.end > fy.end);
        let days = |p: &PeriodFinancials| p.end;
        assert!(q.revenue.unwrap() < fy.revenue.unwrap() / dec!(2), "quarter must be a 3-month figure, got {:?} ending {}", q.revenue, days(q));
        assert!(f.shares_outstanding.unwrap() > dec!(1000000000));
        assert!(!f.eps_computed);
    }
}
