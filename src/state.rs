use std::{
    collections::VecDeque,
    sync::{
        atomic::{AtomicBool, AtomicI64, AtomicU64},
        Arc, Mutex,
    },
};

use serde::{Deserialize, Serialize};
use tokio::sync::{broadcast, mpsc, RwLock};

use crate::{config::Config, db::EventStore, geoip::GeoDb, metrics::Metrics};

/// Seconds since the Unix epoch.
pub fn unix_now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// A single connected (or recently connected) OpenVPN client, enriched with geo data.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ClientEntry {
    /// Internal de-duplication key (Common Name || Username || Client ID).
    #[serde(skip)]
    pub key: String,
    /// OpenVPN Client ID.
    pub cid: u32,
    /// Display name.
    pub node: String,
    /// Public IP address.
    #[serde(rename = "pub", skip_serializing_if = "String::is_empty")]
    pub pub_addr: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub port: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub vpn: String,
    pub received: u64,
    pub sent: u64,
    pub connected: i64,
    pub seen: i64,
    /// Instantaneous download rate (bytes/second), derived from byte-count deltas.
    pub rate_down: f64,
    /// Instantaneous upload rate (bytes/second), derived from byte-count deltas.
    pub rate_up: f64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub country_code: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub country_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub lat: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub lon: Option<f64>,
}

/// Commands sent to a running management-connection task.
pub enum Command {
    Disconnect(u32),
    DisconnectAll,
    /// Force an immediate `status 3` poll.
    Refresh,
}

/// One point of per-server history used by the stats chart.
#[derive(Debug, Clone, Serialize)]
pub struct HistorySample {
    pub t: i64,
    pub clients: u32,
    pub connects: u64,
    pub disconnects: u64,
    pub received: u64,
    pub sent: u64,
}

/// Shared, live state for one configured server.
pub struct ServerState {
    pub id: usize,
    pub name: String,
    pub host: String,
    pub man_port: u16,
    /// Current client list (authoritative from the last `status 3` dump).
    pub entries: RwLock<Vec<ClientEntry>>,
    pub online: AtomicBool,
    pub cmd_tx: mpsc::UnboundedSender<Command>,
    pub connects: AtomicU64,
    pub disconnects: AtomicU64,
    /// Round-trip time of the last `status 3` poll, in milliseconds.
    pub latency_ms: AtomicU64,
    /// Unix time of the last completed status poll.
    pub last_poll: AtomicI64,
    /// Rolling history of client counts and traffic totals.
    pub history: Mutex<VecDeque<HistorySample>>,
}

/// Process-wide application state.
pub struct AppState {
    pub config: Arc<Config>,
    pub servers: Vec<Arc<ServerState>>,
    pub db: EventStore,
    pub geo: Option<Arc<GeoDb>>,
    pub metrics: Arc<Metrics>,
    /// Fan-out channel of pre-serialised JSON messages for WebSocket clients.
    pub tx: broadcast::Sender<String>,
}
