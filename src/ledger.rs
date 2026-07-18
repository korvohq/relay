// Copyright 2025-present Snab Limited (trading as Korvo)
// SPDX-License-Identifier: Apache-2.0

use std::{collections::BTreeMap, fs, path::PathBuf, time::Duration};

use chrono::{DateTime, Utc};
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
use uuid::Uuid;

use crate::{
    config::CapLimits,
    error::{RelayError, Result},
};

const RESERVATION_TTL_SECONDS: i64 = 10 * 60;

#[derive(Clone, Debug)]
pub struct Ledger {
    path: PathBuf,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Reservation {
    pub id: String,
    pub amount_microusd: i64,
}

#[derive(Clone, Debug)]
pub struct CallRecord {
    pub ts: DateTime<Utc>,
    pub provider: String,
    pub model: String,
    pub tokens_in: u64,
    pub tokens_out: u64,
    pub cost_microusd: i64,
    pub latency_ms: u64,
    pub session_id: String,
    pub route_tier: String,
    pub deflected: bool,
    pub price_input_per_mtok: String,
    pub price_output_per_mtok: String,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct UsageSummary {
    pub total_microusd: i64,
    pub calls: u64,
    pub tokens_in: u64,
    pub tokens_out: u64,
    pub by_model: BTreeMap<String, ModelUsage>,
    pub pending_microusd: i64,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct ModelUsage {
    pub calls: u64,
    pub tokens_in: u64,
    pub tokens_out: u64,
    pub cost_microusd: i64,
}

impl Ledger {
    pub fn open(path: PathBuf) -> Result<Self> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let ledger = Self { path };
        let connection = ledger.connection()?;
        ledger.migrate(&connection)?;
        set_private_file_permissions(&ledger.path)?;
        Ok(ledger)
    }

    pub fn reserve(
        &self,
        now: DateTime<Utc>,
        caps: CapLimits,
        amount_microusd: i64,
        provider: &str,
        model: &str,
    ) -> Result<Reservation> {
        if amount_microusd < 0 {
            return Err(RelayError::AmountOutOfRange(amount_microusd.to_string()));
        }
        let (day_key, month_key) = period_keys(now, caps);
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        transaction.execute(
            "DELETE FROM reservations WHERE state = 'active' AND expires_at <= ?1",
            [now.to_rfc3339()],
        )?;

        let daily_spend = sum_calls(&transaction, "day_key", &day_key)?;
        let monthly_spend = sum_calls(&transaction, "month_key", &month_key)?;
        let daily_reserved = sum_reservations(&transaction, "day_key", &day_key)?;
        let monthly_reserved = sum_reservations(&transaction, "month_key", &month_key)?;

        check_cap(
            "daily",
            daily_spend,
            daily_reserved,
            amount_microusd,
            caps.daily_microusd,
        )?;
        check_cap(
            "monthly",
            monthly_spend,
            monthly_reserved,
            amount_microusd,
            caps.monthly_microusd,
        )?;

        let reservation = Reservation {
            id: Uuid::new_v4().to_string(),
            amount_microusd,
        };
        let expires_at = now + chrono::Duration::seconds(RESERVATION_TTL_SECONDS);
        transaction.execute(
            "INSERT INTO reservations
             (id, created_at, expires_at, day_key, month_key, amount_microusd, state, provider, model)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, 'active', ?7, ?8)",
            params![
                reservation.id,
                now.to_rfc3339(),
                expires_at.to_rfc3339(),
                day_key,
                month_key,
                amount_microusd,
                provider,
                model,
            ],
        )?;
        transaction.commit()?;
        Ok(reservation)
    }

    pub fn settle(&self, reservation_id: &str, call: &CallRecord, caps: CapLimits) -> Result<()> {
        let (day_key, month_key) = period_keys(call.ts, caps);
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let state: Option<String> = transaction
            .query_row(
                "SELECT state FROM reservations WHERE id = ?1",
                [reservation_id],
                |row| row.get(0),
            )
            .optional()?;
        if !matches!(state.as_deref(), Some("active" | "pending")) {
            return Err(RelayError::ReservationUnavailable(
                reservation_id.to_owned(),
            ));
        }

        transaction.execute(
            "INSERT INTO calls
             (ts, day_key, month_key, provider, model, tokens_in, tokens_out,
              cost_microusd, latency_ms, session_id, route_tier, deflected,
              status, price_input_per_mtok, price_output_per_mtok)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12,
                     'completed', ?13, ?14)",
            params![
                call.ts.to_rfc3339(),
                day_key,
                month_key,
                call.provider,
                call.model,
                to_i64(call.tokens_in, "input tokens")?,
                to_i64(call.tokens_out, "output tokens")?,
                call.cost_microusd,
                to_i64(call.latency_ms, "latency")?,
                call.session_id,
                call.route_tier,
                i64::from(call.deflected),
                call.price_input_per_mtok,
                call.price_output_per_mtok,
            ],
        )?;
        transaction.execute("DELETE FROM reservations WHERE id = ?1", [reservation_id])?;
        transaction.commit()?;
        Ok(())
    }

    pub fn release(&self, reservation_id: &str) -> Result<()> {
        let connection = self.connection()?;
        connection.execute(
            "DELETE FROM reservations WHERE id = ?1 AND state IN ('active', 'pending')",
            [reservation_id],
        )?;
        Ok(())
    }

    pub fn mark_pending(&self, reservation_id: &str, reason: &str) -> Result<()> {
        let connection = self.connection()?;
        let changed = connection.execute(
            "UPDATE reservations
             SET state = 'pending', reconciliation_reason = ?2
             WHERE id = ?1 AND state = 'active'",
            params![reservation_id, reason.chars().take(500).collect::<String>()],
        )?;
        if changed != 1 {
            return Err(RelayError::ReservationUnavailable(
                reservation_id.to_owned(),
            ));
        }
        Ok(())
    }

    pub fn usage(
        &self,
        now: DateTime<Utc>,
        caps: CapLimits,
        period: Period,
    ) -> Result<UsageSummary> {
        let (day_key, month_key) = period_keys(now, caps);
        let (column, value) = match period {
            Period::Day => ("day_key", day_key),
            Period::Month => ("month_key", month_key),
        };
        let connection = self.connection()?;
        let sql = format!(
            "SELECT model, COUNT(*), COALESCE(SUM(tokens_in), 0),
                    COALESCE(SUM(tokens_out), 0), COALESCE(SUM(cost_microusd), 0)
             FROM calls WHERE {column} = ?1 AND status = 'completed' GROUP BY model"
        );
        let mut statement = connection.prepare(&sql)?;
        let mut rows = statement.query([&value])?;
        let mut summary = UsageSummary::default();
        while let Some(row) = rows.next()? {
            let model: String = row.get(0)?;
            let usage = ModelUsage {
                calls: from_i64(row.get(1)?, "call count")?,
                tokens_in: from_i64(row.get(2)?, "input tokens")?,
                tokens_out: from_i64(row.get(3)?, "output tokens")?,
                cost_microusd: row.get(4)?,
            };
            summary.calls += usage.calls;
            summary.tokens_in += usage.tokens_in;
            summary.tokens_out += usage.tokens_out;
            summary.total_microusd = summary
                .total_microusd
                .checked_add(usage.cost_microusd)
                .ok_or_else(|| RelayError::AmountOutOfRange("usage total overflow".into()))?;
            summary.by_model.insert(model, usage);
        }
        summary.pending_microusd = sum_reservations(&connection, column, &value)?;
        Ok(summary)
    }

    fn connection(&self) -> Result<Connection> {
        let connection = Connection::open(&self.path)?;
        connection.busy_timeout(Duration::from_secs(5))?;
        connection.execute_batch(
            "PRAGMA journal_mode = WAL;
             PRAGMA foreign_keys = ON;
             PRAGMA synchronous = FULL;",
        )?;
        Ok(connection)
    }

    fn migrate(&self, connection: &Connection) -> Result<()> {
        connection.execute_batch(
            "CREATE TABLE IF NOT EXISTS calls (
                id INTEGER PRIMARY KEY,
                ts TEXT NOT NULL,
                day_key TEXT NOT NULL,
                month_key TEXT NOT NULL,
                provider TEXT NOT NULL,
                model TEXT NOT NULL,
                tokens_in INTEGER NOT NULL CHECK (tokens_in >= 0),
                tokens_out INTEGER NOT NULL CHECK (tokens_out >= 0),
                cost_microusd INTEGER NOT NULL CHECK (cost_microusd >= 0),
                latency_ms INTEGER NOT NULL CHECK (latency_ms >= 0),
                session_id TEXT NOT NULL,
                route_tier TEXT NOT NULL DEFAULT 'direct',
                deflected INTEGER NOT NULL DEFAULT 0 CHECK (deflected IN (0, 1)),
                status TEXT NOT NULL DEFAULT 'completed',
                price_input_per_mtok TEXT NOT NULL,
                price_output_per_mtok TEXT NOT NULL
             );
             CREATE INDEX IF NOT EXISTS calls_day_idx ON calls(day_key);
             CREATE INDEX IF NOT EXISTS calls_month_idx ON calls(month_key);
             CREATE INDEX IF NOT EXISTS calls_model_idx ON calls(model);

             CREATE TABLE IF NOT EXISTS reservations (
                id TEXT PRIMARY KEY,
                created_at TEXT NOT NULL,
                expires_at TEXT NOT NULL,
                day_key TEXT NOT NULL,
                month_key TEXT NOT NULL,
                amount_microusd INTEGER NOT NULL CHECK (amount_microusd >= 0),
                state TEXT NOT NULL CHECK (state IN ('active', 'pending')),
                provider TEXT NOT NULL,
                model TEXT NOT NULL,
                reconciliation_reason TEXT
             );
             CREATE INDEX IF NOT EXISTS reservations_day_idx ON reservations(day_key, state);
             CREATE INDEX IF NOT EXISTS reservations_month_idx ON reservations(month_key, state);
             PRAGMA user_version = 1;",
        )?;
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Period {
    Day,
    Month,
}

fn period_keys(now: DateTime<Utc>, caps: CapLimits) -> (String, String) {
    let local = now.with_timezone(&caps.timezone);
    (
        local.format("%Y-%m-%d").to_string(),
        local.format("%Y-%m").to_string(),
    )
}

fn check_cap(
    period: &'static str,
    spent: i64,
    reserved: i64,
    request: i64,
    cap: i64,
) -> Result<()> {
    let projected = spent
        .checked_add(reserved)
        .and_then(|value| value.checked_add(request))
        .ok_or_else(|| RelayError::AmountOutOfRange("cap projection overflow".into()))?;
    if projected > cap {
        return Err(RelayError::CapExceeded {
            period,
            spent_microusd: spent,
            reserved_microusd: reserved,
            request_microusd: request,
            cap_microusd: cap,
        });
    }
    Ok(())
}

fn sum_calls(connection: &Connection, column: &str, value: &str) -> Result<i64> {
    let sql = format!(
        "SELECT COALESCE(SUM(cost_microusd), 0) FROM calls
         WHERE {column} = ?1 AND status = 'completed'"
    );
    Ok(connection.query_row(&sql, [value], |row| row.get(0))?)
}

fn sum_reservations(connection: &Connection, column: &str, value: &str) -> Result<i64> {
    let sql = format!(
        "SELECT COALESCE(SUM(amount_microusd), 0) FROM reservations
         WHERE {column} = ?1 AND state IN ('active', 'pending')"
    );
    Ok(connection.query_row(&sql, [value], |row| row.get(0))?)
}

fn to_i64(value: u64, name: &str) -> Result<i64> {
    value
        .try_into()
        .map_err(|_| RelayError::AmountOutOfRange(format!("{name}: {value}")))
}

fn from_i64(value: i64, name: &str) -> Result<u64> {
    value
        .try_into()
        .map_err(|_| RelayError::InvalidUsage(format!("negative {name} in ledger")))
}

#[cfg(unix)]
fn set_private_file_permissions(path: &std::path::Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, fs::Permissions::from_mode(0o600))?;
    Ok(())
}

#[cfg(not(unix))]
fn set_private_file_permissions(_path: &std::path::Path) -> Result<()> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::{str::FromStr, sync::Arc, thread};

    use chrono::TimeZone;
    use chrono_tz::Tz;
    use tempfile::tempdir;

    use super::*;

    fn caps(daily: i64, monthly: i64) -> CapLimits {
        CapLimits {
            daily_microusd: daily,
            monthly_microusd: monthly,
            timezone: Tz::from_str("UTC").unwrap(),
        }
    }

    #[test]
    fn exact_cap_is_allowed_and_next_request_is_refused() {
        let temporary = tempdir().unwrap();
        let ledger = Ledger::open(temporary.path().join("ledger.db")).unwrap();
        let now = Utc.with_ymd_and_hms(2026, 7, 18, 12, 0, 0).unwrap();
        ledger
            .reserve(now, caps(100, 1_000), 100, "test", "test:model")
            .unwrap();
        assert!(matches!(
            ledger.reserve(now, caps(100, 1_000), 1, "test", "test:model"),
            Err(RelayError::CapExceeded {
                period: "daily",
                ..
            })
        ));
    }

    #[test]
    fn concurrent_reservations_cannot_overbook_the_cap() {
        let temporary = tempdir().unwrap();
        let ledger = Arc::new(Ledger::open(temporary.path().join("ledger.db")).unwrap());
        let now = Utc.with_ymd_and_hms(2026, 7, 18, 12, 0, 0).unwrap();
        let handles: Vec<_> = (0..2)
            .map(|_| {
                let ledger = Arc::clone(&ledger);
                thread::spawn(move || ledger.reserve(now, caps(100, 100), 75, "test", "test:model"))
            })
            .collect();
        let successes = handles
            .into_iter()
            .map(|handle| handle.join().unwrap().is_ok())
            .filter(|success| *success)
            .count();
        assert_eq!(successes, 1);
    }

    #[test]
    fn pending_reservations_remain_in_usage_and_caps() {
        let temporary = tempdir().unwrap();
        let ledger = Ledger::open(temporary.path().join("ledger.db")).unwrap();
        let now = Utc.with_ymd_and_hms(2026, 7, 18, 12, 0, 0).unwrap();
        let reservation = ledger
            .reserve(now, caps(100, 100), 75, "test", "test:model")
            .unwrap();
        ledger
            .mark_pending(&reservation.id, "ambiguous timeout")
            .unwrap();
        let usage = ledger.usage(now, caps(100, 100), Period::Day).unwrap();
        assert_eq!(usage.pending_microusd, 75);
        assert!(
            ledger
                .reserve(now, caps(100, 100), 26, "test", "test:model")
                .is_err()
        );
    }
}
