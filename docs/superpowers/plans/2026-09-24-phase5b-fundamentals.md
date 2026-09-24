# ATrader Phase 5b (Fundamentals: DART and SEC EDGAR) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Give the agent company fundamentals: normalized annual and latest-quarter financials with PER and PBR at the current price, a list of recent filings, and filing text by page. KRX comes from OpenDART and US stocks from SEC EDGAR.

**Architecture:**
- **`src/fundamentals/`** has three files:
  - `mod.rs` holds shared types, HTML/XML-to-text and paging.
  - `edgar.rs` parses the ticker map, submissions and companyfacts, and has a polite HTTP client (User-Agent, gzip, pacing and a 1 h cache).
  - `dart.rs` parses the corp-code zip, the disclosure list, key accounts, share counts and the document zip.
- **Parsing** is pure and tested on real EDGAR responses (AAPL, trimmed) and on DART shapes from the official guide.
- **`App`** holds `Option` clients built from the environment. Three new tools dispatch by venue.

**Tech Stack:** reqwest `gzip` feature. Everything else exists.

**Spec:** `docs/superpowers/specs/2026-09-24-atrader-design.md` §7 rows `get_financials`, `list_filings` and `get_filing`. This completes §15 step 5.

## Global Constraints

- Earlier phases' constraints still apply.
- Environment:
  - `DART_API_KEY` enables KRX fundamentals.
  - `EDGAR_USER_AGENT` enables US fundamentals. SEC requires a descriptive agent with a contact, e.g. `ATrader you@example.com`.
  - A venue without its variable returns `INVALID_REQUEST`, and the message names the variable.
- EDGAR:
  - Endpoints:
    - `https://www.sec.gov/files/company_tickers.json`
    - `https://data.sec.gov/submissions/CIK{cik:010}.json`
    - `https://data.sec.gov/api/xbrl/companyfacts/CIK{cik:010}.json`
  - Documents live at `https://www.sec.gov/Archives/edgar/data/{cik}/{accession without dashes}/{primaryDocument}`.
  - At least 150 ms between requests; SEC's limit is 10 per second.
  - Annual rows: `form == "10-K"` with a duration of 335–395 days. Quarterly rows: a duration of 80–100 days. Deduplicate on `(start, end)`, keeping the latest `filed`.
  - Revenue comes from the first of `RevenueFromContractWithCustomerExcludingAssessedTax`, `Revenues` and `SalesRevenueNet` with the latest `end`. The other concepts are `OperatingIncomeLoss`, `NetIncomeLoss`, `EarningsPerShareDiluted` (units `USD/shares`) and `StockholdersEquity` (instant; matched on `end`). Shares outstanding is `dei:EntityCommonStockSharesOutstanding`, the latest value.
- DART:
  - Base `https://opendart.fss.or.kr/api`, where every call takes `crtfc_key`.
  - Success is `status "000"`. `"013"` means no data and becomes an empty result. Any other status is an error carrying `message`.
  - Zip endpoints send an XML `<result>` instead of a zip when they fail. A body is a zip only if it starts with `PK`.
  - Key accounts: `fnlttSinglAcnt.json` with `reprt_code` `11011` for annual. The latest quarter tries `11014` (Q3), then `11012` (half), then `11013` (Q1) for the current year.
  - `CFS` rows are preferred, falling back to `OFS`. Account names are matched after stripping `(손실)`:
    - revenue: `매출액` or `수익(매출액)`
    - operating income: `영업이익`
    - net income: `당기순이익`
    - equity: `자본총계`
  - Amounts are comma-formatted strings.
  - Shares are `distb_stock_co` from the `stockTotqySttus.json` row whose `se` contains `합계`, falling back to the first row.
  - DART has no EPS, so EPS is computed as net income divided by shares outstanding. The response labels it `eps_basis: "computed"`.
- Filing text: tags are stripped, entities unescaped and whitespace collapsed. Pages are 20,000 characters, split on character boundaries.
- Filing ids:
  - EDGAR: `{accession}:{primaryDocument}`
  - DART: the 14-digit `rcept_no`
- Caching: a 1-hour in-memory response cache per client. The DART corp-code map is refreshed daily.

## Review Focus

1. **Fiscal periods must be chosen correctly.** A 10-K repeats earlier years and the Q4 three-month numbers, and a 10-Q carries year-to-date rows. Annual must come from 10-K full-year rows only, and the quarter from three-month rows only. Task 2 tests this.
2. **Odd DART amounts must not break parsing.** `"-"`, empty, negative (`"-1,234"`) and missing current-period amounts must become `None` or a negative number, never a parse error for the whole response. Task 3 tests this.
3. **A DART error must not be read as a zip.** A zip endpoint that answers with an XML error must surface the DART message. Task 3 tests this.
4. **Invalid ratios must be absent.** PER with a loss, zero shares or missing equity must be `null`, never negative infinity or a division by zero. Task 4 tests this.
5. **Filing text must be safe.** A huge filing must be paged and never returned whole, and a page number past the end must be an error that says how many pages exist. Task 1 tests this.

## File Structure

| File | Responsibility |
| --- | --- |
| `src/fundamentals/mod.rs` | `PeriodFinancials`, `Fundamentals`, `Filing`, `html_to_text`, `page_text`, `ratios` |
| `src/fundamentals/edgar.rs` | EDGAR parsing and `EdgarClient` |
| `src/fundamentals/dart.rs` | DART parsing and `DartClient` |
| `src/app.rs` | Modified: `dart` and `edgar` fields |
| `src/tools/{mod,dto}.rs` | Modified: `get_financials`, `list_filings`, `get_filing` |
| `src/cli.rs` | Modified: build the clients from the environment |
| `tests/fixtures/edgar/*` | Real, trimmed AAPL responses |
| `tests/live.rs` | Modified: EDGAR live check |

---

### Task 1: Shared types, text extraction and paging

**Files:**
- Create: `src/fundamentals/mod.rs`
- Modify: `src/lib.rs`

**Interfaces:**
- Produces:
  - `PeriodFinancials { label: String, end: NaiveDate, revenue, operating_income, net_income, eps, equity: Option<Decimal> }`.
  - `Fundamentals { currency: Currency, annual: Vec<PeriodFinancials> (newest first, ≤ 3), latest_quarter: Option<PeriodFinancials>, shares_outstanding: Option<Decimal>, eps_computed: bool }`.
  - `Filing { id: String, title: String, form: String, date: NaiveDate, url: String }`.
  - `html_to_text(&str) -> String`.
  - `page_text(&str, page: usize) -> Result<(String, usize), String>`, returning the page and the total page count.
  - `Ratios { per: Option<Decimal>, pbr: Option<Decimal> }` and `ratios(price, &Fundamentals) -> Ratios`.

- [ ] **Step 1: Write the failing tests** (create `src/fundamentals/mod.rs` with a tests module; add `pub mod fundamentals;` to `lib.rs`; create empty `edgar.rs` and `dart.rs` with a one-line doc comment and `pub mod edgar; pub mod dart;` in `mod.rs`)

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use rust_decimal_macros::dec;

    #[test]
    fn html_becomes_readable_text() {
        let html = "<html><head><style>p{}</style><script>x()</script></head><body><P>Revenue&nbsp;&amp; costs</P><table><tr><td>1</td><td>2</td></tr></table><br/>&#8212; done &lt;ok&gt;</body></html>";
        assert_eq!(html_to_text(html), "Revenue & costs\n1 2\n— done <ok>");
    }

    #[test]
    fn pages_split_on_characters_and_reject_out_of_range() {
        let text = "가".repeat(PAGE_CHARS + 5);
        let (first, pages) = page_text(&text, 1).unwrap();
        assert_eq!((first.chars().count(), pages), (PAGE_CHARS, 2));
        assert_eq!(page_text(&text, 2).unwrap().0.chars().count(), 5);
        assert_eq!(page_text(&text, 3), Err("page 3 does not exist; the filing has 2 pages".into()));
        assert_eq!(page_text(&text, 0), Err("page 0 does not exist; the filing has 2 pages".into()));
        assert_eq!(page_text("", 1), Ok((String::new(), 1)));
    }

    fn fundamentals(eps: Option<Decimal>, equity: Option<Decimal>, shares: Option<Decimal>) -> Fundamentals {
        Fundamentals {
            currency: crate::domain::Currency::Usd,
            annual: vec![PeriodFinancials { label: "FY2025".into(), end: NaiveDate::from_ymd_opt(2025, 9, 27).unwrap(), revenue: None, operating_income: None, net_income: None, eps, equity }],
            latest_quarter: None,
            shares_outstanding: shares,
            eps_computed: false,
        }
    }

    #[test]
    fn ratios_are_absent_when_meaningless() {
        let r = ratios(dec!(150), &fundamentals(Some(dec!(7.5)), Some(dec!(1000)), Some(dec!(100))));
        assert_eq!((r.per, r.pbr), (Some(dec!(20)), Some(dec!(15))));
        let loss = ratios(dec!(150), &fundamentals(Some(dec!(-2)), Some(dec!(1000)), Some(dec!(0))));
        assert_eq!((loss.per, loss.pbr), (None, None));
        let empty = ratios(dec!(150), &fundamentals(None, None, None));
        assert_eq!((empty.per, empty.pbr), (None, None));
    }
}
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test --lib fundamentals::`
Expected: compile errors.

- [ ] **Step 3: Implement** (prepend to `mod.rs`)

```rust
//! Company fundamentals: KRX from OpenDART, US from SEC EDGAR.

pub mod dart;
pub mod edgar;

use chrono::NaiveDate;
use rust_decimal::Decimal;

use crate::domain::Currency;

pub const PAGE_CHARS: usize = 20_000;

#[derive(Debug, Clone, PartialEq)]
pub struct PeriodFinancials {
    /// `FY2025`, `2026Q2`, …
    pub label: String,
    pub end: NaiveDate,
    pub revenue: Option<Decimal>,
    pub operating_income: Option<Decimal>,
    pub net_income: Option<Decimal>,
    pub eps: Option<Decimal>,
    pub equity: Option<Decimal>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Fundamentals {
    pub currency: Currency,
    /// Newest first, at most 3.
    pub annual: Vec<PeriodFinancials>,
    pub latest_quarter: Option<PeriodFinancials>,
    pub shares_outstanding: Option<Decimal>,
    /// EPS derived as net income / shares (DART) rather than reported (EDGAR).
    pub eps_computed: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Filing {
    /// What `get_filing` takes.
    pub id: String,
    pub title: String,
    pub form: String,
    pub date: NaiveDate,
    /// Human-readable page.
    pub url: String,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Ratios {
    pub per: Option<Decimal>,
    pub pbr: Option<Decimal>,
}

/// PER from the latest annual EPS and PBR from latest annual equity per share; `None` when
/// earnings or book value are not positive or an input is missing.
pub fn ratios(price: Decimal, f: &Fundamentals) -> Ratios {
    let latest = f.annual.first();
    let per = latest.and_then(|p| p.eps).filter(|e| *e > Decimal::ZERO).map(|e| (price / e).round_dp(2));
    let pbr = match (latest.and_then(|p| p.equity), f.shares_outstanding) {
        (Some(eq), Some(sh)) if eq > Decimal::ZERO && sh > Decimal::ZERO => Some((price / (eq / sh)).round_dp(2)),
        _ => None,
    };
    Ratios { per, pbr }
}

fn unescape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(i) = rest.find('&') {
        out.push_str(&rest[..i]);
        rest = &rest[i..];
        let Some(end) = rest.find(';').filter(|e| *e <= 10) else {
            out.push('&');
            rest = &rest[1..];
            continue;
        };
        let entity = &rest[1..end];
        let ch = match entity {
            "amp" => Some('&'),
            "lt" => Some('<'),
            "gt" => Some('>'),
            "quot" => Some('"'),
            "apos" | "#39" => Some('\''),
            "nbsp" | "#160" => Some(' '),
            e if e.starts_with("#x") || e.starts_with("#X") => u32::from_str_radix(&e[2..], 16).ok().and_then(char::from_u32),
            e if e.starts_with('#') => e[1..].parse().ok().and_then(char::from_u32),
            _ => None,
        };
        match ch {
            Some(c) => {
                out.push(c);
                rest = &rest[end + 1..];
            }
            None => {
                out.push('&');
                rest = &rest[1..];
            }
        }
    }
    out.push_str(rest);
    out
}

/// Plain text from HTML or DART XML: scripts and styles dropped, block ends become line breaks,
/// entities decoded, whitespace collapsed.
pub fn html_to_text(html: &str) -> String {
    let lower = html.to_ascii_lowercase();
    let mut text = String::new();
    let mut i = 0;
    while i < html.len() {
        if html[i..].starts_with('<') {
            let end = html[i..].find('>').map_or(html.len(), |e| i + e + 1);
            let tag = lower[i..end].trim_start_matches('<').trim_start_matches('/');
            let name: String = tag.chars().take_while(|c| c.is_ascii_alphanumeric()).collect();
            if !lower[i..].starts_with("</") && (name == "script" || name == "style") {
                let close = format!("</{name}");
                i = lower[end..].find(&close).map_or(html.len(), |c| end + c);
                continue;
            }
            if matches!(name.as_str(), "p" | "br" | "tr" | "div" | "li" | "h1" | "h2" | "h3" | "h4" | "title" | "table") {
                text.push('\n');
            } else {
                text.push(' ');
            }
            i = end;
        } else {
            let next = html[i..].find('<').map_or(html.len(), |n| i + n);
            text.push_str(&html[i..next]);
            i = next;
        }
    }
    unescape(&text)
        .lines()
        .map(|l| l.split_whitespace().collect::<Vec<_>>().join(" "))
        .filter(|l| !l.is_empty())
        .collect::<Vec<_>>()
        .join("\n")
}

/// Page `page` (1-based) of `text`, and how many pages there are.
pub fn page_text(text: &str, page: usize) -> Result<(String, usize), String> {
    let chars: Vec<char> = text.chars().collect();
    let pages = chars.len().div_ceil(PAGE_CHARS).max(1);
    if page == 0 || page > pages {
        return Err(format!("page {page} does not exist; the filing has {pages} pages"));
    }
    let start = (page - 1) * PAGE_CHARS;
    Ok((chars[start..(start + PAGE_CHARS).min(chars.len())].iter().collect(), pages))
}
```

The test's `<table><tr><td>1</td><td>2</td></tr></table>` must produce the line `1 2`. Cells become spaces and `tr`/`table` become line breaks. The `<br/>` gives a line break before `— done <ok>`.

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test --lib fundamentals::`
Expected: 3 passed.

- [ ] **Step 5: Commit**

```bash
git add src/lib.rs src/fundamentals
git commit -m "Add fundamentals types, filing text extraction and paging"
git log -1 --format=%B
```

---

### Task 2: EDGAR parsing and client

**Files:**
- Modify: `src/fundamentals/edgar.rs`, `Cargo.toml` (reqwest `gzip`)
- Create: `tests/fixtures/edgar/{company_tickers,submissions_aapl,companyfacts_aapl}.json` (real SEC responses, trimmed)

**Interfaces:**
- Produces:
  - `parse_tickers(&[u8]) -> anyhow::Result<HashMap<String, u64>>`.
  - `parse_filings(&[u8], cik: u64) -> anyhow::Result<Vec<Filing>>`, newest first.
  - `parse_facts(&[u8]) -> anyhow::Result<Fundamentals>`.
  - `EdgarClient::new(user_agent)` with `fundamentals(symbol)`, `filings(symbol, since: Option<NaiveDate>, limit)` and `filing_text(filing_id) -> anyhow::Result<String>`.

- [ ] **Step 1: Write the failing tests** (append a tests module to `edgar.rs`)

```rust
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
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test --lib fundamentals::edgar`
Expected: compile errors.

- [ ] **Step 3: Implement** (prepend to `edgar.rs`; `cargo add reqwest --no-default-features --features json,query,gzip,rustls-no-provider`)

```rust
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
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test --lib fundamentals::edgar`
Expected: 3 passed.

- [ ] **Step 5: Commit**

```bash
git add Cargo.toml Cargo.lock src/fundamentals/edgar.rs tests/fixtures/edgar
git commit -m "Parse SEC EDGAR filings and company facts"
git log -1 --format=%B
```

---

### Task 3: DART parsing and client

**Files:**
- Modify: `src/fundamentals/dart.rs`

**Interfaces:**
- Produces:
  - `check_status(&Value) -> anyhow::Result<bool>`, where `false` means no data (013).
  - `amount(&Value) -> Option<Decimal>`.
  - `parse_corp_codes(xml: &str) -> HashMap<String, String>`.
  - `parse_list(&Value) -> Vec<Filing>`.
  - `parse_annual(&Value) -> Vec<PeriodFinancials>` (3 years, newest first).
  - `parse_quarter(&Value, label) -> Option<PeriodFinancials>`.
  - `parse_shares(&Value) -> Option<Decimal>`.
  - `zip_or_error(&[u8]) -> anyhow::Result<Vec<u8>>`.
  - `document_text(zip: &[u8]) -> anyhow::Result<String>`.
  - `DartClient::new(key)` with `fundamentals(stock_code, today)`, `filings(stock_code, since, limit)` and `filing_text(rcept_no)`.

- [ ] **Step 1: Write the failing tests** (append to `dart.rs`)

```rust
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
}
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test --lib fundamentals::dart`
Expected: compile errors.

- [ ] **Step 3: Implement** (prepend to `dart.rs`)

```rust
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
            (!stock.is_empty()).then(|| (stock.to_string(), tag(b, "corp_code")?.to_string()))
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
                form: title.split(" (").next().unwrap_or(&title).trim_start_matches(|c| c == '[' || c != '[').to_string(),
                url: format!("https://dart.fss.or.kr/dsaf001/main.do?rcpNo={id}"),
                date: NaiveDate::parse_from_str(r["rcept_dt"].as_str()?, "%Y%m%d").ok()?,
                title,
                id,
            })
        })
        .collect()
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
        Ok(self.http.get(format!("{BASE}/{path}")).query(&q).send().await?.error_for_status()?.bytes().await?.to_vec())
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
```

`parse_list`'s `form` should be the report title without the period suffix or amendment prefix, e.g. `사업보고서`. Replace the `trim_start_matches` expression with this:

```rust
                form: title.trim_start_matches(|c: char| c != ']' && title.starts_with('[')).trim_start_matches(']').split(" (").next().unwrap_or(&title).trim().to_string(),
```

That strips a leading `[기재정정]` and a trailing ` (2025.12)`. The test covers the plain case. If the combinator gets hard to read, a small `fn report_form(title: &str) -> String` with `strip_prefix`/`split_once` is equally acceptable. Pick one and record the choice in the ledger.

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test --lib fundamentals::dart`
Expected: 5 passed.

- [ ] **Step 5: Commit**

```bash
git add src/fundamentals/dart.rs
git commit -m "Parse OpenDART disclosures, key accounts and documents"
git log -1 --format=%B
```

---

### Task 4: Fundamentals tools and wiring

**Files:**
- Modify: `src/app.rs`, `src/tools/mod.rs`, `src/tools/dto.rs`, `src/cli.rs`, `tests/tools.rs`, `tests/live.rs`, `README.md`

**Interfaces:**
- Produces:
  - `App { dart: Option<DartClient>, edgar: Option<EdgarClient> }`, which are public fields set after `App::new` through `with_fundamentals(dart, edgar)`.
  - Tools:
    - `get_financials(id) -> FinancialsView`
    - `list_filings(id, since: Option<String /*YYYY-MM-DD*/>, limit: Option<u32>) -> Vec<FilingView>`
    - `get_filing(id, filing_id, page: Option<u32>) -> FilingText`

- [ ] **Step 1: Write the failing tests** (append to `tests/tools.rs`)

```rust
#[sqlx::test]
async fn fundamentals_need_their_keys(pool: PgPool) {
    let (_, t) = rig(pool).await;
    let e = t.get_financials("UPBIT:KRW-BTC".into()).await.unwrap_err();
    assert_eq!(code(&e), "INVALID_REQUEST");
    assert!(e.message.contains("stocks"), "{}", e.message);
    assert_eq!(code(&t.list_filings("UPBIT:KRW-BTC".into(), None, None).await.unwrap_err()), "INVALID_REQUEST");
    assert_eq!(code(&t.list_filings("UPBIT:KRW-BTC".into(), Some("yesterday".into()), None).await.unwrap_err()), "InvalidParams");
}
```

Add a ratio-view unit test next to the helper in `src/tools/mod.rs` (see below).

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test --test tools fundamentals`
Expected: compile error (`get_financials` not found).

- [ ] **Step 3: Implement**

`src/app.rs`: add two public fields, `pub dart: Option<crate::fundamentals::dart::DartClient>` and `pub edgar: Option<crate::fundamentals::edgar::EdgarClient>`. Initialise both to `None` in `new`, and add:

```rust
    pub fn with_fundamentals(mut self, dart: Option<crate::fundamentals::dart::DartClient>, edgar: Option<crate::fundamentals::edgar::EdgarClient>) -> Self {
        self.dart = dart;
        self.edgar = edgar;
        self
    }
```

Add these DTOs to `src/tools/dto.rs`:

```rust
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct PeriodView {
    /// `FY2025` or `2026Q2`.
    pub period: String,
    pub end: chrono::NaiveDate,
    pub revenue: Option<Decimal>,
    pub operating_income: Option<Decimal>,
    pub net_income: Option<Decimal>,
    pub eps: Option<Decimal>,
    /// Total equity at period end.
    pub equity: Option<Decimal>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct FinancialsView {
    pub id: String,
    /// `DART` (KRX) or `SEC EDGAR` (US).
    pub source: String,
    /// Currency of every amount.
    pub currency: String,
    /// Newest first, up to 3 fiscal years.
    pub annual: Vec<PeriodView>,
    /// The latest quarter's own figures (not year-to-date).
    pub latest_quarter: Option<PeriodView>,
    pub shares_outstanding: Option<Decimal>,
    /// `reported` (EDGAR diluted EPS) or `computed` (DART: net income / shares outstanding).
    pub eps_basis: String,
    /// Current simulated mid price used for the ratios.
    pub price: Option<Decimal>,
    /// Price / latest annual EPS; absent for losses.
    pub per: Option<Decimal>,
    /// Price / (latest annual equity / shares).
    pub pbr: Option<Decimal>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct FilingView {
    /// Pass to `get_filing`.
    pub filing_id: String,
    pub title: String,
    pub form: String,
    pub date: chrono::NaiveDate,
    pub url: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct FilingText {
    pub filing_id: String,
    pub page: u32,
    pub pages: u32,
    /// Plain text, up to 20,000 characters.
    pub text: String,
}
```

Add the trait methods:

```rust
    /// Company financials: up to 3 fiscal years and the latest quarter (revenue, operating and
    /// net income, EPS, equity), shares outstanding, and PER/PBR at the current price. KRX
    /// (DART) and US (SEC EDGAR) stocks only.
    async fn get_financials(&self, id: String) -> zyris::Result<FinancialsView>;

    /// Recent disclosures and filings, newest first. `since` is YYYY-MM-DD (default: 90 days
    /// ago); `limit` default 20, at most 100.
    async fn list_filings(&self, id: String, since: Option<String>, limit: Option<u32>) -> zyris::Result<Vec<FilingView>>;

    /// A filing's text, 20,000 characters per page (`page` from 1; the answer says how many
    /// pages exist).
    async fn get_filing(&self, id: String, filing_id: String, page: Option<u32>) -> zyris::Result<FilingText>;
```

Add the helpers and implementations to `src/tools/mod.rs`:

```rust
fn not_enabled(what: &str) -> zyris::Error {
    order_error(OrderError::InvalidRequest(what.to_string()))
}

fn period_view(p: &crate::fundamentals::PeriodFinancials) -> PeriodView {
    PeriodView { period: p.label.clone(), end: p.end, revenue: p.revenue, operating_income: p.operating_income, net_income: p.net_income, eps: p.eps, equity: p.equity }
}
```

```rust
    async fn get_financials(&self, id: String) -> zyris::Result<FinancialsView> {
        let id = self.known(&id)?;
        let today = self.app.broker.now().date_naive();
        let (source, f) = match id.venue {
            Venue::Krx => {
                let dart = self.app.dart.as_ref().ok_or_else(|| not_enabled("KRX financials need DART_API_KEY on the server"))?;
                ("DART", dart.fundamentals(&id.symbol, today).await.map_err(|e| upstream(format!("{e:#}")))?)
            }
            Venue::Us => {
                let edgar = self.app.edgar.as_ref().ok_or_else(|| not_enabled("US financials need EDGAR_USER_AGENT on the server"))?;
                ("SEC EDGAR", edgar.fundamentals(&id.symbol).await.map_err(|e| upstream(format!("{e:#}")))?)
            }
            _ => return Err(not_enabled("financials exist for KRX and US stocks only")),
        };
        let _ = self.app.market.ensure_fresh(&id).await;
        let price = self.app.broker.book_view(&id, 1).and_then(|v| mid(&v.shadow_bids, &v.shadow_asks));
        let r = price.map(|p| crate::fundamentals::ratios(p, &f));
        Ok(FinancialsView {
            id: id.to_string(),
            source: source.into(),
            currency: f.currency.code().into(),
            annual: f.annual.iter().map(period_view).collect(),
            latest_quarter: f.latest_quarter.as_ref().map(period_view),
            shares_outstanding: f.shares_outstanding,
            eps_basis: if f.eps_computed { "computed" } else { "reported" }.into(),
            price,
            per: r.and_then(|r| r.per),
            pbr: r.and_then(|r| r.pbr),
        })
    }

    async fn list_filings(&self, id: String, since: Option<String>, limit: Option<u32>) -> zyris::Result<Vec<FilingView>> {
        let id = self.known(&id)?;
        let since = match since {
            Some(s) => chrono::NaiveDate::parse_from_str(s.trim(), "%Y-%m-%d").map_err(|_| bad("since must be YYYY-MM-DD"))?,
            None => self.app.broker.now().date_naive() - chrono::Duration::days(90),
        };
        let limit = limit.unwrap_or(20).clamp(1, 100) as usize;
        let filings = match id.venue {
            Venue::Krx => {
                let dart = self.app.dart.as_ref().ok_or_else(|| not_enabled("KRX filings need DART_API_KEY on the server"))?;
                dart.filings(&id.symbol, since, limit).await
            }
            Venue::Us => {
                let edgar = self.app.edgar.as_ref().ok_or_else(|| not_enabled("US filings need EDGAR_USER_AGENT on the server"))?;
                edgar.filings(&id.symbol, Some(since), limit).await
            }
            _ => return Err(not_enabled("filings exist for KRX and US stocks only")),
        }
        .map_err(|e| upstream(format!("{e:#}")))?;
        Ok(filings.into_iter().map(|f| FilingView { filing_id: f.id, title: f.title, form: f.form, date: f.date, url: f.url }).collect())
    }

    async fn get_filing(&self, id: String, filing_id: String, page: Option<u32>) -> zyris::Result<FilingText> {
        let id = self.known(&id)?;
        let text = match id.venue {
            Venue::Krx => self.app.dart.as_ref().ok_or_else(|| not_enabled("KRX filings need DART_API_KEY on the server"))?.filing_text(filing_id.trim()).await,
            Venue::Us => self.app.edgar.as_ref().ok_or_else(|| not_enabled("US filings need EDGAR_USER_AGENT on the server"))?.filing_text(&id.symbol, filing_id.trim()).await,
            _ => return Err(not_enabled("filings exist for KRX and US stocks only")),
        }
        .map_err(|e| upstream(format!("{e:#}")))?;
        let page = page.unwrap_or(1);
        let (chunk, pages) = crate::fundamentals::page_text(&text, page as usize).map_err(bad)?;
        Ok(FilingText { filing_id, page, pages: pages as u32, text: chunk })
    }
```

Wire it up in `src/cli.rs` `serve`: replace `App::new(...).await?` with the following:

```rust
    let dart = std::env::var("DART_API_KEY").ok().filter(|k| !k.trim().is_empty()).map(|k| crate::fundamentals::dart::DartClient::new(k.trim().into()));
    let edgar = std::env::var("EDGAR_USER_AGENT").ok().filter(|u| !u.trim().is_empty()).map(|u| crate::fundamentals::edgar::EdgarClient::new(u.trim().into()));
    tracing::info!(dart = dart.is_some(), edgar = edgar.is_some(), "fundamentals sources");
    let app = Arc::new(App::new(broker.clone(), store, market, FxCache::new()).await?.with_fundamentals(dart, edgar));
```

Add these lines to `USAGE`:
```
  DART_API_KEY          OpenDART key (KRX financials and filings)
  EDGAR_USER_AGENT      "name contact@email" for SEC EDGAR (US financials and filings)
```

Append this EDGAR live check to `tests/live.rs`:

```rust
#[tokio::test]
#[ignore]
async fn edgar_live() {
    let c = atrader::fundamentals::edgar::EdgarClient::new("ATrader test contact@example.com".into());
    let f = c.fundamentals("AAPL").await.unwrap();
    assert!(f.annual[0].revenue.unwrap() > rust_decimal::Decimal::from(100_000_000_000u64));
    let filings = c.filings("AAPL", None, 5).await.unwrap();
    assert_eq!(filings.len(), 5);
    let q = c.filings("AAPL", None, 50).await.unwrap().into_iter().find(|f| f.form == "10-Q").unwrap();
    let text = c.filing_text("AAPL", &q.id).await.unwrap();
    assert!(text.len() > 10_000 && text.contains("Apple"), "{}", &text[..200.min(text.len())]);
}
```

README: add a "Fundamentals" line under Running that names `DART_API_KEY` (issue at opendart.fss.or.kr) and `EDGAR_USER_AGENT`.

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test` (with `DATABASE_URL`), then `cargo test --test live edgar -- --ignored`.
Expected: all pass. EDGAR live returns AAPL financials and a 10-Q's text.

- [ ] **Step 5: Commit**

```bash
git add src tests README.md
git commit -m "Add financials and filings tools backed by DART and SEC EDGAR"
git log -1 --format=%B
```
