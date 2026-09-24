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
