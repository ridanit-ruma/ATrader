//! Agent-defined alerts: conditions, a pure evaluator, and delivery to Attacca.

use std::collections::{HashMap, VecDeque};

use chrono::{DateTime, Duration, Utc};
use rust_decimal::Decimal;

use crate::broker::Fill;
use crate::domain::{InstrumentId, Trade, Venue};
use crate::venue::Calendar;

#[derive(Debug, Clone, PartialEq)]
pub enum Condition {
    PriceAbove { id: InstrumentId, price: Decimal },
    PriceBelow { id: InstrumentId, price: Decimal },
    /// Absolute % change over the window.
    Move { id: InstrumentId, pct: Decimal, window_minutes: u32 },
    /// Traded value in the window versus the average-day pace.
    VolumeSurge { id: InstrumentId, factor: Decimal, window_minutes: u32 },
    OrderFilled { id: Option<InstrumentId> },
    SessionOpen { venue: Venue },
    SessionClose { venue: Venue },
}

impl Condition {
    pub fn kind(&self) -> &'static str {
        match self {
            Condition::PriceAbove { .. } => "price_above",
            Condition::PriceBelow { .. } => "price_below",
            Condition::Move { .. } => "move",
            Condition::VolumeSurge { .. } => "volume_surge",
            Condition::OrderFilled { .. } => "order_filled",
            Condition::SessionOpen { .. } => "session_open",
            Condition::SessionClose { .. } => "session_close",
        }
    }

    pub fn instrument(&self) -> Option<&InstrumentId> {
        match self {
            Condition::PriceAbove { id, .. } | Condition::PriceBelow { id, .. } | Condition::Move { id, .. } | Condition::VolumeSurge { id, .. } => Some(id),
            Condition::OrderFilled { id } => id.as_ref(),
            Condition::SessionOpen { .. } | Condition::SessionClose { .. } => None,
        }
    }

    pub fn venue(&self) -> Option<Venue> {
        match self {
            Condition::SessionOpen { venue } | Condition::SessionClose { venue } => Some(*venue),
            _ => None,
        }
    }

    pub fn threshold(&self) -> Option<Decimal> {
        match self {
            Condition::PriceAbove { price, .. } | Condition::PriceBelow { price, .. } => Some(*price),
            Condition::Move { pct, .. } => Some(*pct),
            Condition::VolumeSurge { factor, .. } => Some(*factor),
            _ => None,
        }
    }

    pub fn window_minutes(&self) -> Option<u32> {
        match self {
            Condition::Move { window_minutes, .. } | Condition::VolumeSurge { window_minutes, .. } => Some(*window_minutes),
            _ => None,
        }
    }

    /// Rebuild from stored columns.
    pub fn from_parts(kind: &str, instrument: Option<InstrumentId>, venue: Option<Venue>, threshold: Option<Decimal>, window: Option<u32>) -> Option<Condition> {
        Some(match kind {
            "price_above" => Condition::PriceAbove { id: instrument?, price: threshold? },
            "price_below" => Condition::PriceBelow { id: instrument?, price: threshold? },
            "move" => Condition::Move { id: instrument?, pct: threshold?, window_minutes: window? },
            "volume_surge" => Condition::VolumeSurge { id: instrument?, factor: threshold?, window_minutes: window? },
            "order_filled" => Condition::OrderFilled { id: instrument },
            "session_open" => Condition::SessionOpen { venue: venue? },
            "session_close" => Condition::SessionClose { venue: venue? },
            _ => return None,
        })
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Alert {
    pub id: i64,
    pub account: String,
    pub generation: i32,
    pub condition: Condition,
    pub note: String,
    pub once: bool,
    pub created_at: DateTime<Utc>,
    pub last_fired_at: Option<DateTime<Utc>>,
}

pub const COOLDOWN: Duration = Duration::minutes(5);
const MAX_WINDOW: Duration = Duration::minutes(240);

/// An alert that fired, ready to deliver.
#[derive(Debug, Clone, PartialEq)]
pub struct Firing {
    pub alert_id: i64,
    pub account: String,
    /// What happened, without the account summary delivery adds.
    pub text: String,
    /// One-shot alerts turn off when they fire.
    pub deactivate: bool,
    /// The agent's note on the alert (empty for digests).
    pub note: String,
}

#[derive(Debug, Default)]
pub struct Watcher {
    alerts: Vec<Alert>,
    /// Recent prints per watched instrument: (time, price, value).
    history: HashMap<InstrumentId, VecDeque<(DateTime<Utc>, Decimal, Decimal)>>,
    open: HashMap<Venue, bool>,
}

impl Watcher {
    pub fn upsert(&mut self, alert: Alert) {
        self.remove(alert.id);
        self.alerts.push(alert);
    }

    pub fn remove(&mut self, id: i64) {
        self.alerts.retain(|a| a.id != id);
    }

    pub fn alerts(&self) -> &[Alert] {
        &self.alerts
    }

    /// Instruments price/move/volume alerts watch (they should stay subscribed).
    pub fn instruments(&self) -> Vec<InstrumentId> {
        let mut ids: Vec<InstrumentId> = self
            .alerts
            .iter()
            .filter(|a| !matches!(a.condition, Condition::OrderFilled { .. }))
            .filter_map(|a| a.condition.instrument().cloned())
            .collect();
        ids.sort_by_key(|i| i.to_string());
        ids.dedup();
        ids
    }

    /// Fire `hit` alerts that are off cooldown; one-shots are removed.
    fn fire(&mut self, now: DateTime<Utc>, hit: impl Fn(&Alert) -> Option<String>) -> Vec<Firing> {
        let mut out = Vec::new();
        for a in self.alerts.iter_mut() {
            if a.last_fired_at.is_some_and(|t| now - t < COOLDOWN) {
                continue;
            }
            if let Some(text) = hit(a) {
                a.last_fired_at = Some(now);
                out.push(Firing { alert_id: a.id, account: a.account.clone(), text, deactivate: a.once, note: a.note.clone() });
            }
        }
        let gone: Vec<i64> = out.iter().filter(|f| f.deactivate).map(|f| f.alert_id).collect();
        self.alerts.retain(|a| !gone.contains(&a.id));
        out
    }

    /// A real trade print. `adv` is the instrument's average daily traded value, if known.
    pub fn on_trade(&mut self, t: &Trade, adv: Option<Decimal>, now: DateTime<Utc>) -> Vec<Firing> {
        let watched = self.alerts.iter().any(|a| a.condition.instrument() == Some(&t.instrument) && !matches!(a.condition, Condition::OrderFilled { .. }));
        if !watched {
            return Vec::new();
        }
        let h = self.history.entry(t.instrument.clone()).or_default();
        h.push_back((t.at, t.price, t.price * t.qty));
        while h.front().is_some_and(|(at, _, _)| now - *at > MAX_WINDOW) {
            h.pop_front();
        }
        let history = h.clone();
        let id = t.instrument.clone();
        self.fire(now, |a| match &a.condition {
            Condition::PriceAbove { id: i, price } if *i == id && t.price >= *price => Some(format!("{id} traded at {} (at or above {price})", t.price)),
            Condition::PriceBelow { id: i, price } if *i == id && t.price <= *price => Some(format!("{id} traded at {} (at or below {price})", t.price)),
            Condition::Move { id: i, pct, window_minutes } if *i == id => {
                let since = now - Duration::minutes(i64::from(*window_minutes));
                let (_, base, _) = history.iter().find(|(at, _, _)| *at >= since)?;
                if base.is_zero() {
                    return None;
                }
                let change = (t.price / base - Decimal::ONE) * Decimal::ONE_HUNDRED;
                (change.abs() >= *pct).then(|| format!("{id} moved {}% in {window_minutes} min ({base} → {})", change.round_dp(2), t.price))
            }
            Condition::VolumeSurge { id: i, factor, window_minutes } if *i == id => {
                let adv = adv.filter(|v| *v > Decimal::ZERO)?;
                let since = now - Duration::minutes(i64::from(*window_minutes));
                let traded: Decimal = history.iter().filter(|(at, _, _)| *at >= since).map(|(_, _, v)| *v).sum();
                let pace = adv * Decimal::from(*window_minutes) / Decimal::from(1440);
                (traded >= *factor * pace).then(|| format!("{id} traded {} in {window_minutes} min, {}x the average pace", traded.round_dp(0), (traded / pace).round_dp(1)))
            }
            _ => None,
        })
    }

    pub fn on_fill(&mut self, f: &Fill, now: DateTime<Utc>) -> Vec<Firing> {
        self.fire(now, |a| match &a.condition {
            Condition::OrderFilled { id } if a.account == f.account && id.as_ref().is_none_or(|i| *i == f.instrument) => Some(format!(
                "order {} filled: {:?} {} {} at {} (fee {})",
                f.order_id, f.side, f.qty, f.instrument, f.price, f.fee
            )),
            _ => None,
        })
    }

    /// Clock tick: detect session opens and closes. The first tick only records the state.
    pub fn on_tick(&mut self, calendar: &Calendar, now: DateTime<Utc>) -> Vec<Firing> {
        let mut changed = Vec::new();
        for venue in [Venue::Krx, Venue::Us] {
            let open = calendar.is_open(venue, now);
            if let Some(was) = self.open.insert(venue, open) {
                if was != open {
                    changed.push((venue, open));
                }
            }
        }
        self.fire(now, |a| match &a.condition {
            Condition::SessionOpen { venue } if changed.contains(&(*venue, true)) => Some(format!("{} opened", venue.tag())),
            Condition::SessionClose { venue } if changed.contains(&(*venue, false)) => Some(format!("{} closed", venue.tag())),
            _ => None,
        })
    }
}

/// At most `PER_HOUR` deliveries per account per rolling hour.
#[derive(Debug, Default)]
pub struct Limiter {
    sent: HashMap<String, VecDeque<DateTime<Utc>>>,
}

pub const PER_HOUR: usize = 20;

impl Limiter {
    pub fn admit(&mut self, account: &str, now: DateTime<Utc>) -> bool {
        let q = self.sent.entry(account.to_string()).or_default();
        while q.front().is_some_and(|t| now - *t >= Duration::hours(1)) {
            q.pop_front();
        }
        if q.len() >= PER_HOUR {
            return false;
        }
        q.push_back(now);
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::broker::{Fill, Liquidity};
    use crate::domain::{Side, Trade};
    use crate::venue::Calendar;
    use chrono::{Duration, TimeZone};
    use rust_decimal_macros::dec;

    fn t0() -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 9, 23, 1, 0, 0).unwrap()
    }

    fn btc() -> InstrumentId {
        "UPBIT:KRW-BTC".parse().unwrap()
    }

    fn alert(id: i64, condition: Condition, once: bool) -> Alert {
        Alert { id, account: "a".into(), generation: 1, condition, note: "n".into(), once, created_at: t0(), last_fired_at: None }
    }

    fn trade(price: Decimal, qty: Decimal, at: DateTime<Utc>) -> Trade {
        Trade { instrument: btc(), price, qty, at }
    }

    #[test]
    fn price_levels_fire_once_or_with_cooldown() {
        let mut w = Watcher::default();
        w.upsert(alert(1, Condition::PriceAbove { id: btc(), price: dec!(100) }, true));
        w.upsert(alert(2, Condition::PriceBelow { id: btc(), price: dec!(90) }, false));
        assert!(w.on_trade(&trade(dec!(99), dec!(1), t0()), None, t0()).is_empty());
        let f = w.on_trade(&trade(dec!(100), dec!(1), t0()), None, t0());
        assert_eq!((f.len(), f[0].alert_id, f[0].deactivate), (1, 1, true));
        assert!(f[0].text.contains("100"));
        assert!(w.on_trade(&trade(dec!(101), dec!(1), t0()), None, t0()).is_empty()); // one-shot removed
        assert_eq!(w.alerts().len(), 1);

        let at = t0() + Duration::seconds(10);
        assert_eq!(w.on_trade(&trade(dec!(89), dec!(1), at), None, at).len(), 1);
        let soon = at + Duration::minutes(2);
        assert!(w.on_trade(&trade(dec!(88), dec!(1), soon), None, soon).is_empty()); // cooldown
        let later = at + COOLDOWN;
        assert_eq!(w.on_trade(&trade(dec!(88), dec!(1), later), None, later).len(), 1);
    }

    #[test]
    fn moves_are_measured_over_the_window() {
        let mut w = Watcher::default();
        w.upsert(alert(1, Condition::Move { id: btc(), pct: dec!(3), window_minutes: 10 }, true));
        w.on_trade(&trade(dec!(100), dec!(1), t0()), None, t0());
        let t5 = t0() + Duration::minutes(5);
        assert!(w.on_trade(&trade(dec!(102), dec!(1), t5), None, t5).is_empty());
        let t9 = t0() + Duration::minutes(9);
        let f = w.on_trade(&trade(dec!(96.9), dec!(1), t9), None, t9);
        assert_eq!(f.len(), 1);
        assert!(f[0].text.contains("-3.1"), "{}", f[0].text);
    }

    #[test]
    fn a_move_needs_history_from_the_start_of_the_window() {
        let mut w = Watcher::default();
        w.upsert(alert(1, Condition::Move { id: btc(), pct: dec!(1), window_minutes: 10 }, true));
        let t20 = t0() + Duration::minutes(20);
        w.on_trade(&trade(dec!(100), dec!(1), t0()), None, t0());
        // The only reference is 20 minutes old: compare against the oldest print inside the window instead.
        assert!(w.on_trade(&trade(dec!(150), dec!(1), t20), None, t20).is_empty());
        let t21 = t20 + Duration::minutes(1);
        assert_eq!(w.on_trade(&trade(dec!(152), dec!(1), t21), None, t21).len(), 1);
    }

    #[test]
    fn volume_surges_compare_with_the_daily_pace() {
        let mut w = Watcher::default();
        w.upsert(alert(1, Condition::VolumeSurge { id: btc(), factor: dec!(3), window_minutes: 60 }, true));
        // ADV 2,400 per day = 100 per hour; 3x = 300 in the hour.
        let adv = Some(dec!(2400));
        assert!(w.on_trade(&trade(dec!(100), dec!(2), t0()), adv, t0()).is_empty()); // 200
        assert!(w.on_trade(&trade(dec!(100), dec!(1), t0()), None, t0()).is_empty()); // no ADV known: never fires
        assert_eq!(w.on_trade(&trade(dec!(100), dec!(0.5), t0()), adv, t0()).len(), 1); // 350
    }

    #[test]
    fn fills_match_account_and_instrument() {
        let mut w = Watcher::default();
        w.upsert(alert(1, Condition::OrderFilled { id: None }, false));
        w.upsert(alert(2, Condition::OrderFilled { id: Some("UPBIT:KRW-ETH".parse().unwrap()) }, false));
        let fill = |account: &str| Fill {
            order_id: 7,
            account: account.into(),
            instrument: btc(),
            side: Side::Buy,
            qty: dec!(0.1),
            notional: dec!(10000000),
            price: dec!(100000000),
            fee: dec!(5000),
            tax: dec!(0),
            realized_pnl: None,
            liquidity: Liquidity::Maker,
            at: t0(),
        };
        let f = w.on_fill(&fill("a"), t0());
        assert_eq!(f.iter().map(|x| x.alert_id).collect::<Vec<_>>(), vec![1]);
        assert!(f[0].text.contains("order 7"));
        assert!(w.on_fill(&fill("b"), t0()).is_empty());
    }

    #[test]
    fn sessions_fire_on_transitions_only() {
        let mut w = Watcher::default();
        w.upsert(alert(1, Condition::SessionOpen { venue: Venue::Krx }, false));
        w.upsert(alert(2, Condition::SessionClose { venue: Venue::Krx }, false));
        let cal = Calendar::default();
        let before = Utc.with_ymd_and_hms(2026, 9, 22, 23, 59, 0).unwrap(); // 08:59 KST
        assert!(w.on_tick(&cal, before).is_empty()); // first tick only learns the state
        let open = Utc.with_ymd_and_hms(2026, 9, 23, 0, 0, 30).unwrap();
        assert_eq!(w.on_tick(&cal, open).iter().map(|f| f.alert_id).collect::<Vec<_>>(), vec![1]);
        assert!(w.on_tick(&cal, open + Duration::minutes(1)).is_empty());
        let close = Utc.with_ymd_and_hms(2026, 9, 23, 6, 30, 30).unwrap();
        assert_eq!(w.on_tick(&cal, close).iter().map(|f| f.alert_id).collect::<Vec<_>>(), vec![2]);
    }

    #[test]
    fn limiter_allows_twenty_an_hour_per_account() {
        let mut l = Limiter::default();
        for i in 0..20 {
            assert!(l.admit("a", t0() + Duration::seconds(i)));
        }
        assert!(!l.admit("a", t0() + Duration::minutes(30)));
        assert!(l.admit("b", t0() + Duration::minutes(30)));
        assert!(l.admit("a", t0() + Duration::minutes(61)));
    }

    #[test]
    fn instruments_lists_what_alerts_watch() {
        let mut w = Watcher::default();
        w.upsert(alert(1, Condition::PriceAbove { id: btc(), price: dec!(1) }, true));
        w.upsert(alert(2, Condition::SessionOpen { venue: Venue::Us }, true));
        assert_eq!(w.instruments(), vec![btc()]);
        w.remove(1);
        assert!(w.instruments().is_empty());
    }
}
