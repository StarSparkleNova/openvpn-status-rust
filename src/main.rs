mod config;
mod db;
mod geoip;
mod metrics;
mod openvpn;
mod state;
mod web;

use std::{
    collections::VecDeque,
    sync::{
        atomic::{AtomicBool, AtomicI64, AtomicU64},
        Arc, Mutex,
    },
    time::Duration,
};

use anyhow::{Context, Result};
use tokio::sync::{broadcast, mpsc, RwLock};
use tracing::{info, warn};
use tracing_subscriber::{fmt, prelude::*, EnvFilter};

#[tokio::main]
async fn main() -> Result<()> {
    init_tracing();

    let config = Arc::new(config::Config::load()?);
    info!(version = env!("CARGO_PKG_VERSION"), "openvpn-status-rust starting");

    let db = db::EventStore::init(&config.database_url)
        .await
        .context("initialising database")?;

    let geo = match &config.geoip_db {
        Some(path) => match geoip::GeoDb::open(path) {
            Ok(g) => {
                info!(db = %path.display(), "loaded GeoIP database");
                Some(Arc::new(g))
            }
            Err(e) => {
                warn!(error = %e, "GeoIP database unavailable; geolocation disabled");
                None
            }
        },
        None => {
            warn!("no GeoIP database configured; geolocation disabled");
            None
        }
    };

    let metrics = Arc::new(metrics::Metrics::default());
    let (tx, _rx) = broadcast::channel::<String>(2048);

    let mut servers = Vec::with_capacity(config.servers.len());
    let mut receivers = Vec::with_capacity(config.servers.len());
    for (id, sc) in config.servers.iter().enumerate() {
        let (cmd_tx, cmd_rx) = mpsc::unbounded_channel();
        servers.push(Arc::new(state::ServerState {
            id,
            name: sc.name.clone(),
            host: sc.host.clone(),
            man_port: sc.man_port,
            entries: RwLock::new(Vec::new()),
            online: AtomicBool::new(false),
            cmd_tx,
            connects: AtomicU64::new(0),
            disconnects: AtomicU64::new(0),
            latency_ms: AtomicU64::new(0),
            last_poll: AtomicI64::new(0),
            history: Mutex::new(VecDeque::with_capacity(config.history_points + 1)),
        }));
        receivers.push(cmd_rx);
    }

    let app_state = Arc::new(state::AppState {
        config: config.clone(),
        servers,
        db,
        geo,
        metrics,
        tx,
    });

    for (id, rx) in receivers.into_iter().enumerate() {
        tokio::spawn(openvpn::run_server(app_state.clone(), id, rx));
    }

    if config.retention_days > 0 {
        let db = app_state.db.clone();
        let days = config.retention_days;
        tokio::spawn(async move {
            let mut ticker = tokio::time::interval(Duration::from_secs(3600));
            loop {
                ticker.tick().await;
                match db.prune(days).await {
                    Ok(n) if n > 0 => info!(removed = n, "pruned old events"),
                    Ok(_) => {}
                    Err(e) => warn!(error = %e, "event pruning failed"),
                }
            }
        });
    }

    let router = web::build_router(app_state.clone());
    let listener = tokio::net::TcpListener::bind((config.bind.as_str(), config.port))
        .await
        .with_context(|| format!("binding {}:{}", config.bind, config.port))?;
    info!(addr = %listener.local_addr()?, "listening");

    axum::serve(listener, router)
        .with_graceful_shutdown(shutdown_signal())
        .await
        .context("server error")?;

    info!("shutdown complete");
    Ok(())
}

fn init_tracing() {
    let filter = EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| EnvFilter::new("info,tower_http=warn"));
    tracing_subscriber::registry()
        .with(filter)
        .with(fmt::layer().with_target(false))
        .init();
}

async fn shutdown_signal() {
    let ctrl_c = async {
        let _ = tokio::signal::ctrl_c().await;
    };

    #[cfg(unix)]
    let term = async {
        if let Ok(mut sig) =
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
        {
            sig.recv().await;
        }
    };
    #[cfg(not(unix))]
    let term = std::future::pending::<()>();

    tokio::select! {
        _ = ctrl_c => {},
        _ = term => {},
    }
    info!("shutdown signal received");
}
