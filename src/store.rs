//! Postgres persistence. Ledger entries are append-only; a portfolio is rebuilt by replaying
//! deposits and fills through the same `Portfolio` code the broker uses.

use std::collections::HashMap;

use chrono::{DateTime, Utc};
use rust_decimal::Decimal;
use sqlx::{PgPool, Postgres, Row, Transaction};

use crate::broker::{Conversion, Fill, Liquidity, Order, OrderRequest, OrderStatus, OrderType, Tif};
use crate::domain::{Currency, InstrumentId, Side};
use crate::ledger::Portfolio;
use crate::sim::Size;
use crate::candles::Candle;
use crate::alerts::{Alert, Condition};
use crate::domain::Venue;
use crate::performance::{Snapshot, SnapshotKind};

#[derive(Debug, Clone, PartialEq)]
pub struct AccountRow {
    pub id: String,
    pub name: String,
    /// The Attacca agent that trades this account; `None` hides it from tools.
    pub agent_id: Option<String>,
    pub generation: i32,
}

fn parse_order(r: &sqlx::postgres::PgRow) -> sqlx::Result<Order> {
    let text = |col: &str| -> String { r.get(col) };
    let bad = |col: &str, v: String| decode_err(format!("unknown {col} {v:?}"));
    let side = Side::from_code(&text("side")).ok_or_else(|| bad("side", text("side")))?;
    let kind = match text("kind").as_str() {
        "market" => OrderType::Market,
        "limit" => OrderType::Limit,
        v => return Err(bad("kind", v.into())),
    };
    let tif = match text("tif").as_str() {
        "day" => Tif::Day,
        "gtc" => Tif::Gtc,
        "ioc" => Tif::Ioc,
        v => return Err(bad("tif", v.into())),
    };
    let status = match text("status").as_str() {
        "open" => OrderStatus::Open,
        "filled" => OrderStatus::Filled,
        "cancelled" => OrderStatus::Cancelled,
        "expired" => OrderStatus::Expired,
        v => return Err(bad("status", v.into())),
    };
    let size = match (r.get::<Option<Decimal>, _>("qty"), r.get::<Option<Decimal>, _>("notional")) {
        (Some(q), _) => Size::Qty(q),
        (None, Some(n)) => Size::Notional(n),
        (None, None) => return Err(decode_err("order without qty or notional".into())),
    };
    Ok(Order {
        id: r.get::<i64, _>("id") as u64,
        account: text("account_id"),
        req: OrderRequest {
            instrument: text("instrument").parse().map_err(decode_err)?,
            side,
            kind,
            size,
            limit_price: r.get("limit_price"),
            tif,
            reason: text("reason"),
        },
        status,
        filled_qty: r.get("filled_qty"),
        filled_notional: r.get("filled_notional"),
        created_at: r.get("created_at"),
    })
}

fn parse_fill(r: &sqlx::postgres::PgRow) -> sqlx::Result<Fill> {
    let side_code: String = r.get("side");
    let liquidity = match r.get::<String, _>("liquidity").as_str() {
        "taker" => Liquidity::Taker,
        "maker" => Liquidity::Maker,
        v => return Err(decode_err(format!("unknown liquidity {v:?}"))),
    };
    Ok(Fill {
        order_id: r.get::<i64, _>("order_id") as u64,
        account: r.get("account_id"),
        instrument: r.get::<String, _>("instrument").parse().map_err(decode_err)?,
        side: Side::from_code(&side_code).ok_or_else(|| decode_err(format!("unknown side {side_code}")))?,
        qty: r.get("qty"),
        notional: r.get("notional"),
        price: r.get("price"),
        fee: r.get("fee"),
        tax: r.get("tax"),
        realized_pnl: r.get("realized_pnl"),
        liquidity,
        at: r.get("at"),
    })
}

fn parse_alert(r: &sqlx::postgres::PgRow) -> sqlx::Result<Alert> {
    let kind: String = r.get("kind");
    let instrument = r.get::<Option<String>, _>("instrument").map(|s| s.parse::<InstrumentId>()).transpose().map_err(decode_err)?;
    let venue = r.get::<Option<String>, _>("venue").and_then(|v| Venue::from_tag(&v));
    let window = r.get::<Option<i32>, _>("window_minutes").map(|w| w as u32);
    let condition = Condition::from_parts(&kind, instrument, venue, r.get("threshold"), window).ok_or_else(|| decode_err(format!("incomplete {kind} alert")))?;
    Ok(Alert {
        id: r.get("id"),
        account: r.get("account_id"),
        generation: r.get("generation"),
        condition,
        note: r.get("note"),
        once: r.get("once"),
        created_at: r.get("created_at"),
        last_fired_at: r.get("last_fired_at"),
    })
}

pub struct Store {
    pool: PgPool,
}

/// Lower-case enum name, e.g. `OrderStatus::Filled` -> "filled".
fn code(x: impl std::fmt::Debug) -> String {
    format!("{x:?}").to_lowercase()
}

fn decode_err(msg: String) -> sqlx::Error {
    sqlx::Error::Decode(msg.into())
}

async fn deposit(
    tx: &mut Transaction<'_, Postgres>,
    account: &str,
    generation: i32,
    initial: &[(Currency, Decimal)],
    at: DateTime<Utc>,
) -> sqlx::Result<()> {
    for (c, amount) in initial {
        sqlx::query(
            "INSERT INTO ledger_entries (account_id, generation, currency, amount, kind, at)
             VALUES ($1, $2, $3, $4, 'deposit', $5)",
        )
        .bind(account)
        .bind(generation)
        .bind(c.code())
        .bind(amount)
        .bind(at)
        .execute(&mut **tx)
        .await?;
    }
    Ok(())
}

impl Store {
    pub fn new(pool: PgPool) -> Self {
        Store { pool }
    }

    pub async fn connect(url: &str) -> anyhow::Result<Self> {
        let pool = PgPool::connect(url).await?;
        sqlx::migrate!().run(&pool).await?;
        Ok(Store { pool })
    }

    pub async fn create_account(
        &self,
        id: &str,
        name: &str,
        agent_id: Option<&str>,
        initial: &[(Currency, Decimal)],
        at: DateTime<Utc>,
    ) -> sqlx::Result<()> {
        let mut tx = self.pool.begin().await?;
        sqlx::query("INSERT INTO accounts (id, name, agent_id, created_at) VALUES ($1, $2, $3, $4)")
            .bind(id)
            .bind(name)
            .bind(agent_id)
            .bind(at)
            .execute(&mut *tx)
            .await?;
        deposit(&mut tx, id, 1, initial, at).await?;
        tx.commit().await
    }

    /// Start the account over with fresh cash. Earlier generations stay queryable.
    pub async fn reset_account(&self, id: &str, initial: &[(Currency, Decimal)], at: DateTime<Utc>) -> sqlx::Result<i32> {
        let mut tx = self.pool.begin().await?;
        let generation: i32 = sqlx::query_scalar("UPDATE accounts SET generation = generation + 1 WHERE id = $1 RETURNING generation")
            .bind(id)
            .fetch_one(&mut *tx)
            .await?;
        deposit(&mut tx, id, generation, initial, at).await?;
        tx.commit().await?;
        Ok(generation)
    }

    pub async fn generation(&self, id: &str) -> sqlx::Result<i32> {
        sqlx::query_scalar("SELECT generation FROM accounts WHERE id = $1").bind(id).fetch_one(&self.pool).await
    }

    pub async fn max_order_id(&self) -> sqlx::Result<u64> {
        let max: Option<i64> = sqlx::query_scalar("SELECT MAX(id) FROM orders").fetch_one(&self.pool).await?;
        Ok(max.unwrap_or(0) as u64)
    }

    pub async fn save_order(&self, o: &Order, generation: i32) -> sqlx::Result<()> {
        let (qty, notional) = match o.req.size {
            Size::Qty(q) => (Some(q), None),
            Size::Notional(n) => (None, Some(n)),
        };
        sqlx::query(
            "INSERT INTO orders (id, account_id, generation, instrument, side, kind, qty, notional, limit_price,
                                 tif, reason, status, filled_qty, filled_notional, created_at)
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15)
             ON CONFLICT (id) DO UPDATE SET status = EXCLUDED.status, filled_qty = EXCLUDED.filled_qty,
                 filled_notional = EXCLUDED.filled_notional, updated_at = now()",
        )
        .bind(o.id as i64)
        .bind(&o.account)
        .bind(generation)
        .bind(o.req.instrument.to_string())
        .bind(o.req.side.code())
        .bind(code(o.req.kind))
        .bind(qty)
        .bind(notional)
        .bind(o.req.limit_price)
        .bind(code(o.req.tif))
        .bind(&o.req.reason)
        .bind(code(o.status))
        .bind(o.filled_qty)
        .bind(o.filled_notional)
        .bind(o.created_at)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// Insert a fill and its cash legs in one transaction.
    pub async fn save_fill(&self, f: &Fill, generation: i32) -> sqlx::Result<i64> {
        let mut tx = self.pool.begin().await?;
        let fill_id: i64 = sqlx::query_scalar(
            "INSERT INTO fills (order_id, account_id, generation, instrument, side, qty, notional, price, fee, tax,
                                realized_pnl, liquidity, at)
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13) RETURNING id",
        )
        .bind(f.order_id as i64)
        .bind(&f.account)
        .bind(generation)
        .bind(f.instrument.to_string())
        .bind(f.side.code())
        .bind(f.qty)
        .bind(f.notional)
        .bind(f.price)
        .bind(f.fee)
        .bind(f.tax)
        .bind(f.realized_pnl)
        .bind(code(f.liquidity))
        .bind(f.at)
        .fetch_one(&mut *tx)
        .await?;
        let trade = match f.side {
            Side::Buy => -f.notional,
            Side::Sell => f.notional,
        };
        for (kind, amount) in [("trade", trade), ("fee", -f.fee), ("tax", -f.tax)] {
            if amount.is_zero() {
                continue;
            }
            sqlx::query(
                "INSERT INTO ledger_entries (account_id, generation, currency, amount, kind, fill_id, at)
                 VALUES ($1, $2, $3, $4, $5, $6, $7)",
            )
            .bind(&f.account)
            .bind(generation)
            .bind(f.instrument.venue.currency().code())
            .bind(amount)
            .bind(kind)
            .bind(fill_id)
            .bind(f.at)
            .execute(&mut *tx)
            .await?;
        }
        tx.commit().await?;
        Ok(fill_id)
    }

    /// Record a currency conversion as two `fx` ledger entries.
    pub async fn save_conversion(&self, account: &str, generation: i32, c: &Conversion, at: DateTime<Utc>) -> sqlx::Result<()> {
        let mut tx = self.pool.begin().await?;
        for (currency, amount) in [(c.from, -c.debit), (c.to, c.credit)] {
            sqlx::query(
                "INSERT INTO ledger_entries (account_id, generation, currency, amount, kind, at)
                 VALUES ($1, $2, $3, $4, 'fx', $5)",
            )
            .bind(account)
            .bind(generation)
            .bind(currency.code())
            .bind(amount)
            .bind(at)
            .execute(&mut *tx)
            .await?;
        }
        tx.commit().await
    }

    pub async fn list_accounts(&self) -> sqlx::Result<Vec<AccountRow>> {
        let rows = sqlx::query("SELECT id, name, agent_id, generation FROM accounts ORDER BY id").fetch_all(&self.pool).await?;
        Ok(rows
            .iter()
            .map(|r| AccountRow { id: r.get("id"), name: r.get("name"), agent_id: r.get("agent_id"), generation: r.get("generation") })
            .collect())
    }

    /// Newest first.
    pub async fn orders(&self, account: &str, generation: i32, open_only: bool, limit: i64) -> sqlx::Result<Vec<Order>> {
        let rows = sqlx::query(
            "SELECT * FROM orders WHERE account_id = $1 AND generation = $2 AND (NOT $3 OR status = 'open')
             ORDER BY id DESC LIMIT $4",
        )
        .bind(account)
        .bind(generation)
        .bind(open_only)
        .bind(limit)
        .fetch_all(&self.pool)
        .await?;
        rows.iter().map(parse_order).collect()
    }

    /// Newest first; `since` is exclusive.
    pub async fn fills(&self, account: &str, generation: i32, since: Option<DateTime<Utc>>, limit: i64) -> sqlx::Result<Vec<Fill>> {
        let rows = sqlx::query(
            "SELECT * FROM fills WHERE account_id = $1 AND generation = $2 AND ($3::timestamptz IS NULL OR at > $3)
             ORDER BY id DESC LIMIT $4",
        )
        .bind(account)
        .bind(generation)
        .bind(since)
        .bind(limit)
        .fetch_all(&self.pool)
        .await?;
        rows.iter().map(parse_fill).collect()
    }

    /// A bar saved again for the same minute (a late print after the flush) is merged in.
    pub async fn save_bars(&self, bars: &[(InstrumentId, Candle)]) -> sqlx::Result<()> {
        let mut tx = self.pool.begin().await?;
        for (id, c) in bars {
            sqlx::query(
                "INSERT INTO bars (instrument, start, open, high, low, close, volume, value) VALUES ($1,$2,$3,$4,$5,$6,$7,$8)
                 ON CONFLICT (instrument, start) DO UPDATE SET high = GREATEST(bars.high, EXCLUDED.high),
                     low = LEAST(bars.low, EXCLUDED.low), close = EXCLUDED.close,
                     volume = bars.volume + EXCLUDED.volume, value = bars.value + EXCLUDED.value",
            )
            .bind(id.to_string())
            .bind(c.start)
            .bind(c.open)
            .bind(c.high)
            .bind(c.low)
            .bind(c.close)
            .bind(c.volume)
            .bind(c.value)
            .execute(&mut *tx)
            .await?;
        }
        tx.commit().await
    }

    /// 1-minute bars from `since`, oldest first.
    pub async fn bars(&self, id: &InstrumentId, since: DateTime<Utc>) -> sqlx::Result<Vec<Candle>> {
        let rows = sqlx::query("SELECT * FROM bars WHERE instrument = $1 AND start >= $2 ORDER BY start")
            .bind(id.to_string())
            .bind(since)
            .fetch_all(&self.pool)
            .await?;
        Ok(rows
            .iter()
            .map(|r| Candle {
                start: r.get("start"),
                open: r.get("open"),
                high: r.get("high"),
                low: r.get("low"),
                close: r.get("close"),
                volume: r.get("volume"),
                value: r.get("value"),
            })
            .collect())
    }

    pub async fn save_snapshot(&self, s: &Snapshot) -> sqlx::Result<()> {
        sqlx::query(
            "INSERT INTO equity_snapshots (account_id, generation, at, kind, equity_krw, cash_krw, positions_krw)
             VALUES ($1,$2,$3,$4,$5,$6,$7) ON CONFLICT DO NOTHING",
        )
        .bind(&s.account)
        .bind(s.generation)
        .bind(s.at)
        .bind(if s.kind == SnapshotKind::Daily { "daily" } else { "minute" })
        .bind(s.equity_krw)
        .bind(s.cash_krw)
        .bind(s.positions_krw)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// Oldest first; `since` inclusive. `daily_only` skips the per-minute rows (1,440 a day).
    pub async fn snapshots(&self, account: &str, generation: i32, since: Option<DateTime<Utc>>, daily_only: bool) -> sqlx::Result<Vec<Snapshot>> {
        let rows = sqlx::query(
            "SELECT * FROM equity_snapshots WHERE account_id = $1 AND generation = $2 AND ($3::timestamptz IS NULL OR at >= $3)
               AND (NOT $4 OR kind = 'daily')
             ORDER BY at, kind",
        )
        .bind(account)
        .bind(generation)
        .bind(since)
        .bind(daily_only)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows
            .iter()
            .map(|r| Snapshot {
                account: r.get("account_id"),
                generation: r.get("generation"),
                at: r.get("at"),
                kind: if r.get::<String, _>("kind") == "daily" { SnapshotKind::Daily } else { SnapshotKind::Minute },
                equity_krw: r.get("equity_krw"),
                cash_krw: r.get("cash_krw"),
                positions_krw: r.get("positions_krw"),
            })
            .collect())
    }

    pub async fn create_alert(&self, a: &Alert) -> sqlx::Result<i64> {
        let c = &a.condition;
        sqlx::query_scalar(
            "INSERT INTO alerts (account_id, generation, kind, instrument, venue, threshold, window_minutes, note, once, created_at)
             VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10) RETURNING id",
        )
        .bind(&a.account)
        .bind(a.generation)
        .bind(c.kind())
        .bind(c.instrument().map(|i| i.to_string()))
        .bind(c.venue().map(|v| v.tag()))
        .bind(c.threshold())
        .bind(c.window_minutes().map(|w| w as i32))
        .bind(&a.note)
        .bind(a.once)
        .bind(a.created_at)
        .fetch_one(&self.pool)
        .await
    }

    pub async fn active_alerts(&self, account: &str, generation: i32) -> sqlx::Result<Vec<Alert>> {
        let rows = sqlx::query("SELECT * FROM alerts WHERE account_id = $1 AND generation = $2 AND active ORDER BY id")
            .bind(account)
            .bind(generation)
            .fetch_all(&self.pool)
            .await?;
        rows.iter().map(parse_alert).collect()
    }

    /// Active alerts of every account at the generation it was restored at.
    pub async fn all_active_alerts(&self, generations: &HashMap<String, i32>) -> sqlx::Result<Vec<Alert>> {
        let mut out = Vec::new();
        for (account, generation) in generations {
            out.extend(self.active_alerts(account, *generation).await?);
        }
        Ok(out)
    }

    /// Turn off `account`'s alert `id`. False when it is not that account's or already off.
    pub async fn deactivate_alert(&self, account: &str, id: i64) -> sqlx::Result<bool> {
        let done = sqlx::query("UPDATE alerts SET active = false WHERE id = $1 AND account_id = $2 AND active")
            .bind(id)
            .bind(account)
            .execute(&self.pool)
            .await?;
        Ok(done.rows_affected() == 1)
    }

    pub async fn mark_fired(&self, id: i64, at: DateTime<Utc>, deactivate: bool) -> sqlx::Result<()> {
        sqlx::query("UPDATE alerts SET last_fired_at = $2, active = active AND NOT $3 WHERE id = $1")
            .bind(id)
            .bind(at)
            .bind(deactivate)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    pub async fn record_alert_event(&self, alert_id: i64, account: &str, at: DateTime<Utc>, message: &str, delivered: bool, error: Option<&str>) -> sqlx::Result<i64> {
        sqlx::query_scalar(
            "INSERT INTO alert_events (alert_id, account_id, fired_at, message, delivered, error) VALUES ($1,$2,$3,$4,$5,$6) RETURNING id",
        )
        .bind(alert_id)
        .bind(account)
        .bind(at)
        .bind(message)
        .bind(delivered)
        .bind(error)
        .fetch_one(&self.pool)
        .await
    }

    pub async fn set_event_delivered(&self, event_id: i64, delivered: bool, error: Option<&str>) -> sqlx::Result<()> {
        sqlx::query("UPDATE alert_events SET delivered = $2, error = $3 WHERE id = $1")
            .bind(event_id)
            .bind(delivered)
            .bind(error)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    pub async fn alert_session(&self, account: &str) -> sqlx::Result<Option<String>> {
        sqlx::query_scalar("SELECT alert_session_id FROM accounts WHERE id = $1").bind(account).fetch_one(&self.pool).await
    }

    pub async fn set_alert_session(&self, account: &str, session: &str) -> sqlx::Result<()> {
        sqlx::query("UPDATE accounts SET alert_session_id = $2 WHERE id = $1").bind(account).bind(session).execute(&self.pool).await?;
        Ok(())
    }

    pub async fn cash_balances(&self, account: &str, generation: i32) -> sqlx::Result<HashMap<Currency, Decimal>> {
        self.sum_by_currency(account, generation, false).await
    }

    async fn sum_by_currency(&self, account: &str, generation: i32, non_trade_only: bool) -> sqlx::Result<HashMap<Currency, Decimal>> {
        let rows = sqlx::query(
            "SELECT currency, SUM(amount) AS total FROM ledger_entries
             WHERE account_id = $1 AND generation = $2 AND (NOT $3 OR kind IN ('deposit', 'fx'))
             GROUP BY currency",
        )
        .bind(account)
        .bind(generation)
        .bind(non_trade_only)
        .fetch_all(&self.pool)
        .await?;
        rows.iter()
            .map(|r| {
                let c: String = r.get("currency");
                let c = Currency::from_code(&c).ok_or_else(|| decode_err(format!("unknown currency {c}")))?;
                Ok((c, r.get::<Decimal, _>("total")))
            })
            .collect()
    }

    /// Rebuild a portfolio from deposits and fills. Reservations for open orders are not restored.
    // ponytail: open orders are not reloaded after a restart; Phase 3 restores resting orders on startup.
    pub async fn load_portfolio(&self, account: &str, generation: i32) -> sqlx::Result<Portfolio> {
        let deposits = self.sum_by_currency(account, generation, true).await?;
        let mut pf = Portfolio::new(&deposits.into_iter().collect::<Vec<_>>());
        let rows = sqlx::query(
            "SELECT instrument, side, qty, notional, fee, tax FROM fills
             WHERE account_id = $1 AND generation = $2 ORDER BY id",
        )
        .bind(account)
        .bind(generation)
        .fetch_all(&self.pool)
        .await?;
        for r in rows {
            let id: InstrumentId = r.get::<String, _>("instrument").parse().map_err(decode_err)?;
            let side_code: String = r.get("side");
            let side = Side::from_code(&side_code).ok_or_else(|| decode_err(format!("unknown side {side_code}")))?;
            pf.apply_fill(&id, side, r.get("qty"), r.get("notional"), r.get("fee"), r.get("tax"));
        }
        Ok(pf)
    }
}
