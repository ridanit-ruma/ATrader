//! Scheduled market briefings: without them the agent only acts when someone talks to it. At each
//! KRX and US session open and close, and every few hours in between, the account's alert
//! conversation gets the account, its positions and the session's movers.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration as StdDuration;

use chrono::{DateTime, Duration, Utc};

use super::deliver::Notifier;
use crate::app::App;
use crate::domain::Venue;
use crate::tools::{Trader, TraderTools};

/// Venues with sessions; crypto never closes, so it has no open or close to brief on.
const VENUES: [Venue; 2] = [Venue::Krx, Venue::Us];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Cadence {
    Off,
    /// Session open and close only.
    Edges,
    /// Open, close, and every this many hours in between.
    Hours(i64),
}

impl Cadence {
    pub fn parse(s: &str) -> Option<Cadence> {
        match s {
            "off" => Some(Cadence::Off),
            "edges" => Some(Cadence::Edges),
            "1h" => Some(Cadence::Hours(1)),
            "2h" => Some(Cadence::Hours(2)),
            "4h" => Some(Cadence::Hours(4)),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Moment {
    Opened(Venue),
    Closed(Venue),
    During(Venue),
}

/// What is due when. In memory: a restart neither repeats an open nor briefs the moment it starts.
#[derive(Default)]
pub struct Schedule {
    open: HashMap<Venue, bool>,
    last: HashMap<(String, Venue), DateTime<Utc>>,
}

impl Schedule {
    /// The briefings due at `now`, per account.
    pub fn tick(&mut self, now: DateTime<Utc>, is_open: impl Fn(Venue) -> bool, accounts: &[(String, Cadence)]) -> Vec<(String, Vec<Moment>)> {
        let mut due: HashMap<&str, Vec<Moment>> = HashMap::new();
        for venue in VENUES {
            let open = is_open(venue);
            let was = self.open.insert(venue, open);
            for (account, cadence) in accounts {
                if *cadence == Cadence::Off {
                    continue;
                }
                let key = (account.clone(), venue);
                let moment = match was {
                    Some(false) if open => {
                        self.last.insert(key, now);
                        Some(Moment::Opened(venue))
                    }
                    Some(true) if !open => {
                        self.last.remove(&key);
                        Some(Moment::Closed(venue))
                    }
                    _ if open => match cadence {
                        Cadence::Hours(h) => {
                            let last = self.last.entry(key).or_insert(now);
                            (now - *last >= Duration::hours(*h)).then(|| {
                                *last = now;
                                Moment::During(venue)
                            })
                        }
                        _ => None,
                    },
                    _ => None,
                };
                if let Some(m) = moment {
                    due.entry(account).or_default().push(m);
                }
            }
        }
        let mut out: Vec<(String, Vec<Moment>)> = due.into_iter().map(|(a, m)| (a.to_string(), m)).collect();
        out.sort_by(|a, b| a.0.cmp(&b.0));
        out
    }
}

fn describe(m: &Moment) -> String {
    match m {
        Moment::Opened(v) => format!("{} session opened", v.tag()),
        Moment::Closed(v) => format!("{} session closed", v.tag()),
        Moment::During(v) => format!("{} session update", v.tag()),
    }
}

/// The briefing text for `account`: why it was sent, the account, its positions, and each open
/// session's movers. Nothing here fails: missing parts are left out.
pub async fn briefing_text(app: &Arc<App>, account: &str, moments: &[Moment]) -> String {
    let t = TraderTools::new(app.clone());
    let why = if moments.is_empty() { "requested from the dashboard".to_string() } else { moments.iter().map(describe).collect::<Vec<_>>().join(", ") };
    let mut out = format!("[ATrader briefing] {why}.");
    if let Ok(s) = t.get_account(Some(account.to_string())).await {
        let cash: Vec<String> = s.cash.iter().map(|c| format!("{} {}", c.currency, c.balance.round_dp(2))).collect();
        out.push_str(&format!("\nAccount {} ({}): equity ₩{}, cash {}.", s.name, s.id, s.equity_krw.round_dp(0), cash.join(", ")));
    }
    if let Ok(positions) = t.get_positions(Some(account.to_string())).await {
        if positions.is_empty() {
            out.push_str("\nNo positions.");
        }
        for p in positions {
            let price = p.price.map(|x| x.to_string()).unwrap_or_else(|| "?".into());
            out.push_str(&format!("\n- {} {}: {} @ avg {}, now {} ({:+}%, {} {})", p.instrument, p.name, p.qty, p.avg_cost, price, p.unrealized_pct.round_dp(2), p.unrealized_pnl.round_dp(2), p.currency));
        }
    }
    let venues: Vec<Venue> = if moments.is_empty() {
        VENUES.to_vec()
    } else {
        moments.iter().filter_map(|m| match m {
            Moment::Opened(v) | Moment::During(v) => Some(*v),
            Moment::Closed(_) => None,
        }).collect()
    };
    for venue in venues {
        for ranking in ["gainers", "losers", "value"] {
            if let Ok(rows) = t.screen(venue.tag().to_string(), ranking.to_string(), Some(5)).await {
                let list: Vec<String> = rows.iter().map(|r| format!("{} {} ({:+}%)", r.id, r.name, r.change_pct.round_dp(2))).collect();
                if !list.is_empty() {
                    out.push_str(&format!("\n{} top {ranking}: {}", venue.tag(), list.join(", ")));
                }
            }
        }
    }
    out.push_str("\nReview the account and act if the plan calls for it; give a reason with every order.");
    out
}

/// Every 30 s, send whatever briefings are due to each account's alert conversation.
pub async fn briefing_loop(app: Arc<App>, notifier: Arc<dyn Notifier>) {
    let mut schedule = Schedule::default();
    let mut tick = tokio::time::interval(StdDuration::from_secs(30));
    loop {
        tick.tick().await;
        let targets = match app.store.briefing_targets().await {
            Ok(t) => t,
            Err(e) => {
                tracing::warn!(error = %e, "could not read briefing settings");
                continue;
            }
        };
        let accounts: Vec<(String, Cadence)> = targets.iter().map(|(a, c, _)| (a.clone(), Cadence::parse(c).unwrap_or(Cadence::Off))).collect();
        let calendar = app.broker.calendar();
        let now = app.broker.now();
        for (account, moments) in schedule.tick(now, |v| calendar.is_open(v, now), &accounts) {
            let Some(session) = targets.iter().find(|t| t.0 == account).and_then(|t| t.2.clone()) else { continue };
            let text = briefing_text(&app, &account, &moments).await;
            if let Err(e) = notifier.send(&session, &text).await {
                tracing::warn!(error = %e, %account, "briefing delivery failed");
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    #[test]
    fn briefs_on_open_close_and_every_few_hours_between() {
        let t0 = Utc.with_ymd_and_hms(2026, 9, 28, 0, 0, 0).unwrap();
        let at = |m: i64| t0 + Duration::minutes(m);
        let accounts = vec![("a".to_string(), Cadence::Hours(2)), ("e".to_string(), Cadence::Edges), ("x".to_string(), Cadence::Off)];
        let mut s = Schedule::default();
        let krx = |open: bool| move |v: Venue| v == Venue::Krx && open;

        assert!(s.tick(at(0), krx(false), &accounts).is_empty(), "first look only learns the state");
        let opened = s.tick(at(1), krx(true), &accounts);
        assert_eq!(opened, vec![("a".into(), vec![Moment::Opened(Venue::Krx)]), ("e".into(), vec![Moment::Opened(Venue::Krx)])]);
        assert!(s.tick(at(60), krx(true), &accounts).is_empty());
        assert_eq!(s.tick(at(121), krx(true), &accounts), vec![("a".into(), vec![Moment::During(Venue::Krx)])]);
        assert!(s.tick(at(200), krx(true), &accounts).is_empty());
        let closed = s.tick(at(390), krx(false), &accounts);
        assert_eq!(closed.len(), 2);
        assert!(s.tick(at(600), krx(false), &accounts).is_empty(), "nothing while closed");

        // Starting mid-session briefs neither at once nor as an open.
        let mut fresh = Schedule::default();
        assert!(fresh.tick(at(0), krx(true), &accounts).is_empty());
        assert_eq!(fresh.tick(at(121), krx(true), &accounts), vec![("a".into(), vec![Moment::During(Venue::Krx)])]);
    }
}
