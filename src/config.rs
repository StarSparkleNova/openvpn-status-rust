use std::{env, fs, path::PathBuf};

use anyhow::{Context, Result};
use serde::Deserialize;

/// One OpenVPN server whose management interface we poll.
#[derive(Debug, Clone, Deserialize)]
pub struct ServerConfig {
    pub name: String,
    pub host: String,
    #[serde(default = "default_man_port")]
    pub man_port: u16,
    /// Management interface password (only if `management ... pw-file` is used).
    #[serde(default)]
    pub man_pwd: Option<String>,
    /// Only merge ROUTING_TABLE entries whose public IP is inside this CIDR.
    #[serde(default)]
    pub netmask: Option<String>,
}

fn default_man_port() -> u16 {
    7505
}

/// Top-level application configuration.
///
/// Sources, in increasing priority: defaults -> `config.toml` -> `STATUS_*` env vars.
#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct Config {
    pub bind: String,
    pub port: u16,
    pub username: Option<String>,
    pub password: Option<String>,
    pub database_url: String,
    pub geoip_db: Option<PathBuf>,
    pub servers_dir: Option<PathBuf>,
    /// How often to ask OpenVPN for a full `status 3` dump.
    pub poll_interval_secs: u64,
    /// `bytecount N` interval requested from OpenVPN.
    pub bytecount_interval_secs: u64,
    /// How often batched `>BYTECOUNT_CLI` updates are flushed to browsers.
    pub update_flush_secs: u64,
    /// Window (seconds) used to coalesce connect-after-disconnect into a reconnect.
    pub event_dedup_secs: i64,
    /// Delete persisted events older than this many days (0 = keep forever).
    pub retention_days: u64,
    /// Number of history samples retained per server for the stats chart.
    pub history_points: usize,
    /// Free tile server for the map view. Empty string disables the map.
    pub map_tile_url: String,
    /// Inline server definitions (`[[servers]]` in TOML).
    pub servers: Vec<ServerConfig>,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            bind: "127.0.0.1".into(),
            port: 3013,
            username: None,
            password: None,
            database_url: "sqlite://data.sqlite".into(),
            geoip_db: None,
            servers_dir: Some(PathBuf::from("servers")),
            poll_interval_secs: 5,
            bytecount_interval_secs: 5,
            update_flush_secs: 5,
            event_dedup_secs: 60,
            retention_days: 0,
            history_points: 720,
            map_tile_url: "https://tile.openstreetmap.org/{z}/{x}/{y}.png".into(),
            servers: Vec::new(),
        }
    }
}

impl Config {
    pub fn load() -> Result<Config> {
        let path = env::var("STATUS_CONFIG")
            .map(PathBuf::from)
            .unwrap_or_else(|_| PathBuf::from("config.toml"));

        let mut cfg = if path.exists() {
            let text = fs::read_to_string(&path)
                .with_context(|| format!("reading config {}", path.display()))?;
            toml::from_str(&text).with_context(|| format!("parsing config {}", path.display()))?
        } else {
            tracing::warn!(path = %path.display(), "config file not found; using defaults and environment");
            Config::default()
        };

        // Environment overrides.
        if let Ok(v) = env::var("STATUS_BIND") {
            cfg.bind = v;
        }
        if let Ok(v) = env::var("STATUS_PORT") {
            cfg.port = v.parse().context("STATUS_PORT must be a number")?;
        }
        if let Ok(v) = env::var("STATUS_USERNAME") {
            cfg.username = Some(v);
        }
        if let Ok(v) = env::var("STATUS_PASSWORD") {
            cfg.password = Some(v);
        }
        if let Ok(v) = env::var("STATUS_DATABASE_URL") {
            cfg.database_url = v;
        }
        if let Ok(v) = env::var("STATUS_GEOIP_DB") {
            cfg.geoip_db = Some(PathBuf::from(v));
        }
        if let Ok(v) = env::var("STATUS_MAP_TILE_URL") {
            cfg.map_tile_url = v;
        }

        // Auto-discover a GeoLite2 database if none configured.
        if cfg.geoip_db.is_none() {
            for candidate in ["GeoLite2-City.mmdb", "/usr/share/GeoIP/GeoLite2-City.mmdb"] {
                let p = PathBuf::from(candidate);
                if p.exists() {
                    cfg.geoip_db = Some(p);
                    break;
                }
            }
        }

        // Merge server definitions from a directory of JSON files (Docker-friendly).
        if let Some(dir) = cfg.servers_dir.clone() {
            if dir.is_dir() {
                let mut files: Vec<PathBuf> = fs::read_dir(&dir)
                    .with_context(|| format!("reading servers dir {}", dir.display()))?
                    .filter_map(|e| e.ok())
                    .map(|e| e.path())
                    .filter(|p| p.extension().and_then(|e| e.to_str()) == Some("json"))
                    .collect();
                files.sort();
                for p in files {
                    let text = fs::read_to_string(&p)
                        .with_context(|| format!("reading {}", p.display()))?;
                    let sc: ServerConfig = serde_json::from_str(&text)
                        .with_context(|| format!("parsing {}", p.display()))?;
                    tracing::info!(server = %sc.name, file = %p.display(), "loaded server");
                    cfg.servers.push(sc);
                }
            }
        }

        if cfg.username.is_some() {
            match cfg.password.as_deref() {
                Some(p) if !p.is_empty() => {}
                _ => anyhow::bail!("authentication enabled (username set) but password is empty"),
            }
        } else {
            tracing::warn!("HTTP authentication is disabled: set username/password to enable it");
        }
        if cfg.servers.is_empty() {
            tracing::warn!("no OpenVPN servers configured");
        }
        Ok(cfg)
    }

    pub fn auth_enabled(&self) -> bool {
        self.username.is_some()
    }
}
