use std::fmt;
use std::str::FromStr;
use std::sync::{Arc, Mutex};

use chrono::{DateTime, Utc};
use rust_decimal::Decimal;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Venue {
    Krx,
    Us,
    Upbit,
    Binance,
}

impl Venue {
    pub fn tag(self) -> &'static str {
        match self {
            Venue::Krx => "KRX",
            Venue::Us => "US",
            Venue::Upbit => "UPBIT",
            Venue::Binance => "BINANCE",
        }
    }

    pub fn from_tag(s: &str) -> Option<Venue> {
        [Venue::Krx, Venue::Us, Venue::Upbit, Venue::Binance].into_iter().find(|v| v.tag() == s)
    }

    pub fn currency(self) -> Currency {
        match self {
            Venue::Krx | Venue::Upbit => Currency::Krw,
            Venue::Us => Currency::Usd,
            Venue::Binance => Currency::Usdt,
        }
    }

    /// Stock venues trade in sessions; crypto venues trade around the clock.
    pub fn has_session(self) -> bool {
        matches!(self, Venue::Krx | Venue::Us)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Currency {
    Krw,
    Usd,
    Usdt,
}

impl Currency {
    pub fn code(self) -> &'static str {
        match self {
            Currency::Krw => "KRW",
            Currency::Usd => "USD",
            Currency::Usdt => "USDT",
        }
    }

    pub fn from_code(s: &str) -> Option<Currency> {
        [Currency::Krw, Currency::Usd, Currency::Usdt].into_iter().find(|c| c.code() == s)
    }

    /// Decimal places of the currency's minor unit.
    pub fn decimals(self) -> u32 {
        match self {
            Currency::Krw => 0,
            Currency::Usd => 2,
            Currency::Usdt => 8,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct InstrumentId {
    pub venue: Venue,
    pub symbol: String,
}

impl fmt::Display for InstrumentId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}:{}", self.venue.tag(), self.symbol)
    }
}

impl FromStr for InstrumentId {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, String> {
        let (v, symbol) = s.split_once(':').ok_or_else(|| format!("expected VENUE:SYMBOL, got {s:?}"))?;
        let venue = Venue::from_tag(v).ok_or_else(|| format!("unknown venue {v:?}"))?;
        if symbol.is_empty() {
            return Err("empty symbol".into());
        }
        Ok(InstrumentId { venue, symbol: symbol.to_string() })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Side {
    Buy,
    Sell,
}

impl Side {
    pub fn sign(self) -> f64 {
        match self {
            Side::Buy => 1.0,
            Side::Sell => -1.0,
        }
    }

    pub fn opposite(self) -> Side {
        match self {
            Side::Buy => Side::Sell,
            Side::Sell => Side::Buy,
        }
    }

    pub fn code(self) -> &'static str {
        match self {
            Side::Buy => "buy",
            Side::Sell => "sell",
        }
    }

    pub fn from_code(s: &str) -> Option<Side> {
        match s {
            "buy" => Some(Side::Buy),
            "sell" => Some(Side::Sell),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Level {
    pub price: Decimal,
    pub qty: Decimal,
}

/// A real order book snapshot. Bids best (highest) first, asks best (lowest) first.
#[derive(Debug, Clone, PartialEq)]
pub struct Book {
    pub instrument: InstrumentId,
    pub bids: Vec<Level>,
    pub asks: Vec<Level>,
    pub prev_close: Option<Decimal>,
    pub received_at: DateTime<Utc>,
}

/// One real trade print.
#[derive(Debug, Clone, PartialEq)]
pub struct Trade {
    pub instrument: InstrumentId,
    pub price: Decimal,
    pub qty: Decimal,
    pub at: DateTime<Utc>,
}

pub trait Clock: Send + Sync {
    fn now(&self) -> DateTime<Utc>;
}

pub struct SystemClock;

impl Clock for SystemClock {
    fn now(&self) -> DateTime<Utc> {
        Utc::now()
    }
}

/// A clock tests move by hand.
#[derive(Clone)]
pub struct ManualClock(Arc<Mutex<DateTime<Utc>>>);

impl ManualClock {
    pub fn new(t: DateTime<Utc>) -> Self {
        ManualClock(Arc::new(Mutex::new(t)))
    }

    pub fn set(&self, t: DateTime<Utc>) {
        *self.0.lock().unwrap() = t;
    }

    pub fn advance(&self, d: chrono::Duration) {
        *self.0.lock().unwrap() += d;
    }
}

impl Clock for ManualClock {
    fn now(&self) -> DateTime<Utc> {
        *self.0.lock().unwrap()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn instrument_id_round_trips() {
        let id: InstrumentId = "UPBIT:KRW-BTC".parse().unwrap();
        assert_eq!(id.venue, Venue::Upbit);
        assert_eq!(id.symbol, "KRW-BTC");
        assert_eq!(id.to_string(), "UPBIT:KRW-BTC");
    }

    #[test]
    fn instrument_id_rejects_garbage() {
        assert!("AAPL".parse::<InstrumentId>().is_err());
        assert!("NASDAQ:AAPL".parse::<InstrumentId>().is_err());
        assert!("US:".parse::<InstrumentId>().is_err());
    }

    #[test]
    fn venues_know_their_currency_and_session() {
        assert_eq!(Venue::Krx.currency(), Currency::Krw);
        assert_eq!(Venue::Binance.currency(), Currency::Usdt);
        assert!(Venue::Us.has_session());
        assert!(!Venue::Upbit.has_session());
        assert_eq!(Currency::from_code("USD"), Some(Currency::Usd));
        assert_eq!(Currency::Krw.decimals(), 0);
    }

    #[test]
    fn manual_clock_advances() {
        let t0 = Utc::now();
        let c = ManualClock::new(t0);
        c.advance(chrono::Duration::seconds(5));
        assert_eq!(c.now(), t0 + chrono::Duration::seconds(5));
    }
}
