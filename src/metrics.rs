use std::sync::atomic::{AtomicI64, AtomicU64, Ordering};

use crate::state::AppState;

/// Cheap, dependency-free Prometheus-style metrics.
#[derive(Default)]
pub struct Metrics {
    pub events_total: AtomicU64,
    pub ws_connections: AtomicI64,
}

pub fn render(state: &AppState) -> String {
    let mut out = String::with_capacity(2048);

    out.push_str("# HELP openvpn_status_build_info Build information.\n");
    out.push_str("# TYPE openvpn_status_build_info gauge\n");
    out.push_str(&format!(
        "openvpn_status_build_info{{version=\"{}\"}} 1\n",
        env!("CARGO_PKG_VERSION")
    ));

    out.push_str("# HELP openvpn_status_servers Configured OpenVPN servers.\n");
    out.push_str("# TYPE openvpn_status_servers gauge\n");
    out.push_str(&format!("openvpn_status_servers {}\n", state.servers.len()));

    out.push_str("# HELP openvpn_status_server_online Whether the management connection is up.\n");
    out.push_str("# TYPE openvpn_status_server_online gauge\n");
    for s in &state.servers {
        out.push_str(&format!(
            "openvpn_status_server_online{{server=\"{}\",name=\"{}\"}} {}\n",
            s.id,
            escape(&s.name),
            s.online.load(Ordering::Relaxed) as u8
        ));
    }

    out.push_str("# HELP openvpn_status_clients Currently connected clients.\n");
    out.push_str("# TYPE openvpn_status_clients gauge\n");
    for s in &state.servers {
        let n = s.entries.try_read().map(|e| e.len()).unwrap_or(0);
        out.push_str(&format!("openvpn_status_clients{{server=\"{}\"}} {}\n", s.id, n));
    }

    out.push_str("# HELP openvpn_status_connects_total Total connect events observed.\n");
    out.push_str("# TYPE openvpn_status_connects_total counter\n");
    for s in &state.servers {
        out.push_str(&format!(
            "openvpn_status_connects_total{{server=\"{}\"}} {}\n",
            s.id,
            s.connects.load(Ordering::Relaxed)
        ));
    }

    out.push_str("# HELP openvpn_status_disconnects_total Total disconnect events observed.\n");
    out.push_str("# TYPE openvpn_status_disconnects_total counter\n");
    for s in &state.servers {
        out.push_str(&format!(
            "openvpn_status_disconnects_total{{server=\"{}\"}} {}\n",
            s.id,
            s.disconnects.load(Ordering::Relaxed)
        ));
    }

    out.push_str("# HELP openvpn_status_events_logged_total Total persisted events.\n");
    out.push_str("# TYPE openvpn_status_events_logged_total counter\n");
    out.push_str(&format!(
        "openvpn_status_events_logged_total {}\n",
        state.metrics.events_total.load(Ordering::Relaxed)
    ));

    out.push_str("# HELP openvpn_status_poll_latency_ms Round-trip time of the last status poll.\n");
    out.push_str("# TYPE openvpn_status_poll_latency_ms gauge\n");
    for s in &state.servers {
        out.push_str(&format!(
            "openvpn_status_poll_latency_ms{{server=\"{}\"}} {}\n",
            s.id,
            s.latency_ms.load(Ordering::Relaxed)
        ));
    }

    out.push_str("# HELP openvpn_status_history_points Retained history samples.\n");
    out.push_str("# TYPE openvpn_status_history_points gauge\n");
    for s in &state.servers {
        let n = s.history.lock().map(|h| h.len()).unwrap_or(0);
        out.push_str(&format!(
            "openvpn_status_history_points{{server=\"{}\"}} {}\n",
            s.id, n
        ));
    }

    out.push_str("# HELP openvpn_status_websocket_connections Active WebSocket connections.\n");
    out.push_str("# TYPE openvpn_status_websocket_connections gauge\n");
    out.push_str(&format!(
        "openvpn_status_websocket_connections {}\n",
        state.metrics.ws_connections.load(Ordering::Relaxed)
    ));

    out
}

fn escape(s: &str) -> String {
    s.replace('\\', "\\\\").replace('"', "\\\"")
}
