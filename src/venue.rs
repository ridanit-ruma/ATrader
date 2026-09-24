use rust_decimal::{Decimal, RoundingStrategy};
use rust_decimal_macros::dec;

use crate::domain::{InstrumentId, Side, Venue};

#[derive(Debug, Clone, PartialEq)]
pub enum TickRule {
    /// KRX unified price-band table (KOSPI and KOSDAQ, since 2023-01-25).
    Krx,
    /// US equities: $0.01 at or above $1, $0.0001 below.
    Us,
    /// Upbit KRW market table (docs.upbit.com "KRW market info", checked 2026-09-24).
    // ponytail: Upbit's per-coin tick exceptions are not modelled; add them when a listed coin trips INVALID_TICK.
    Upbit,
    /// One tick size for every price (crypto, from exchange metadata).
    Fixed(Decimal),
}

/// (upper bound exclusive, tick) pairs; 1,000 above the last bound.
const KRX_TICKS: [(i64, i64); 6] =
    [(2_000, 1), (5_000, 5), (20_000, 10), (50_000, 50), (200_000, 100), (500_000, 500)];

/// (lower bound inclusive, tick), highest first; below the last bound the tick is 0.00000001.
const UPBIT_TICKS: [(Decimal, Decimal); 14] = [
    (dec!(1000000), dec!(1000)),
    (dec!(500000), dec!(500)),
    (dec!(100000), dec!(100)),
    (dec!(50000), dec!(50)),
    (dec!(10000), dec!(10)),
    (dec!(5000), dec!(5)),
    (dec!(100), dec!(1)),
    (dec!(10), dec!(0.1)),
    (dec!(1), dec!(0.01)),
    (dec!(0.1), dec!(0.001)),
    (dec!(0.01), dec!(0.0001)),
    (dec!(0.001), dec!(0.00001)),
    (dec!(0.0001), dec!(0.000001)),
    (dec!(0.00001), dec!(0.0000001)),
];

impl TickRule {
    pub fn size_at(&self, price: Decimal) -> Decimal {
        match self {
            TickRule::Krx => KRX_TICKS
                .iter()
                .find(|(upper, _)| price < Decimal::from(*upper))
                .map_or(dec!(1000), |(_, t)| Decimal::from(*t)),
            TickRule::Us => {
                if price >= Decimal::ONE {
                    dec!(0.01)
                } else {
                    dec!(0.0001)
                }
            }
            TickRule::Upbit => UPBIT_TICKS
                .iter()
                .find(|(lower, _)| price >= *lower)
                .map_or(dec!(0.00000001), |(_, t)| *t),
            TickRule::Fixed(t) => *t,
        }
    }

    pub fn floor(&self, price: Decimal) -> Decimal {
        let t = self.size_at(price);
        (price / t).floor() * t
    }

    pub fn ceil(&self, price: Decimal) -> Decimal {
        let t = self.size_at(price);
        (price / t).ceil() * t
    }

    pub fn round(&self, price: Decimal) -> Decimal {
        let t = self.size_at(price);
        (price / t).round() * t
    }

    pub fn is_valid(&self, price: Decimal) -> bool {
        price > Decimal::ZERO && (price % self.size_at(price)).is_zero()
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct LotRule {
    pub step: Decimal,
    pub min_qty: Decimal,
    pub min_notional: Decimal,
}

impl LotRule {
    pub fn is_valid(&self, qty: Decimal, price: Decimal) -> bool {
        qty > Decimal::ZERO
            && (qty % self.step).is_zero()
            && qty >= self.min_qty
            && qty * price >= self.min_notional
    }
}

/// Integer shares, no minimum notional (KRX and US in v1).
pub fn whole_shares() -> LotRule {
    LotRule { step: Decimal::ONE, min_qty: Decimal::ONE, min_notional: Decimal::ZERO }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Instrument {
    pub id: InstrumentId,
    pub name: String,
    pub tick: TickRule,
    pub lot: LotRule,
    pub tradable: bool,
}

/// KRX daily price limit: ±30% of the previous close, snapped inward to valid ticks.
/// Other venues have none.
pub fn price_band(venue: Venue, tick: &TickRule, prev_close: Option<Decimal>) -> Option<(Decimal, Decimal)> {
    if venue != Venue::Krx {
        return None;
    }
    let p = prev_close?;
    Some((tick.ceil(p * dec!(0.7)), tick.floor(p * dec!(1.3))))
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct FeeSchedule {
    pub commission_bps: Decimal,
    pub sell_tax_bps: Decimal,
    pub sell_reg_fee_bps: Decimal,
}

impl FeeSchedule {
    /// Defaults per spec §3. The KRX securities transaction tax (20 bps) follows the law in force
    /// for 2026; check it when the law changes.
    // ponytail: hard-coded defaults; fees.toml with effective dates lands in Phase 8.
    pub fn default_for(venue: Venue) -> Self {
        let (commission_bps, sell_tax_bps, sell_reg_fee_bps) = match venue {
            Venue::Krx => (dec!(1.5), dec!(20), dec!(0)),
            Venue::Us => (dec!(0), dec!(0), dec!(0.278)),
            Venue::Upbit => (dec!(5), dec!(0), dec!(0)),
            Venue::Binance => (dec!(10), dec!(0), dec!(0)),
        };
        FeeSchedule { commission_bps, sell_tax_bps, sell_reg_fee_bps }
    }

    /// (fee, tax) for one execution of `notional`, truncated to `dp` decimal places (brokers drop
    /// the sub-unit remainder). Truncation also keeps a fee within the commission reserved for it.
    pub fn cost(&self, side: Side, notional: Decimal, dp: u32) -> (Decimal, Decimal) {
        let bps = |b: Decimal| notional * b / dec!(10000);
        let mut fee = bps(self.commission_bps);
        let mut tax = Decimal::ZERO;
        if side == Side::Sell {
            fee += bps(self.sell_reg_fee_bps);
            tax = bps(self.sell_tax_bps);
        }
        (
            fee.round_dp_with_strategy(dp, RoundingStrategy::ToZero),
            tax.round_dp_with_strategy(dp, RoundingStrategy::ToZero),
        )
    }
}

use std::collections::{HashMap, HashSet};

use chrono::{DateTime, Datelike, Duration, NaiveDate, NaiveTime, TimeZone, Utc, Weekday};
use chrono_tz::Tz;

/// Regular session in venue-local time. `None` = trades around the clock.
fn session(venue: Venue) -> Option<(Tz, NaiveTime, NaiveTime)> {
    let t = |h, m| NaiveTime::from_hms_opt(h, m, 0).unwrap();
    match venue {
        Venue::Krx => Some((chrono_tz::Asia::Seoul, t(9, 0), t(15, 30))),
        Venue::Us => Some((chrono_tz::America::New_York, t(9, 30), t(16, 0))),
        Venue::Upbit | Venue::Binance => None,
    }
}

#[derive(Debug, Clone, Default)]
pub struct Calendar {
    holidays: HashMap<Venue, HashSet<NaiveDate>>,
}

impl Calendar {
    pub fn new(holidays: HashMap<Venue, HashSet<NaiveDate>>) -> Self {
        Calendar { holidays }
    }

    /// Parse `VENUE = ["YYYY-MM-DD", ...]` tables (see `holidays.toml`).
    pub fn from_toml(src: &str) -> anyhow::Result<Self> {
        let raw: HashMap<String, Vec<NaiveDate>> = toml::from_str(src)?;
        let mut holidays = HashMap::new();
        for (tag, days) in raw {
            let venue = Venue::from_tag(&tag).ok_or_else(|| anyhow::anyhow!("unknown venue {tag:?}"))?;
            holidays.insert(venue, days.into_iter().collect());
        }
        Ok(Calendar { holidays })
    }

    fn is_trading_day(&self, venue: Venue, d: NaiveDate) -> bool {
        !matches!(d.weekday(), Weekday::Sat | Weekday::Sun)
            && !self.holidays.get(&venue).is_some_and(|h| h.contains(&d))
    }

    pub fn is_open(&self, venue: Venue, now: DateTime<Utc>) -> bool {
        let Some((tz, open, close)) = session(venue) else { return true };
        let local = now.with_timezone(&tz);
        self.is_trading_day(venue, local.date_naive()) && local.time() >= open && local.time() < close
    }

    pub fn next_open(&self, venue: Venue, now: DateTime<Utc>) -> Option<DateTime<Utc>> {
        let (tz, open, _) = session(venue)?;
        let today = now.with_timezone(&tz).date_naive();
        (0..15)
            .map(|i| today + Duration::days(i))
            .filter(|d| self.is_trading_day(venue, *d))
            .filter_map(|d| tz.from_local_datetime(&d.and_time(open)).single())
            .map(|t| t.with_timezone(&Utc))
            .find(|t| *t > now)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rust_decimal_macros::dec;

    #[test]
    fn krx_ticks_follow_price_bands() {
        let t = TickRule::Krx;
        assert_eq!(t.size_at(dec!(1999)), dec!(1));
        assert_eq!(t.size_at(dec!(2000)), dec!(5));
        assert_eq!(t.size_at(dec!(49950)), dec!(50));
        assert_eq!(t.size_at(dec!(50000)), dec!(100));
        assert_eq!(t.size_at(dec!(600000)), dec!(1000));
        assert!(!t.is_valid(dec!(50050)));
        assert!(t.is_valid(dec!(50100)));
        assert_eq!((t.floor(dec!(50050)), t.ceil(dec!(50050))), (dec!(50000), dec!(50100)));
    }

    #[test]
    fn us_ticks() {
        let t = TickRule::Us;
        assert_eq!(t.size_at(dec!(12.345)), dec!(0.01));
        assert_eq!(t.size_at(dec!(0.5)), dec!(0.0001));
        assert!(t.is_valid(dec!(12.34)));
        assert!(!t.is_valid(dec!(12.345)));
        assert!(!t.is_valid(dec!(0)));
    }

    #[test]
    fn lot_rules() {
        let shares = whole_shares();
        assert!(shares.is_valid(dec!(1), dec!(100)));
        assert!(!shares.is_valid(dec!(0), dec!(100)));
        assert!(!shares.is_valid(dec!(-1), dec!(100)));
        assert!(!shares.is_valid(dec!(1.5), dec!(100)));
        let btc = LotRule { step: dec!(0.00000001), min_qty: dec!(0.00000001), min_notional: dec!(5000) };
        assert!(btc.is_valid(dec!(0.0001), dec!(100000000)));
        assert!(!btc.is_valid(dec!(0.00001), dec!(100000000)));
        assert!(!btc.is_valid(dec!(0.000000001), dec!(100000000)));
    }

    #[test]
    fn krx_price_band_is_thirty_percent() {
        assert_eq!(price_band(Venue::Krx, &TickRule::Krx, Some(dec!(70000))), Some((dec!(49000), dec!(91000))));
        assert_eq!(price_band(Venue::Krx, &TickRule::Krx, None), None);
        assert_eq!(price_band(Venue::Us, &TickRule::Us, Some(dec!(100))), None);
    }

    #[test]
    fn fees_and_taxes() {
        let krx = FeeSchedule::default_for(Venue::Krx);
        assert_eq!(krx.cost(Side::Sell, dec!(1000000), 0), (dec!(150), dec!(2000)));
        assert_eq!(krx.cost(Side::Buy, dec!(1000000), 0), (dec!(150), dec!(0)));
        let us = FeeSchedule::default_for(Venue::Us);
        assert_eq!(us.cost(Side::Sell, dec!(10000), 2), (dec!(0.27), dec!(0)));
        let upbit = FeeSchedule::default_for(Venue::Upbit);
        assert_eq!(upbit.cost(Side::Buy, dec!(1000000), 0), (dec!(500), dec!(0)));
    }

    use chrono::{DateTime, NaiveDate, TimeZone, Utc};
    use std::collections::{HashMap, HashSet};

    fn utc(y: i32, m: u32, d: u32, h: u32, mi: u32) -> DateTime<Utc> {
        Utc.with_ymd_and_hms(y, m, d, h, mi, 0).unwrap()
    }

    #[test]
    fn krx_session_in_seoul_time() {
        let c = Calendar::default();
        assert!(c.is_open(Venue::Krx, utc(2026, 9, 23, 1, 0))); // Wed 10:00 KST
        assert!(!c.is_open(Venue::Krx, utc(2026, 9, 22, 23, 59))); // Wed 08:59 KST
        assert!(!c.is_open(Venue::Krx, utc(2026, 9, 23, 6, 30))); // Wed 15:30 KST
        assert!(!c.is_open(Venue::Krx, utc(2026, 9, 26, 1, 0))); // Sat
    }

    #[test]
    fn next_open_skips_weekend_and_holidays() {
        let holiday = NaiveDate::from_ymd_opt(2026, 9, 28).unwrap();
        let c = Calendar::new(HashMap::from([(Venue::Krx, HashSet::from([holiday]))]));
        // Fri 16:00 KST -> Mon is a holiday -> Tue 09:00 KST.
        assert_eq!(c.next_open(Venue::Krx, utc(2026, 9, 25, 7, 0)), Some(utc(2026, 9, 29, 0, 0)));
        assert!(!c.is_open(Venue::Krx, utc(2026, 9, 28, 1, 0)));
    }

    #[test]
    fn us_session_tracks_dst() {
        let c = Calendar::default();
        assert!(c.is_open(Venue::Us, utc(2026, 9, 23, 13, 30))); // 09:30 EDT
        assert!(!c.is_open(Venue::Us, utc(2026, 9, 23, 13, 29)));
        assert!(c.is_open(Venue::Us, utc(2026, 11, 2, 14, 30))); // 09:30 EST
        assert!(!c.is_open(Venue::Us, utc(2026, 11, 2, 13, 30))); // 08:30 EST
        assert!(!c.is_open(Venue::Us, utc(2026, 11, 2, 21, 0))); // 16:00 EST
    }

    #[test]
    fn crypto_is_always_open() {
        let c = Calendar::default();
        assert!(c.is_open(Venue::Upbit, utc(2026, 9, 26, 3, 0)));
        assert_eq!(c.next_open(Venue::Binance, utc(2026, 9, 26, 3, 0)), None);
    }

    #[test]
    fn holidays_parse_from_toml() {
        let c = Calendar::from_toml("KRX = [\"2026-09-25\"]\nUS = []").unwrap();
        assert!(!c.is_open(Venue::Krx, utc(2026, 9, 25, 1, 0)));
        assert!(Calendar::from_toml("NASDAQ = []").is_err());
    }

    #[test]
    fn shipped_holiday_file_parses() {
        Calendar::from_toml(include_str!("../holidays.toml")).unwrap();
    }

    #[test]
    fn upbit_krw_ticks() {
        let t = TickRule::Upbit;
        assert_eq!(t.size_at(dec!(114851000)), dec!(1000));
        assert_eq!(t.size_at(dec!(1000000)), dec!(1000));
        assert_eq!(t.size_at(dec!(750000)), dec!(500));
        assert_eq!(t.size_at(dec!(120000)), dec!(100));
        assert_eq!(t.size_at(dec!(60000)), dec!(50));
        assert_eq!(t.size_at(dec!(12000)), dec!(10));
        assert_eq!(t.size_at(dec!(7000)), dec!(5));
        assert_eq!(t.size_at(dec!(3000)), dec!(1));
        assert_eq!(t.size_at(dec!(500)), dec!(1));
        assert_eq!(t.size_at(dec!(50)), dec!(0.1));
        assert_eq!(t.size_at(dec!(5)), dec!(0.01));
        assert_eq!(t.size_at(dec!(0.5)), dec!(0.001));
        assert_eq!(t.size_at(dec!(0.000001)), dec!(0.00000001));
        assert!(t.is_valid(dec!(114851000)));
        assert!(!t.is_valid(dec!(114851500)));
    }
}
