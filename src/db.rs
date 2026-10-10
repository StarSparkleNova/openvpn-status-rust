use std::{str::FromStr, sync::Arc, time::Duration};

use anyhow::{Context, Result};
use serde::Serialize;
use sqlx::{
    sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePool, SqlitePoolOptions},
    Row,
};
use tokio::sync::Mutex;

use crate::state::unix_now;

/// A persisted event as returned to the API.
#[derive(Debug, Serialize)]
pub struct EventRow {
    pub id: i64,
    pub timestamp: i64,
    pub server: i64,
    pub node: String,
    pub event: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub vpn: Option<String>,
    #[serde(rename = "pub", skip_serializing_if = "Option::is_none")]
    pub pub_addr: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub country_code: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub country_name: Option<String>,
}

/// A new event to record.
pub struct NewEvent {
    pub timestamp: i64,
    pub server: i64,
    pub node: String,
    pub event: String,
    pub vpn: Option<String>,
    pub pub_addr: Option<String>,
    pub country_code: Option<String>,
    pub country_name: Option<String>,
}

/// SQLite-backed event store.
///
/// A single async mutex serialises writes so the read-modify-write used for
/// reconnect coalescing cannot interleave (the original used a hand-rolled
/// promise mutex; this is the same idea with a proper primitive).
#[derive(Clone)]
pub struct EventStore {
    pool: SqlitePool,
    lock: Arc<Mutex<()>>,
}

impl EventStore {
    pub async fn init(url: &str) -> Result<Self> {
        let opts = SqliteConnectOptions::from_str(url)
            .with_context(|| format!("invalid database url '{url}'"))?
            .create_if_missing(true)
            .journal_mode(SqliteJournalMode::Wal)
            .busy_timeout(Duration::from_secs(5));

        let pool = SqlitePoolOptions::new()
            .max_connections(5)
            .connect_with(opts)
            .await
            .context("connecting to SQLite")?;

        sqlx::query(
            r#"CREATE TABLE IF NOT EXISTS events (
                 id           INTEGER PRIMARY KEY AUTOINCREMENT,
                 timestamp    INTEGER NOT NULL,
                 server       INTEGER NOT NULL,
                 node         TEXT    NOT NULL,
                 event        TEXT    NOT NULL,
                 vpn          TEXT,
                 pub          TEXT,
                 country_code TEXT,
                 country_name TEXT
               )"#,
        )
        .execute(&pool)
        .await?;
        sqlx::query(
            "CREATE INDEX IF NOT EXISTS idx_events_server_node_ts ON events (server, node, timestamp)",
        )
        .execute(&pool)
        .await?;
        sqlx::query("CREATE INDEX IF NOT EXISTS idx_events_server_ts ON events (server, timestamp)")
            .execute(&pool)
            .await?;

        Ok(EventStore {
            pool,
            lock: Arc::new(Mutex::new(())),
        })
    }

    /// Persist an event, returning the event name actually stored (a `connect`
    /// shortly after a `disconnect` is rewritten to `reconnect`).
    pub async fn log(&self, ev: &NewEvent, dedup_secs: i64) -> Result<String> {
        let _guard = self.lock.lock().await;

        let existing = sqlx::query(
            "SELECT id, event FROM events
             WHERE server = ? AND node = ? AND timestamp BETWEEN ? AND ?
             ORDER BY timestamp DESC LIMIT 1",
        )
        .bind(ev.server)
        .bind(&ev.node)
        .bind(ev.timestamp - dedup_secs)
        .bind(ev.timestamp + dedup_secs)
        .fetch_optional(&self.pool)
        .await?;

        if let Some(row) = existing {
            let id: i64 = row.get("id");
            let event: String = row.get("event");
            if (event == "disconnect" || event == "reconnect") && ev.event == "connect" {
                sqlx::query("UPDATE events SET event = 'reconnect', timestamp = ? WHERE id = ?")
                    .bind(ev.timestamp)
                    .bind(id)
                    .execute(&self.pool)
                    .await?;
                return Ok("reconnect".to_string());
            }
        }

        sqlx::query(
            "INSERT INTO events (timestamp, server, node, event, vpn, pub, country_code, country_name)
             VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(ev.timestamp)
        .bind(ev.server)
        .bind(&ev.node)
        .bind(&ev.event)
        .bind(&ev.vpn)
        .bind(&ev.pub_addr)
        .bind(&ev.country_code)
        .bind(&ev.country_name)
        .execute(&self.pool)
        .await?;

        Ok(ev.event.clone())
    }

    pub async fn count(&self, server: i64, search: &str) -> Result<i64> {
        let needle = format!("%{search}%");
        let row = sqlx::query("SELECT COUNT(*) AS c FROM events WHERE server = ? AND node LIKE ?")
            .bind(server)
            .bind(needle)
            .fetch_one(&self.pool)
            .await?;
        Ok(row.get::<i64, _>("c"))
    }

    pub async fn page(
        &self,
        server: i64,
        page: i64,
        size: i64,
        search: &str,
    ) -> Result<Vec<EventRow>> {
        let needle = format!("%{search}%");
        let offset = (page.max(1) - 1) * size;
        let rows = sqlx::query(
            "SELECT id, timestamp, server, node, event, vpn, pub, country_code, country_name
             FROM events WHERE server = ? AND node LIKE ?
             ORDER BY timestamp DESC LIMIT ? OFFSET ?",
        )
        .bind(server)
        .bind(needle)
        .bind(size)
        .bind(offset)
        .fetch_all(&self.pool)
        .await?;

        let mut out = Vec::with_capacity(rows.len());
        for r in rows {
            out.push(EventRow {
                id: r.get("id"),
                timestamp: r.get("timestamp"),
                server: r.get("server"),
                node: r.get("node"),
                event: r.get("event"),
                vpn: r.try_get::<Option<String>, _>("vpn").unwrap_or(None),
                pub_addr: r.try_get::<Option<String>, _>("pub").unwrap_or(None),
                country_code: r.try_get::<Option<String>, _>("country_code").unwrap_or(None),
                country_name: r.try_get::<Option<String>, _>("country_name").unwrap_or(None),
            });
        }
        Ok(out)
    }

    /// Delete events older than `retention_days`. Returns the number removed.
    pub async fn prune(&self, retention_days: u64) -> Result<u64> {
        if retention_days == 0 {
            return Ok(0);
        }
        let _guard = self.lock.lock().await;
        let cutoff = unix_now() - (retention_days as i64) * 86_400;
        let res = sqlx::query("DELETE FROM events WHERE timestamp < ?")
            .bind(cutoff)
            .execute(&self.pool)
            .await?;
        Ok(res.rows_affected())
    }

    /// All matching events (newest first) for export, capped by `limit`.
    pub async fn export(&self, server: i64, search: &str, limit: i64) -> Result<Vec<EventRow>> {
        self.page(server, 1, limit.clamp(1, 1_000_000), search).await
    }
}
