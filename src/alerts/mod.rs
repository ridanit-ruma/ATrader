//! Agent-defined alerts: conditions, a pure evaluator, and delivery to Attacca.

use chrono::{DateTime, Utc};
use rust_decimal::Decimal;

use crate::domain::{InstrumentId, Venue};

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
