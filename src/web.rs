use std::sync::{atomic::Ordering, Arc};

use axum::{
    extract::{
        ws::{Message, WebSocket, WebSocketUpgrade},
        Path, Query, Request, State,
    },
    http::{header, HeaderValue, StatusCode, Uri},
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::{get, post},
    Json, Router,
};
use base64::{engine::general_purpose::STANDARD, Engine as _};
use rust_embed::RustEmbed;
use serde::Deserialize;
use serde_json::json;
use subtle::ConstantTimeEq;
use tokio::sync::broadcast;
use tracing::error;

use crate::{
    metrics,
    state::{unix_now, AppState, Command, HistorySample},
};

#[derive(RustEmbed)]
#[folder = "web"]
struct Assets;

#[derive(Clone)]
struct AuthState {
    /// Base64 of `username:password`, or `None` when auth is disabled.
    expected: Option<String>,
}

pub fn build_router(state: Arc<AppState>) -> Router {
    let expected = if state.config.auth_enabled() {
        let user = state.config.username.clone().unwrap_or_default();
        let pass = state.config.password.clone().unwrap_or_default();
        Some(STANDARD.encode(format!("{user}:{pass}")))
    } else {
        None
    };
    let auth = Arc::new(AuthState { expected });

    let protected = Router::new()
        .route("/api/config", get(get_config))
        .route("/api/time", get(get_time))
        .route("/api/servers", get(get_servers))
        .route("/api/servers/:id/clients", get(get_clients))
        .route("/api/servers/:id/history", get(get_history))
        .route("/api/servers/:id/events", get(get_events))
        .route("/api/servers/:id/events/export", get(export_events))
        .route("/api/servers/:id/clients/:cid/disconnect", post(disconnect))
        .route("/api/servers/:id/clients/disconnect-all", post(disconnect_all))
        .route("/api/servers/:id/refresh", post(refresh))
        .route("/api/live", get(ws_handler))
        .fallback(static_handler)
        .layer(middleware::from_fn_with_state(auth, auth_mw));

    Router::new()
        .route("/healthz", get(health))
        .route("/metrics", get(metrics_handler))
        .merge(protected)
        .layer(tower_http::compression::CompressionLayer::new())
        .layer(tower_http::trace::TraceLayer::new_for_http())
        .with_state(state)
}

// ---------------------------------------------------------------------------
// Auth
// ---------------------------------------------------------------------------

async fn auth_mw(State(auth): State<Arc<AuthState>>, req: Request, next: Next) -> Response {
    if let Some(expected) = &auth.expected {
        let ok = req
            .headers()
            .get(header::AUTHORIZATION)
            .and_then(|v| v.to_str().ok())
            .and_then(|s| s.strip_prefix("Basic "))
            .map(|tok| bool::from(tok.trim().as_bytes().ct_eq(expected.as_bytes())))
            .unwrap_or(false);
        if !ok {
            return unauthorized();
        }
    }
    next.run(req).await
}

fn unauthorized() -> Response {
    let mut res = (StatusCode::UNAUTHORIZED, "Authentication required").into_response();
    res.headers_mut().insert(
        header::WWW_AUTHENTICATE,
        HeaderValue::from_static("Basic realm=\"openvpn-status\""),
    );
    res
}

// ---------------------------------------------------------------------------
// REST handlers
// ---------------------------------------------------------------------------

async fn health() -> &'static str {
    "ok"
}

async fn metrics_handler(State(state): State<Arc<AppState>>) -> Response {
    (
        [(header::CONTENT_TYPE, "text/plain; version=0.0.4")],
        metrics::render(&state),
    )
        .into_response()
}

async fn get_config(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    Json(json!({
        "version": env!("CARGO_PKG_VERSION"),
        "map_tile_url": state.config.map_tile_url,
        "poll_interval_secs": state.config.poll_interval_secs,
    }))
}

async fn get_time() -> impl IntoResponse {
    Json(json!({ "time": unix_now() }))
}

async fn get_servers(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    let mut list = Vec::with_capacity(state.servers.len());
    for s in &state.servers {
        let clients = s.entries.read().await.len();
        list.push(json!({
            "id": s.id,
            "name": s.name,
            "host": s.host,
            "man_port": s.man_port,
            "online": s.online.load(Ordering::Relaxed),
            "clients": clients,
            "latency_ms": s.latency_ms.load(Ordering::Relaxed),
            "last_poll": s.last_poll.load(Ordering::Relaxed),
            "connects_total": s.connects.load(Ordering::Relaxed),
            "disconnects_total": s.disconnects.load(Ordering::Relaxed),
        }));
    }
    Json(list)
}

async fn get_history(State(state): State<Arc<AppState>>, Path(id): Path<usize>) -> Response {
    let Some(server) = state.servers.get(id) else {
        return (StatusCode::NOT_FOUND, "unknown server").into_response();
    };
    let samples: Vec<HistorySample> = server
        .history
        .lock()
        .map(|h| h.iter().cloned().collect())
        .unwrap_or_default();
    Json(samples).into_response()
}

async fn get_clients(State(state): State<Arc<AppState>>, Path(id): Path<usize>) -> Response {
    let Some(server) = state.servers.get(id) else {
        return (StatusCode::NOT_FOUND, "unknown server").into_response();
    };
    let mut entries = server.entries.read().await.clone();
    entries.sort_by(|a, b| {
        (
            a.country_name.as_deref().unwrap_or(""),
            a.node.as_str(),
            a.pub_addr.as_str(),
        )
            .cmp(&(
                b.country_name.as_deref().unwrap_or(""),
                b.node.as_str(),
                b.pub_addr.as_str(),
            ))
    });
    Json(entries).into_response()
}

#[derive(Deserialize)]
struct EventQuery {
    page: Option<i64>,
    size: Option<i64>,
    search: Option<String>,
}

async fn get_events(
    State(state): State<Arc<AppState>>,
    Path(id): Path<usize>,
    Query(q): Query<EventQuery>,
) -> Response {
    if state.servers.get(id).is_none() {
        return (StatusCode::NOT_FOUND, "unknown server").into_response();
    }
    let page = q.page.unwrap_or(1).max(1);
    let size = q.size.unwrap_or(25).clamp(1, 1000);
    let search = q.search.unwrap_or_default();

    let total = match state.db.count(id as i64, &search).await {
        Ok(t) => t,
        Err(e) => {
            error!(error = %e, "event count failed");
            return (StatusCode::INTERNAL_SERVER_ERROR, "database error").into_response();
        }
    };
    let items = match state.db.page(id as i64, page, size, &search).await {
        Ok(i) => i,
        Err(e) => {
            error!(error = %e, "event page failed");
            return (StatusCode::INTERNAL_SERVER_ERROR, "database error").into_response();
        }
    };

    Json(json!({ "total": total, "page": page, "size": size, "items": items })).into_response()
}

async fn disconnect(
    State(state): State<Arc<AppState>>,
    Path((id, cid)): Path<(usize, u32)>,
) -> Response {
    let Some(server) = state.servers.get(id) else {
        return (StatusCode::NOT_FOUND, "unknown server").into_response();
    };
    if !server.online.load(Ordering::Relaxed) {
        return (StatusCode::SERVICE_UNAVAILABLE, "management connection is down").into_response();
    }
    match server.cmd_tx.send(Command::Disconnect(cid)) {
        Ok(_) => StatusCode::ACCEPTED.into_response(),
        Err(_) => (StatusCode::SERVICE_UNAVAILABLE, "server not connected").into_response(),
    }
}

async fn disconnect_all(State(state): State<Arc<AppState>>, Path(id): Path<usize>) -> Response {
    let Some(server) = state.servers.get(id) else {
        return (StatusCode::NOT_FOUND, "unknown server").into_response();
    };
    if !server.online.load(Ordering::Relaxed) {
        return (StatusCode::SERVICE_UNAVAILABLE, "management connection is down").into_response();
    }
    match server.cmd_tx.send(Command::DisconnectAll) {
        Ok(_) => StatusCode::ACCEPTED.into_response(),
        Err(_) => (StatusCode::SERVICE_UNAVAILABLE, "server not connected").into_response(),
    }
}

async fn refresh(State(state): State<Arc<AppState>>, Path(id): Path<usize>) -> Response {
    let Some(server) = state.servers.get(id) else {
        return (StatusCode::NOT_FOUND, "unknown server").into_response();
    };
    match server.cmd_tx.send(Command::Refresh) {
        Ok(_) => StatusCode::ACCEPTED.into_response(),
        Err(_) => (StatusCode::SERVICE_UNAVAILABLE, "server not connected").into_response(),
    }
}

#[derive(Deserialize)]
struct ExportQuery {
    format: Option<String>,
    search: Option<String>,
    limit: Option<i64>,
}

async fn export_events(
    State(state): State<Arc<AppState>>,
    Path(id): Path<usize>,
    Query(q): Query<ExportQuery>,
) -> Response {
    if state.servers.get(id).is_none() {
        return (StatusCode::NOT_FOUND, "unknown server").into_response();
    }
    let search = q.search.unwrap_or_default();
    let limit = q.limit.unwrap_or(100_000);
    let rows = match state.db.export(id as i64, &search, limit).await {
        Ok(r) => r,
        Err(e) => {
            error!(error = %e, "event export failed");
            return (StatusCode::INTERNAL_SERVER_ERROR, "database error").into_response();
        }
    };

    if q.format.as_deref() == Some("csv") {
        csv_response(id, &rows)
    } else {
        json_attachment(id, &rows)
    }
}

fn csv_field(value: &str) -> String {
    if value.contains([',', '"', '\n', '\r']) {
        format!("\"{}\"", value.replace('"', "\"\""))
    } else {
        value.to_string()
    }
}

fn csv_response(id: usize, rows: &[crate::db::EventRow]) -> Response {
    let mut out = String::from("timestamp,server,node,vpn,pub,event,country_code,country_name\n");
    for r in rows {
        out.push_str(&format!(
            "{},{},{},{},{},{},{},{}\n",
            r.timestamp,
            r.server,
            csv_field(&r.node),
            csv_field(r.vpn.as_deref().unwrap_or("")),
            csv_field(r.pub_addr.as_deref().unwrap_or("")),
            r.event,
            csv_field(r.country_code.as_deref().unwrap_or("")),
            csv_field(r.country_name.as_deref().unwrap_or("")),
        ));
    }
    attachment(out, "text/csv; charset=utf-8", &format!("events-server-{id}.csv"))
}

fn json_attachment(id: usize, rows: &[crate::db::EventRow]) -> Response {
    let body = serde_json::to_string(rows).unwrap_or_else(|_| "[]".to_string());
    attachment(body, "application/json", &format!("events-server-{id}.json"))
}

fn attachment(body: String, content_type: &'static str, filename: &str) -> Response {
    let mut res = body.into_response();
    res.headers_mut()
        .insert(header::CONTENT_TYPE, HeaderValue::from_static(content_type));
    if let Ok(hv) = HeaderValue::from_str(&format!("attachment; filename=\"{filename}\"")) {
        res.headers_mut()
            .insert(header::CONTENT_DISPOSITION, hv);
    }
    res
}

// ---------------------------------------------------------------------------
// WebSocket
// ---------------------------------------------------------------------------

async fn ws_handler(ws: WebSocketUpgrade, State(state): State<Arc<AppState>>) -> Response {
    let rx = state.tx.subscribe();
    ws.on_upgrade(move |socket| ws_loop(socket, rx, state))
}

async fn ws_loop(mut socket: WebSocket, mut rx: broadcast::Receiver<String>, state: Arc<AppState>) {
    state.metrics.ws_connections.fetch_add(1, Ordering::Relaxed);
    loop {
        tokio::select! {
            msg = rx.recv() => {
                match msg {
                    Ok(text) => {
                        if socket.send(Message::Text(text)).await.is_err() {
                            break;
                        }
                    }
                    Err(broadcast::error::RecvError::Lagged(_)) => continue,
                    Err(broadcast::error::RecvError::Closed) => break,
                }
            }
            incoming = socket.recv() => {
                match incoming {
                    Some(Ok(Message::Close(_))) | None => break,
                    Some(Ok(_)) => {}
                    Some(Err(_)) => break,
                }
            }
        }
    }
    state.metrics.ws_connections.fetch_sub(1, Ordering::Relaxed);
}

// ---------------------------------------------------------------------------
// Embedded static assets
// ---------------------------------------------------------------------------

async fn static_handler(uri: Uri) -> Response {
    let path = uri.path().trim_start_matches('/');
    let path = if path.is_empty() { "index.html" } else { path };
    match Assets::get(path) {
        Some(file) => {
            let mime = file.metadata.mimetype();
            ([(header::CONTENT_TYPE, mime)], file.data.into_owned()).into_response()
        }
        None => (StatusCode::NOT_FOUND, "Not found").into_response(),
    }
}
