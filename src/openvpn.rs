use std::{
    collections::HashMap,
    net::IpAddr,
    sync::Arc,
    time::{Duration, Instant},
};

use anyhow::Result;
use ipnet::IpNet;
use serde_json::json;
use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader},
    net::TcpStream,
    sync::mpsc,
    time::{interval, MissedTickBehavior},
};
use tracing::{debug, error, info, warn};

use crate::{
    db::NewEvent,
    geoip::GeoDb,
    state::{unix_now, AppState, ClientEntry, Command, HistorySample},
};

// ---------------------------------------------------------------------------
// Status parsing
// ---------------------------------------------------------------------------

/// Result of consuming one full `status 3` dump.
pub struct StatusDiff {
    pub snapshot: Vec<ClientEntry>,
    pub connected: Vec<ClientEntry>,
    pub disconnected: Vec<ClientEntry>,
}

/// Incremental parser for OpenVPN `status 3` output.
///
/// Unlike the original (which relied on a single, overwritten header list and
/// assumed record ordering), this keys headers by section name and auto-detects
/// the field delimiter, so it handles both the tab- and comma-separated
/// variants of the management interface.
#[derive(Default)]
pub struct StatusParser {
    headers: HashMap<String, Vec<String>>,
    previous: HashMap<String, ClientEntry>,
    current: HashMap<String, ClientEntry>,
    in_status: bool,
    /// When set, ROUTING_TABLE rows are only merged for public IPs inside it.
    netmask: Option<IpNet>,
}

impl StatusParser {
    pub fn new(netmask: Option<IpNet>) -> Self {
        Self {
            netmask,
            ..Default::default()
        }
    }

    /// Feed one line. Returns a diff when an `END` line closes a status dump.
    pub fn handle_line(&mut self, raw: &str) -> Option<StatusDiff> {
        let line = raw.trim_end_matches(['\r', '\n']);
        if line.is_empty() {
            return None;
        }
        let delim = if line.contains('\t') { '\t' } else { ',' };
        let parts: Vec<&str> = line.split(delim).map(str::trim).collect();
        let kind = parts[0];

        match kind {
            "TITLE" => {
                self.headers.clear();
                self.previous = std::mem::take(&mut self.current);
                self.in_status = true;
                None
            }
            "HEADER" if self.in_status => {
                if parts.len() >= 2 {
                    let section = parts[1].to_string();
                    let fields = parts[2.min(parts.len())..]
                        .iter()
                        .map(|s| s.to_string())
                        .collect();
                    self.headers.insert(section, fields);
                }
                None
            }
            "CLIENT_LIST" if self.in_status => {
                if let Some(fields) = self.headers.get("CLIENT_LIST").cloned() {
                    let map = zip(&fields, &parts[1..]);
                    if let Some(entry) = build_entry(&map) {
                        self.current.insert(entry.key.clone(), entry);
                    }
                }
                None
            }
            "ROUTING_TABLE" if self.in_status => {
                if let Some(fields) = self.headers.get("ROUTING_TABLE").cloned() {
                    let map = zip(&fields, &parts[1..]);
                    self.apply_routing(&map);
                }
                None
            }
            "END" if self.in_status => {
                self.in_status = false;
                Some(self.finish())
            }
            _ => None,
        }
    }

    /// Apply a `>BYTECOUNT_CLI` update to the live client list.
    pub fn update_bytecount(&mut self, cid: u32, received: u64, sent: u64) -> Option<ClientEntry> {
        let entry = self.current.values_mut().find(|c| c.cid == cid)?;
        entry.received = received;
        entry.sent = sent;
        Some(entry.clone())
    }

    /// Client IDs currently connected (used for kill-all).
    pub fn client_ids(&self) -> Vec<u32> {
        self.current.values().map(|c| c.cid).collect()
    }

    fn apply_routing(&mut self, map: &HashMap<String, String>) {
        let Some(real) = field(map, "Real Address") else {
            return;
        };
        let (pub_addr, port) = split_addr(real);
        if let Some(nm) = &self.netmask {
            match pub_addr.parse::<IpAddr>() {
                Ok(ip) if nm.contains(&ip) => {}
                _ => return,
            }
        }
        let seen = to_i64(field(map, "Last Ref (time_t)"));
        let vpn = field(map, "Virtual Address").map(str::to_string);
        for c in self.current.values_mut() {
            if c.pub_addr == pub_addr && c.port == port {
                if seen != 0 {
                    c.seen = seen;
                }
                if let Some(v) = &vpn {
                    if c.vpn.is_empty() {
                        c.vpn = v.clone();
                    }
                }
            }
        }
    }

    fn finish(&mut self) -> StatusDiff {
        let connected = self
            .current
            .iter()
            .filter(|(k, _)| !self.previous.contains_key(*k))
            .map(|(_, v)| v.clone())
            .collect();
        let disconnected = self
            .previous
            .iter()
            .filter(|(k, _)| !self.current.contains_key(*k))
            .map(|(_, v)| v.clone())
            .collect();
        let snapshot = self.current.values().cloned().collect();
        StatusDiff {
            snapshot,
            connected,
            disconnected,
        }
    }
}

fn zip(fields: &[String], values: &[&str]) -> HashMap<String, String> {
    let mut map = HashMap::with_capacity(fields.len());
    for (i, f) in fields.iter().enumerate() {
        if let Some(v) = values.get(i) {
            map.insert(f.clone(), v.to_string());
        }
    }
    map
}

fn field<'a>(map: &'a HashMap<String, String>, key: &str) -> Option<&'a str> {
    map.get(key)
        .map(String::as_str)
        .filter(|s| !s.is_empty() && *s != "UNDEF")
}

fn to_i64(v: Option<&str>) -> i64 {
    v.and_then(|s| s.parse().ok()).unwrap_or(0)
}

fn to_u64(v: Option<&str>) -> u64 {
    v.and_then(|s| s.parse().ok()).unwrap_or(0)
}

fn split_addr(real: &str) -> (String, String) {
    match real.rsplit_once(':') {
        Some((h, p)) => (h.to_string(), p.to_string()),
        None => (real.to_string(), String::new()),
    }
}

fn build_entry(map: &HashMap<String, String>) -> Option<ClientEntry> {
    let cn = field(map, "Common Name").or_else(|| field(map, "Username"));
    let cid_str = field(map, "Client ID");
    let key = cn.or(cid_str)?.to_string();

    let (pub_addr, port) = split_addr(field(map, "Real Address").unwrap_or(""));
    let node = cn
        .map(str::to_string)
        .unwrap_or_else(|| format!("Unknown_{pub_addr}"));

    Some(ClientEntry {
        key,
        cid: cid_str.and_then(|s| s.parse().ok()).unwrap_or(0),
        node,
        pub_addr,
        port,
        vpn: field(map, "Virtual Address").unwrap_or("").to_string(),
        received: to_u64(field(map, "Bytes Received")),
        sent: to_u64(field(map, "Bytes Sent")),
        connected: to_i64(field(map, "Connected Since (time_t)")),
        seen: to_i64(field(map, "Last Ref (time_t)")),
        rate_down: 0.0,
        rate_up: 0.0,
        country_code: None,
        country_name: None,
        lat: None,
        lon: None,
    })
}

fn parse_bytecount(rest: &str, parser: &mut StatusParser) -> Option<ClientEntry> {
    let mut it = rest.split(',');
    let cid: u32 = it.next()?.trim().parse().ok()?;
    let received: u64 = it.next()?.trim().parse().ok()?;
    let sent: u64 = it.next()?.trim().parse().ok()?;
    parser.update_bytecount(cid, received, sent)
}

// ---------------------------------------------------------------------------
// Management connection task
// ---------------------------------------------------------------------------

/// Supervises the TCP connection to one OpenVPN management interface, with
/// exponential-backoff reconnection.
pub async fn run_server(state: Arc<AppState>, sid: usize, mut cmd_rx: mpsc::UnboundedReceiver<Command>) {
    let sc = state.config.servers[sid].clone();
    let addr = format!("{}:{}", sc.host, sc.man_port);
    let mut backoff = 1u64;

    loop {
        match TcpStream::connect(&addr).await {
            Ok(stream) => {
                backoff = 1;
                set_online(&state, sid, true);
                info!(server = %sc.name, %addr, "connected to OpenVPN management console");
                if let Err(e) = session(&state, sid, stream, &mut cmd_rx).await {
                    warn!(server = %sc.name, error = %e, "management session ended");
                }
                set_online(&state, sid, false);
            }
            Err(e) => {
                warn!(server = %sc.name, %addr, error = %e, "cannot reach management console");
            }
        }
        tokio::time::sleep(Duration::from_secs(backoff)).await;
        backoff = (backoff * 2).min(30);
    }
}

async fn session(
    state: &Arc<AppState>,
    sid: usize,
    stream: TcpStream,
    cmd_rx: &mut mpsc::UnboundedReceiver<Command>,
) -> Result<()> {
    let sc = state.config.servers[sid].clone();
    let (rd, mut wr) = stream.into_split();

    let mut status_sent_at: Option<Instant> = None;

    if let Some(pwd) = sc.man_pwd.as_deref() {
        wr.write_all(format!("{pwd}\r\n").as_bytes()).await?;
        wr.flush().await?;
    }
    wr.write_all(format!("bytecount {}\r\n", state.config.bytecount_interval_secs).as_bytes())
        .await?;
    request_status(&mut wr, &mut status_sent_at).await?;

    // Dedicated reader task so the select! loop never loses a partially read line.
    let (line_tx, mut line_rx) = mpsc::unbounded_channel::<String>();
    tokio::spawn(async move {
        let mut reader = BufReader::new(rd);
        let mut buf = String::new();
        loop {
            buf.clear();
            match reader.read_line(&mut buf).await {
                Ok(0) | Err(_) => break,
                Ok(_) => {
                    if line_tx.send(std::mem::take(&mut buf)).is_err() {
                        break;
                    }
                }
            }
        }
    });

    let mut parser = StatusParser::new(parse_netmask(&sc.netmask));
    let mut pending: HashMap<u32, ClientEntry> = HashMap::new();
    // Previous byte counters per client (received, sent, at) for rate calculation.
    let mut last_bytes: HashMap<u32, (u64, u64, i64)> = HashMap::new();

    let mut poll = interval(Duration::from_secs(state.config.poll_interval_secs.max(1)));
    poll.set_missed_tick_behavior(MissedTickBehavior::Delay);
    poll.tick().await; // consume the immediate first tick

    let mut flush = interval(Duration::from_secs(state.config.update_flush_secs.max(1)));
    flush.set_missed_tick_behavior(MissedTickBehavior::Delay);
    flush.tick().await;

    loop {
        tokio::select! {
            line = line_rx.recv() => {
                let Some(line) = line else { break };
                let trimmed = line.trim_end_matches(['\r', '\n']);
                if trimmed.is_empty() {
                    continue;
                }
                if let Some(rest) = trimmed.strip_prefix(">BYTECOUNT_CLI:") {
                    if let Some(entry) = parse_bytecount(rest, &mut parser) {
                        pending.insert(entry.cid, entry);
                    }
                } else if trimmed.starts_with(">CLIENT:ESTABLISHED")
                    || trimmed.starts_with(">CLIENT:DISCONNECT")
                {
                    request_status(&mut wr, &mut status_sent_at).await?;
                } else if let Some(diff) = parser.handle_line(trimmed) {
                    finish_poll(state, sid, &mut status_sent_at);
                    apply_diff(state, sid, diff).await;
                }
            }
            _ = poll.tick() => {
                request_status(&mut wr, &mut status_sent_at).await?;
            }
            _ = flush.tick() => {
                if !pending.is_empty() {
                    let now = unix_now();
                    let batch: Vec<ClientEntry> = pending
                        .drain()
                        .map(|(_, mut e)| {
                            if let Some((lr, ls, lt)) = last_bytes.get(&e.cid).copied() {
                                let dt = (now - lt).max(1) as f64;
                                e.rate_down = e.received.saturating_sub(lr) as f64 / dt;
                                e.rate_up = e.sent.saturating_sub(ls) as f64 / dt;
                            }
                            last_bytes.insert(e.cid, (e.received, e.sent, now));
                            e
                        })
                        .collect();
                    apply_updates(state, sid, batch).await;
                }
            }
            cmd = cmd_rx.recv() => {
                match cmd {
                    Some(Command::Disconnect(cid)) => {
                        debug!(cid, "requesting client-kill");
                        wr.write_all(format!("client-kill {cid}\r\n").as_bytes()).await?;
                        wr.flush().await?;
                    }
                    Some(Command::DisconnectAll) => {
                        let ids = parser.client_ids();
                        debug!(count = ids.len(), "requesting client-kill for all clients");
                        for cid in ids {
                            wr.write_all(format!("client-kill {cid}\r\n").as_bytes()).await?;
                        }
                        wr.flush().await?;
                    }
                    Some(Command::Refresh) => {
                        request_status(&mut wr, &mut status_sent_at).await?;
                    }
                    None => break,
                }
            }
        }
    }
    Ok(())
}

async fn request_status<W>(wr: &mut W, at: &mut Option<Instant>) -> Result<()>
where
    W: tokio::io::AsyncWrite + Unpin,
{
    wr.write_all(b"status 3\r\n").await?;
    wr.flush().await?;
    *at = Some(Instant::now());
    Ok(())
}

fn finish_poll(state: &Arc<AppState>, sid: usize, at: &mut Option<Instant>) {
    use std::sync::atomic::Ordering;
    if let Some(t) = at.take() {
        state.servers[sid]
            .latency_ms
            .store(t.elapsed().as_millis() as u64, Ordering::Relaxed);
    }
    state.servers[sid].last_poll.store(unix_now(), Ordering::Relaxed);
}

// ---------------------------------------------------------------------------
// State application helpers
// ---------------------------------------------------------------------------

fn enrich(mut c: ClientEntry, geo: &Option<Arc<GeoDb>>) -> ClientEntry {
    if let Some(g) = geo {
        if let Some(loc) = g.lookup(&c.pub_addr) {
            c.country_code = loc.country_code;
            c.country_name = loc.country_name;
            c.lat = loc.lat;
            c.lon = loc.lon;
        }
    }
    c
}

async fn apply_diff(state: &Arc<AppState>, sid: usize, diff: StatusDiff) {
    let server = &state.servers[sid];
    let geo = &state.geo;

    let mut snapshot: Vec<ClientEntry> = diff
        .snapshot
        .into_iter()
        .map(|c| enrich(c, geo))
        .collect();
    let connected: Vec<ClientEntry> = diff
        .connected
        .into_iter()
        .map(|c| enrich(c, geo))
        .collect();
    let disconnected: Vec<ClientEntry> = diff
        .disconnected
        .into_iter()
        .map(|c| enrich(c, geo))
        .collect();

    use std::sync::atomic::Ordering;

    let (clients, received, sent) = {
        let mut entries = server.entries.write().await;
        // Preserve last-known rates across status polls (the status dump has none).
        let prev: HashMap<u32, (f64, f64)> = entries
            .iter()
            .map(|e| (e.cid, (e.rate_down, e.rate_up)))
            .collect();
        for e in snapshot.iter_mut() {
            if let Some((down, up)) = prev.get(&e.cid) {
                e.rate_down = *down;
                e.rate_up = *up;
            }
        }
        *entries = snapshot;
        (
            entries.len() as u32,
            entries.iter().map(|c| c.received).sum::<u64>(),
            entries.iter().map(|c| c.sent).sum::<u64>(),
        )
    };

    for c in &disconnected {
        server.disconnects.fetch_add(1, Ordering::Relaxed);
        record(state, sid, "disconnect", c).await;
    }
    for c in &connected {
        server.connects.fetch_add(1, Ordering::Relaxed);
        record(state, sid, "connect", c).await;
    }

    let sample = HistorySample {
        t: unix_now(),
        clients,
        connects: server.connects.load(Ordering::Relaxed),
        disconnects: server.disconnects.load(Ordering::Relaxed),
        received,
        sent,
    };
    if let Ok(mut history) = server.history.lock() {
        while history.len() >= state.config.history_points.max(1) {
            history.pop_front();
        }
        history.push_back(sample);
    }
}

async fn apply_updates(state: &Arc<AppState>, sid: usize, batch: Vec<ClientEntry>) {
    let server = &state.servers[sid];
    let enriched: Vec<ClientEntry> = batch.into_iter().map(|c| enrich(c, &state.geo)).collect();

    {
        let mut entries = server.entries.write().await;
        for u in &enriched {
            if let Some(e) = entries.iter_mut().find(|e| e.cid == u.cid) {
                e.received = u.received;
                e.sent = u.sent;
                e.rate_down = u.rate_down;
                e.rate_up = u.rate_up;
                if e.country_code.is_none() {
                    e.country_code = u.country_code.clone();
                    e.country_name = u.country_name.clone();
                    e.lat = u.lat;
                    e.lon = u.lon;
                }
                e.seen = u.seen.max(e.seen);
            }
        }
    }

    let msg = json!({ "kind": "update", "server": sid, "clients": enriched });
    let _ = state.tx.send(msg.to_string());
}

async fn record(state: &Arc<AppState>, sid: usize, event: &str, c: &ClientEntry) {
    let ev = NewEvent {
        timestamp: unix_now(),
        server: sid as i64,
        node: c.node.clone(),
        event: event.to_string(),
        vpn: opt(&c.vpn),
        pub_addr: opt(&c.pub_addr),
        country_code: c.country_code.clone(),
        country_name: c.country_name.clone(),
    };

    let final_event = match state.db.log(&ev, state.config.event_dedup_secs).await {
        Ok(e) => e,
        Err(e) => {
            error!(error = %e, "failed to persist event");
            event.to_string()
        }
    };
    state
        .metrics
        .events_total
        .fetch_add(1, std::sync::atomic::Ordering::Relaxed);

    let msg = json!({ "kind": "event", "server": sid, "event": final_event, "client": c });
    let _ = state.tx.send(msg.to_string());
}

fn set_online(state: &Arc<AppState>, sid: usize, online: bool) {
    let server = &state.servers[sid];
    server.online.store(online, std::sync::atomic::Ordering::Relaxed);
    let msg = json!({ "kind": "server", "server": sid, "online": online });
    let _ = state.tx.send(msg.to_string());
}

fn opt(s: &str) -> Option<String> {
    if s.is_empty() {
        None
    } else {
        Some(s.to_string())
    }
}

/// Parse a `netmask` config value into an [`IpNet`], if present and valid.
pub fn parse_netmask(netmask: &Option<String>) -> Option<IpNet> {
    netmask.as_deref().and_then(|s| s.parse().ok())
}

#[cfg(test)]
mod tests {
    use super::*;

    const STATUS_3: &str = "TITLE\tOpenVPN 2.6.0\n\
HEADER\tCLIENT_LIST\tCommon Name\tReal Address\tVirtual Address\tVirtual IPv6 Address\tBytes Received\tBytes Sent\tConnected Since\tConnected Since (time_t)\tUsername\tClient ID\tPeer ID\tData Channel Cipher\n\
CLIENT_LIST\tclient1\t203.0.113.5:1194\t10.8.0.2\t\t1000\t2000\tMon Jan 1\t1577836800\tUNDEF\t1\t0\tAES-256-GCM\n\
CLIENT_LIST\tclient2\t198.51.100.9:5000\t10.8.0.3\t\t10\t20\tMon Jan 1\t1577836900\tUNDEF\t2\t0\tAES-256-GCM\n\
HEADER\tROUTING_TABLE\tVirtual Address\tCommon Name\tReal Address\tLast Ref\tLast Ref (time_t)\n\
ROUTING_TABLE\t10.8.0.2\tclient1\t203.0.113.5:1194\tMon Jan 1\t1577836950\n\
GLOBAL_STATS\tMax bcast/mcast queue length\t0\n\
END";

    fn drive(parser: &mut StatusParser, dump: &str) -> Option<StatusDiff> {
        let mut diff = None;
        for line in dump.lines() {
            if let Some(d) = parser.handle_line(line) {
                diff = Some(d);
            }
        }
        diff
    }

    #[test]
    fn parses_tab_separated_status_3() {
        let mut p = StatusParser::new(None);
        let d = drive(&mut p, STATUS_3).expect("status diff");
        assert_eq!(d.snapshot.len(), 2);
        assert_eq!(d.connected.len(), 2);
        assert!(d.disconnected.is_empty());

        let c1 = d.snapshot.iter().find(|c| c.node == "client1").unwrap();
        assert_eq!(c1.cid, 1);
        assert_eq!(c1.pub_addr, "203.0.113.5");
        assert_eq!(c1.port, "1194");
        assert_eq!(c1.vpn, "10.8.0.2");
        assert_eq!(c1.received, 1000);
        assert_eq!(c1.sent, 2000);
        assert_eq!(c1.seen, 1577836950, "seen should come from ROUTING_TABLE");
    }

    #[test]
    fn detects_connect_and_disconnect_between_dumps() {
        let mut p = StatusParser::new(None);
        let dump1 = "TITLE\tx\n\
HEADER\tCLIENT_LIST\tCommon Name\tReal Address\tVirtual Address\tBytes Received\tBytes Sent\tConnected Since (time_t)\tClient ID\n\
CLIENT_LIST\tclient1\t1.2.3.4:1194\t10.8.0.2\t1\t2\t100\t1\nEND";
        drive(&mut p, dump1);

        let dump2 = "TITLE\tx\n\
HEADER\tCLIENT_LIST\tCommon Name\tReal Address\tVirtual Address\tBytes Received\tBytes Sent\tConnected Since (time_t)\tClient ID\n\
CLIENT_LIST\tclient2\t5.6.7.8:1194\t10.8.0.3\t3\t4\t200\t2\nEND";
        let d = drive(&mut p, dump2).expect("status diff");

        assert_eq!(d.connected.len(), 1);
        assert_eq!(d.connected[0].node, "client2");
        assert_eq!(d.disconnected.len(), 1);
        assert_eq!(d.disconnected[0].node, "client1");
    }

    #[test]
    fn parses_comma_separated_and_updates_bytecount() {
        let mut p = StatusParser::new(None);
        let dump = "TITLE,OpenVPN\n\
HEADER,CLIENT_LIST,Common Name,Real Address,Virtual Address,Bytes Received,Bytes Sent,Client ID\n\
CLIENT_LIST,client1,1.2.3.4:1194,10.8.0.2,10,20,7\nEND";
        drive(&mut p, dump);

        let u = p.update_bytecount(7, 500, 600).expect("bytecount match");
        assert_eq!((u.received, u.sent), (500, 600));
        assert!(p.update_bytecount(99, 1, 1).is_none());
    }

    #[test]
    fn netmask_filters_routing_table_merge() {
        let mut p = StatusParser::new(parse_netmask(&Some("10.0.0.0/8".to_string())));
        // Real Address is public (203.x) -> outside 10.0.0.0/8, so `seen` stays 0.
        let dump = "TITLE\tx\n\
HEADER\tCLIENT_LIST\tCommon Name\tReal Address\tVirtual Address\tConnected Since (time_t)\tClient ID\n\
CLIENT_LIST\tclient1\t203.0.113.5:1194\t10.8.0.2\t100\t1\n\
HEADER\tROUTING_TABLE\tVirtual Address\tCommon Name\tReal Address\tLast Ref (time_t)\n\
ROUTING_TABLE\t10.8.0.2\tclient1\t203.0.113.5:1194\t999\nEND";
        let d = drive(&mut p, dump).unwrap();
        assert_eq!(d.snapshot[0].seen, 0);
    }

    #[test]
    fn unknown_client_gets_synthetic_node() {
        let entry = build_entry(
            &[
                ("Real Address".to_string(), "9.9.9.9:1234".to_string()),
                ("Client ID".to_string(), "42".to_string()),
            ]
            .into_iter()
            .collect(),
        )
        .unwrap();
        assert_eq!(entry.node, "Unknown_9.9.9.9");
        assert_eq!(entry.cid, 42);
    }
}
