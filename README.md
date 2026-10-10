# openvpn-status-rust

A fast, asynchronous **Rust rewrite** of the classic web dashboard for monitoring
and administering one or more OpenVPN servers.

It talks to each OpenVPN **management interface** over TCP, parses the live client
list, enriches it with local GeoIP data, persists an event log to SQLite, and streams
updates to a dependency-free web UI over WebSocket.

> This repository began as a fork of `AuspeXeu/openvpn-status`.

---

## Highlights vs. the original

| Area | Original (Node.js, ~2015–2020) | This rewrite |
| --- | --- | --- |
| Runtime | Node 10 + Express + `ws` | Tokio + Axum, single static binary |
| OpenVPN client | callback state machine, single overwritten header list | async state machine, headers keyed per section, delimiter auto-detected |
| Persistence | Sequelize + SQLite, hardcoded DB password, promise "mutex" | `sqlx` + SQLite, no secrets, real async mutex, WAL |
| Geolocation | local MaxMind DB **and** outbound links to `geoiplookup.net` | **local only** — client IPs never leave the host |
| Frontend | Vue 2 + Vuetify 2 + Leaflet, built with Vue CLI, ~6 CDN dependencies | one self-contained vanilla ES module + CSS, **no CDN, no framework** |
| Flags | 240 bundled PNGs | Unicode flag emoji (zero assets) |
| Maps | Leaflet + terminator plugin from CDN | built-in ~150-line slippy map using free OpenStreetMap raster tiles |
| Auth | default `admin`/`admin` | **no default credentials**; constant-time Basic-auth compare |
| Reconnection | fixed 10 s retry | exponential backoff (1 s → 30 s) |
| Ops | none | `/healthz`, `/metrics` (Prometheus), structured logs, graceful shutdown |
| Config | `cfg.json` + env | `config.toml` + env, typed and validated |

### Removed as redundant / stale
- The entire Vue 2 / Vuetify 2 / Vuex / vue-router / axios frontend stack and its
  build toolchain (`@vue/cli-service`, Babel, ESLint-airbnb, husky, depcheck, ncu).
- The unused, misspelled `Clent` Sequelize model and its table.
- `moment.js` (replaced by `Intl.DateTimeFormat` in the browser).
- The `.jshintrc` legacy linter config.
- All third-party CDN `<script>`/`<link>` tags (Google Fonts, FontAwesome, MDI,
  jsDelivr, unpkg, cdnjs, ReconnectingWebSocket, Leaflet).
- Outbound `geoiplookup.net` hyperlinks.
- The hardcoded SQLite credentials committed in `database.js`.

### Third-party dependencies / services
Runtime dependencies are just crates from the standard Rust ecosystem — no SaaS, no
API keys, no telemetry. The only external service the **browser** contacts is the map
tile server, which is the free, no-key OpenStreetMap raster service
(`https://tile.openstreetmap.org/...`) and is fully configurable or disableable via
`map_tile_url = ""`. The server itself makes no outbound HTTP requests at all.

---

## Features

- Multi-server support with live per-server status.
- Real-time client list pushed over WebSocket (batched byte-count updates).
- **Live per-client upload/download throughput** derived from byte-count deltas.
- Connect / disconnect / reconnect events, coalesced and stored in SQLite.
- Searchable, paginated, persistent event log.
- **Event export** as CSV or JSON (download).
- Remote client disconnect, plus **bulk "disconnect all"**.
- **Stats tab** with a client-count history chart and connect/disconnect totals.
- **Management-interface poll latency** and last-update freshness indicator.
- **Forced refresh** of the current server on demand.
- **Event retention** (`retention_days`) with a periodic pruning task.
- Map view with client markers, pan/zoom/fit (dependency-free).
- Light / dark / auto **theme toggle** (persisted in `localStorage`).
- Optional HTTP Basic authentication (constant-time compare, no defaults).
- Prometheus metrics (`/metrics`) and a health endpoint (`/healthz`).
- Structured logs and graceful shutdown on Ctrl-C / SIGTERM.

---

## Requirements

- Rust 1.75+ (edition 2021)
- An OpenVPN server with the management interface enabled
- *(optional)* a MaxMind `GeoLite2-City.mmdb` for geolocation

---

## Build & run

```bash
cargo build --release
./target/release/openvpn-status          # reads ./config.toml
```

Then open <http://127.0.0.1:3013>.

### OpenVPN server configuration

Add to your `server.conf` and restart OpenVPN:

```
management 127.0.0.1 7656
# optional password file:
# management 127.0.0.1 7656 /etc/openvpn/mgmt.pw
```

---

## Configuration

Configuration is layered: **defaults → `config.toml` → `STATUS_*` environment
variables**. The config file path defaults to `./config.toml` and can be overridden
with `STATUS_CONFIG`.

```toml
bind = "127.0.0.1"
port = 3013

# Enable auth by setting BOTH username and password. No insecure defaults.
# username = "admin"
# password = "change-me"

database_url = "sqlite://data.sqlite"
# geoip_db = "GeoLite2-City.mmdb"

poll_interval_secs = 5         # how often a full `status 3` dump is requested
bytecount_interval_secs = 5    # `bytecount N` requested from OpenVPN
update_flush_secs = 5          # how often batched updates are pushed to browsers
event_dedup_secs = 60          # window for connect-after-disconnect -> reconnect
retention_days = 0             # delete events older than N days (0 = keep forever)
history_points = 720           # history samples kept per server for the chart
map_tile_url = "https://tile.openstreetmap.org/{z}/{x}/{y}.png"  # "" disables the map

[[servers]]
name = "Server A"
host = "127.0.0.1"
man_port = 7656
# man_pwd = "1337"
# netmask = "0.0.0.0/0"        # only merge ROUTING_TABLE rows for IPs inside this CIDR
```

Servers may also be dropped in as individual JSON files under `servers/` (the
directory name is configurable via `servers_dir`), which is convenient for Docker
volume mounts:

```json
{ "name": "Server A", "host": "127.0.0.1", "man_port": 7656, "man_pwd": "1337" }
```

### Environment variables

| Variable | Purpose |
| --- | --- |
| `STATUS_CONFIG` | Path to the TOML config file |
| `STATUS_BIND` | Bind address |
| `STATUS_PORT` | Listen port |
| `STATUS_USERNAME` / `STATUS_PASSWORD` | Basic-auth credentials |
| `STATUS_DATABASE_URL` | SQLite URL, e.g. `sqlite:///app/data/status.sqlite` |
| `STATUS_GEOIP_DB` | Path to a GeoLite2 `.mmdb` |
| `STATUS_MAP_TILE_URL` | Map tile template; empty disables the map |
| `RUST_LOG` | Log filter, e.g. `info` or `openvpn_status=debug,tower_http=warn` |

---

## HTTP API

All endpoints except `/healthz` and `/metrics` require Basic auth when it is enabled.

| Method | Path | Description |
| --- | --- | --- |
| `GET` | `/` | Web UI (embedded) |
| `GET` | `/healthz` | Liveness probe (`ok`) |
| `GET` | `/metrics` | Prometheus metrics |
| `GET` | `/api/config` | UI config (`version`, `map_tile_url`, intervals) |
| `GET` | `/api/time` | `{ "time": <unix> }` |
| `GET` | `/api/servers` | `[{ id, name, host, man_port, online, clients }]` |
| `GET` | `/api/servers/:id/clients` | Current clients (JSON) |
| `GET` | `/api/servers/:id/history` | Rolling `[{ t, clients, connects, disconnects, received, sent }]` |
| `GET` | `/api/servers/:id/events?page=&size=&search=` | `{ total, page, size, items }` |
| `GET` | `/api/servers/:id/events/export?format=csv\|json&search=` | Downloadable event log |
| `POST` | `/api/servers/:id/clients/:cid/disconnect` | `202` accepted, `503` if the server is offline |
| `POST` | `/api/servers/:id/clients/disconnect-all` | Disconnect every client |
| `POST` | `/api/servers/:id/refresh` | Force an immediate status poll |
| `GET` | `/api/live` | WebSocket upgrade |

### WebSocket protocol

The server pushes JSON frames of three shapes:

```jsonc
// a lifecycle event
{ "kind": "event",  "server": 0, "event": "connect|disconnect|reconnect", "client": { /* ClientEntry */ } }
// batched byte-count updates (every update_flush_secs)
{ "kind": "update", "server": 0, "clients": [ { /* ClientEntry */ } ] }
// management-connection status change
{ "kind": "server", "server": 0, "online": true }
```

`ClientEntry` shape:

```jsonc
{
  "cid": 1, "node": "client1", "pub": "203.0.113.5", "port": "1194",
  "vpn": "10.8.0.2", "received": 1000, "sent": 2000,
  "rate_down": 15234.5, "rate_up": 4096.0,
  "connected": 1577836800, "seen": 1577836950,
  "country_code": "DE", "country_name": "Germany", "lat": 51.0, "lon": 10.0
}
```

---

## Deployment

### systemd

```ini
[Unit]
Description=OpenVPN Status
After=network.target

[Service]
User=openvpn-status
WorkingDirectory=/opt/openvpn-status
ExecStart=/opt/openvpn-status/openvpn-status
Environment=STATUS_CONFIG=/etc/openvpn-status/config.toml
Restart=on-failure
RestartSec=5s

[Install]
WantedBy=multi-user.target
```

### Docker

```bash
docker build -t openvpn-status-rust .
docker run --rm -p 8080:3013 \
  -e STATUS_USERNAME=admin -e STATUS_PASSWORD=change-me \
  -v "$PWD/servers:/app/servers:ro" \
  openvpn-status-rust
```

A `docker-compose.sample.yml` is included.

### nginx reverse proxy (TLS termination + WebSocket)

```nginx
server {
  listen 443 ssl;
  server_name status.example.com;
  location / {
    proxy_pass http://127.0.0.1:3013;
    proxy_set_header Host $host;
    proxy_set_header X-Forwarded-Proto $scheme;
    proxy_http_version 1.1;
    proxy_set_header Upgrade $http_upgrade;
    proxy_set_header Connection "upgrade";
    proxy_read_timeout 86400;
  }
}
```

---

## Testing

```bash
cargo test        # parser unit tests (tab/comma formats, diffing, byte counts, netmask)
cargo clippy --all-targets
```

## Project layout

```
src/
  main.rs      startup, task wiring, graceful shutdown
  config.rs    TOML + env config, server discovery
  openvpn.rs   management-protocol parser + supervised connection task
  db.rs        SQLite event store (WAL, reconnect coalescing)
  geoip.rs     MaxMind GeoLite2 lookup
  metrics.rs   Prometheus text exporter
  state.rs     shared app/server state and types
  web.rs       Axum router, REST handlers, auth, WebSocket, embedded assets
web/
  index.html   single-page UI
  app.js       vanilla ES2020 client (no framework)
  style.css    responsive, dark-mode aware
config.toml          sample configuration
servers/             drop-in JSON server definitions
Dockerfile           multi-stage build
docker-compose.sample.yml
```

## License

MIT (inherited from the original project).
