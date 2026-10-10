"use strict";

/* ------------------------------------------------------------------ *
 * State
 * ------------------------------------------------------------------ */
const state = {
  config: {},
  servers: [],
  server: 0,
  clients: [],
  events: { items: [], total: 0, page: 1, size: 25 },
  history: [],
  search: "",
  tab: "clients",
  online: false,
  sortCol: "node",
  sortDir: 1,
};

const $ = (sel) => document.querySelector(sel);

async function api(path, opts) {
  const res = await fetch(path, opts);
  if (!res.ok) throw new Error(`${res.status} ${res.statusText}`);
  const ct = res.headers.get("content-type") || "";
  return ct.includes("application/json") ? res.json() : res.text();
}

/* ------------------------------------------------------------------ *
 * Formatting helpers
 * ------------------------------------------------------------------ */
function escapeHtml(s) {
  return String(s == null ? "" : s).replace(/[&<>"']/g, (c) => ({
    "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;", "'": "&#39;",
  }[c]));
}

function flagEmoji(code) {
  if (!code) return "🏳️";
  const cc = code.toUpperCase();
  if (!/^[A-Z]{2}$/.test(cc)) return "🏳️";
  return String.fromCodePoint(...[...cc].map((ch) => 127397 + ch.charCodeAt(0)));
}

function fmtTime(ts) {
  return ts ? new Date(ts * 1000).toLocaleString() : "";
}

function fmtDuration(from) {
  if (!from) return "";
  let s = Math.max(0, Math.floor(Date.now() / 1000) - from);
  const d = Math.floor(s / 86400); s %= 86400;
  const h = Math.floor(s / 3600); s %= 3600;
  const m = Math.floor(s / 60); s %= 60;
  if (d) return `${d}d ${h}h`;
  if (h) return `${h}h ${m}m`;
  if (m) return `${m}m ${s}s`;
  return `${s}s`;
}

function fmtBytes(n) {
  n = Number(n) || 0;
  const units = ["B", "KB", "MB", "GB", "TB"];
  let i = 0;
  while (n >= 1000 && i < units.length - 1) { n /= 1000; i++; }
  return `${n.toFixed(i ? 1 : 0)} ${units[i]}`;
}

function fmtRate(bps) {
  if (!bps || bps < 1) return "0 B/s";
  return `${fmtBytes(bps)}/s`;
}

function matches(c) {
  if (!state.search) return true;
  const q = state.search.toLowerCase();
  return [c.node, c.pub, c.vpn].some((v) => (v || "").toLowerCase().includes(q));
}

/* ------------------------------------------------------------------ *
 * Theme
 * ------------------------------------------------------------------ */
function applyTheme(t) {
  if (t) document.documentElement.dataset.theme = t;
  else delete document.documentElement.dataset.theme;
  localStorage.setItem("theme", t || "");
  $("#themeToggle").textContent = t === "dark" ? "🌙" : t === "light" ? "☀️" : "🌓";
}
function cycleTheme() {
  const order = ["", "light", "dark"];
  const cur = localStorage.getItem("theme") || "";
  applyTheme(order[(order.indexOf(cur) + 1) % order.length]);
}

/* ------------------------------------------------------------------ *
 * Clients view
 * ------------------------------------------------------------------ */
function renderClients() {
  const body = $("#clientsBody");
  const col = state.sortCol;
  const rows = state.clients.filter(matches).slice().sort((a, b) => {
    let av, bv;
    if (col === "connected") { av = a.connected || 0; bv = b.connected || 0; }
    else if (col === "traffic") { av = (a.received || 0) + (a.sent || 0); bv = (b.received || 0) + (b.sent || 0); }
    else { av = (a[col] || "").toString().toLowerCase(); bv = (b[col] || "").toString().toLowerCase(); }
    if (av < bv) return -1 * state.sortDir;
    if (av > bv) return 1 * state.sortDir;
    return 0;
  });

  body.innerHTML = rows.map((c) => `
    <tr data-cid="${c.cid}">
      <td class="c-flag"><span class="flag" title="${escapeHtml(c.country_name || "Unknown")}">${flagEmoji(c.country_code)}</span></td>
      <td>${escapeHtml(c.node)}</td>
      <td>${escapeHtml(c.vpn || "—")}</td>
      <td>${c.pub ? `<a href="https://${escapeHtml(c.pub)}" target="_blank" rel="noopener noreferrer">${escapeHtml(c.pub)}</a>` : "—"}</td>
      <td class="connected" title="${escapeHtml(fmtTime(c.connected))}">${fmtDuration(c.connected)}</td>
      <td class="traffic"><span class="up" title="sent">▲ ${fmtBytes(c.sent)}</span> <span class="down" title="received">▼ ${fmtBytes(c.received)}</span></td>
      <td class="rate"><span class="up" title="upload">▲ ${fmtRate(c.rate_up)}</span><br><span class="down" title="download">▼ ${fmtRate(c.rate_down)}</span></td>
      <td class="c-action"><button class="disc" data-cid="${c.cid}" title="Disconnect client">✖</button></td>
    </tr>`).join("");

  $("#clientsEmpty").classList.toggle("hidden", rows.length > 0);
}

function patchRow(cid) {
  const tr = document.querySelector(`tr[data-cid="${cid}"]`);
  const c = state.clients.find((x) => x.cid === cid);
  if (!tr || !c) return;
  const traffic = tr.querySelector(".traffic");
  if (traffic) {
    traffic.innerHTML = `<span class="up" title="sent">▲ ${fmtBytes(c.sent)}</span> <span class="down" title="received">▼ ${fmtBytes(c.received)}</span>`;
  }
  const rate = tr.querySelector(".rate");
  if (rate) {
    rate.innerHTML = `<span class="up" title="upload">▲ ${fmtRate(c.rate_up)}</span><br><span class="down" title="download">▼ ${fmtRate(c.rate_down)}</span>`;
  }
}

function refreshDurations() {
  const byCid = new Map(state.clients.map((c) => [String(c.cid), c]));
  document.querySelectorAll("#clientsBody tr[data-cid]").forEach((tr) => {
    const c = byCid.get(tr.dataset.cid);
    if (!c) return;
    const cell = tr.querySelector(".connected");
    if (cell) cell.textContent = fmtDuration(c.connected);
  });
}

/* ------------------------------------------------------------------ *
 * Events view
 * ------------------------------------------------------------------ */
function renderEvents() {
  const body = $("#eventsBody");
  body.innerHTML = state.events.items.map((e) => `
    <tr>
      <td class="c-flag"><span class="flag">${flagEmoji(e.country_code)}</span></td>
      <td>${escapeHtml(e.node)}</td>
      <td>${escapeHtml(e.vpn || "—")}</td>
      <td>${e.pub ? escapeHtml(e.pub) : "—"}</td>
      <td><span class="ev ${escapeHtml(e.event)}">${escapeHtml(e.event)}</span></td>
      <td>${escapeHtml(fmtTime(e.timestamp))}</td>
    </tr>`).join("");

  const total = state.events.total;
  const pages = Math.max(1, Math.ceil(total / state.events.size));
  $("#pageInfo").textContent = total ? `Page ${state.events.page} / ${pages} · ${total} events` : "No events";
  $("#prevPage").disabled = state.events.page <= 1;
  $("#nextPage").disabled = state.events.page >= pages;
}

/* ------------------------------------------------------------------ *
 * Stats view
 * ------------------------------------------------------------------ */
function renderStats() {
  const srv = state.servers.find((s) => s.id === state.server) || {};
  $("#statClients").textContent = state.clients.length;
  $("#statConnects").textContent = srv.connects_total ?? 0;
  $("#statDisconnects").textContent = srv.disconnects_total ?? 0;
  $("#statLatency").textContent = srv.latency_ms ? `${srv.latency_ms} ms` : "–";
  $("#statLast").textContent = srv.last_poll ? `${fmtDuration(srv.last_poll)} ago` : "–";
  drawChart();
}

function drawChart() {
  const canvas = $("#historyChart");
  if (!canvas) return;
  const dpr = window.devicePixelRatio || 1;
  const W = canvas.clientWidth, H = canvas.clientHeight;
  if (!W || !H) return;
  canvas.width = W * dpr;
  canvas.height = H * dpr;
  const ctx = canvas.getContext("2d");
  ctx.setTransform(dpr, 0, 0, dpr, 0, 0);
  ctx.clearRect(0, 0, W, H);

  const css = getComputedStyle(document.documentElement);
  const accent = css.getPropertyValue("--accent").trim() || "#0b6bcb";
  const grid = css.getPropertyValue("--grid").trim() || "rgba(127,127,127,0.2)";
  const muted = css.getPropertyValue("--muted").trim() || "#888";

  const data = state.history || [];
  const pad = { l: 36, r: 10, t: 12, b: 22 };
  const plotW = W - pad.l - pad.r;
  const plotH = H - pad.t - pad.b;

  const maxY = Math.max(1, ...data.map((d) => d.clients));
  const yScale = (v) => pad.t + plotH * (1 - v / maxY);

  // horizontal grid + y labels
  ctx.strokeStyle = grid;
  ctx.fillStyle = muted;
  ctx.font = "11px system-ui, sans-serif";
  ctx.textBaseline = "middle";
  const steps = 4;
  for (let i = 0; i <= steps; i++) {
    const v = (maxY * i) / steps;
    const y = yScale(v);
    ctx.beginPath();
    ctx.moveTo(pad.l, y);
    ctx.lineTo(W - pad.r, y);
    ctx.stroke();
    ctx.fillText(String(Math.round(v)), 4, y);
  }

  if (data.length < 2) {
    ctx.fillStyle = muted;
    ctx.textAlign = "center";
    ctx.fillText("Collecting history…", W / 2, H / 2);
    ctx.textAlign = "left";
    return;
  }

  const xScale = (i) => pad.l + plotW * (i / (data.length - 1));

  // area
  ctx.beginPath();
  ctx.moveTo(xScale(0), yScale(0));
  data.forEach((d, i) => ctx.lineTo(xScale(i), yScale(d.clients)));
  ctx.lineTo(xScale(data.length - 1), yScale(0));
  ctx.closePath();
  ctx.fillStyle = accent + "22";
  ctx.fill();

  // line
  ctx.beginPath();
  data.forEach((d, i) => {
    const x = xScale(i), y = yScale(d.clients);
    if (i === 0) ctx.moveTo(x, y);
    else ctx.lineTo(x, y);
  });
  ctx.strokeStyle = accent;
  ctx.lineWidth = 2;
  ctx.stroke();

  // x labels (first / last)
  ctx.fillStyle = muted;
  ctx.textBaseline = "top";
  ctx.fillText(fmtTime(data[0].t).replace(/.*, /, ""), pad.l, H - pad.b + 4);
  const last = fmtTime(data[data.length - 1].t).replace(/.*, /, "");
  ctx.textAlign = "right";
  ctx.fillText(last, W - pad.r, H - pad.b + 4);
  ctx.textAlign = "left";
}

async function loadHistory() {
  try {
    state.history = await api(`/api/servers/${state.server}/history`);
    renderStats();
  } catch { /* ignore */ }
}

/* ------------------------------------------------------------------ *
 * Data loading
 * ------------------------------------------------------------------ */
function rebuildServerSelect() {
  const sel = $("#serverSelect");
  sel.innerHTML = state.servers
    .map((s) => `<option value="${s.id}">${escapeHtml(s.name)}${s.online ? "" : " (offline)"}</option>`)
    .join("");
  sel.value = String(state.server);
}

async function refreshServers() {
  const servers = await api("/api/servers");
  const changed = servers.length !== state.servers.length
    || servers.some((s, i) => !state.servers[i] || state.servers[i].id !== s.id || state.servers[i].name !== s.name);
  state.servers = servers;
  if (changed) rebuildServerSelect();
  updateOnline();
}

async function loadClients() {
  state.clients = await api(`/api/servers/${state.server}/clients`);
  renderClients();
  if (state.tab === "map") MapView.setNodes(state.clients);
  if (state.tab === "stats") renderStats();
}

async function loadEvents() {
  const { page, size } = state.events;
  const qs = new URLSearchParams({ page: String(page), size: String(size), search: state.search });
  const data = await api(`/api/servers/${state.server}/events?${qs}`);
  state.events.items = data.items;
  state.events.total = data.total;
  renderEvents();
}

function updateOnline() {
  const srv = state.servers.find((s) => s.id === state.server);
  state.online = srv ? !!srv.online : false;
  $("#onlineDot").className = `dot ${state.online ? "online" : "offline"}`;
  $("#onlineDot").title = state.online ? "management connection up" : "management connection down";
  $("#latency").textContent = srv && srv.latency_ms ? `${srv.latency_ms} ms` : "";
}

/* ------------------------------------------------------------------ *
 * WebSocket live updates
 * ------------------------------------------------------------------ */
let ws = null;
let reconnectMs = 1000;

function connectWs() {
  const proto = location.protocol === "https:" ? "wss" : "ws";
  ws = new WebSocket(`${proto}://${location.host}/api/live`);
  ws.onopen = () => { reconnectMs = 1000; $("#connState").textContent = "● live"; };
  ws.onclose = () => {
    $("#connState").textContent = "○ reconnecting…";
    setTimeout(connectWs, reconnectMs);
    reconnectMs = Math.min(reconnectMs * 2, 15000);
  };
  ws.onmessage = ({ data }) => {
    let msg;
    try { msg = JSON.parse(data); } catch { return; }
    handleMessage(msg);
  };
}

function handleMessage(msg) {
  if (msg.kind === "server") {
    const srv = state.servers.find((s) => s.id === msg.server);
    if (srv) { srv.online = msg.online; if (msg.server === state.server) updateOnline(); }
    return;
  }
  if (msg.server !== state.server) return;

  if (msg.kind === "update") {
    for (const u of msg.clients) {
      const c = state.clients.find((x) => x.cid === u.cid);
      if (!c) continue;
      Object.assign(c, { received: u.received, sent: u.sent, rate_down: u.rate_down, rate_up: u.rate_up, seen: u.seen });
      patchRow(u.cid);
    }
    return;
  }

  if (msg.kind === "event") {
    const e = msg.client || {};
    if (e.node && msg.event !== "disconnect") {
      state.clients = state.clients.filter((c) => c.node !== e.node);
      state.clients.push(e);
      if (!e.cid) loadClients();
    } else if (e.node) {
      state.clients = state.clients.filter((c) => c.node !== e.node);
    }
    renderClients();

    toast(`${e.node || "client"} ${msg.event}ed`, msg.event);

    if (matches(e)) {
      state.events.items.unshift({
        id: null,
        timestamp: Math.floor(Date.now() / 1000),
        server: msg.server,
        node: e.node,
        event: msg.event,
        vpn: e.vpn,
        pub: e.pub,
        country_code: e.country_code,
        country_name: e.country_name,
      });
      state.events.items = state.events.items.slice(0, state.events.size);
      state.events.total++;
      renderEvents();
    }
  }
}

function toast(text, kind) {
  const div = document.createElement("div");
  div.className = `toast ${kind || ""}`;
  div.textContent = text;
  $("#toasts").appendChild(div);
  setTimeout(() => div.remove(), 4000);
}

/* ------------------------------------------------------------------ *
 * Minimal dependency-free slippy map (OSM tiles are a free service)
 * ------------------------------------------------------------------ */
const MapView = (() => {
  const TILE = 256;
  let el = null, layer = null;
  const tiles = new Map();
  let zoom = 2;
  let center = { lat: 25, lon: 0 };
  let nodes = [];
  let tileUrl = "";
  let ready = false;
  let dragging = null;

  function project(lat, lon, z) {
    const world = TILE * 2 ** z;
    const x = ((lon + 180) / 360) * world;
    const s = Math.sin((lat * Math.PI) / 180);
    const y = (0.5 - Math.log((1 + s) / (1 - s)) / (4 * Math.PI)) * world;
    return { x, y };
  }
  function unproject(x, y, z) {
    const world = TILE * 2 ** z;
    const lon = (x / world) * 360 - 180;
    const n = Math.PI - (2 * Math.PI * y) / world;
    const lat = (180 / Math.PI) * Math.atan(0.5 * (Math.exp(n) - Math.exp(-n)));
    return { lat, lon };
  }

  function render() {
    if (!el) return;
    const W = el.clientWidth, H = el.clientHeight;
    const c = project(center.lat, center.lon, zoom);
    const originX = c.x - W / 2, originY = c.y - H / 2;
    const nTiles = 2 ** zoom;

    if (tileUrl) {
      const tx0 = Math.floor(originX / TILE), tx1 = Math.floor((originX + W) / TILE);
      const ty0 = Math.floor(originY / TILE), ty1 = Math.floor((originY + H) / TILE);
      const needed = new Set();
      for (let ty = ty0; ty <= ty1; ty++) {
        if (ty < 0 || ty >= nTiles) continue;
        for (let tx = tx0; tx <= tx1; tx++) {
          const wx = ((tx % nTiles) + nTiles) % nTiles;
          const key = `${zoom}/${wx}/${ty}`;
          needed.add(key);
          let img = tiles.get(key);
          if (!img) {
            img = document.createElement("img");
            img.className = "tile";
            img.loading = "lazy";
            img.src = tileUrl.replace("{z}", zoom).replace("{x}", wx).replace("{y}", ty);
            img.onerror = () => { img.style.display = "none"; };
            tiles.set(key, img);
            el.insertBefore(img, layer);
          }
          img.style.left = tx * TILE - originX + "px";
          img.style.top = ty * TILE - originY + "px";
        }
      }
      for (const [key, img] of [...tiles]) {
        if (!needed.has(key)) { img.remove(); tiles.delete(key); }
      }
    }

    const pts = nodes.map((n) => project(n.lat, n.lon, zoom));
    layer.innerHTML = pts.map((p, i) => {
      const n = nodes[i];
      return `<div class="marker" style="left:${p.x - originX}px;top:${p.y - originY}px" title="${escapeHtml(n.node)}"><span class="pin">📍</span></div>`;
    }).join("");
  }

  function fit() {
    const pts = nodes.filter((n) => n.lat != null && n.lon != null);
    if (!pts.length || !el) return;
    let minLat = 90, maxLat = -90, minLon = 180, maxLon = -180;
    for (const n of pts) {
      minLat = Math.min(minLat, n.lat); maxLat = Math.max(maxLat, n.lat);
      minLon = Math.min(minLon, n.lon); maxLon = Math.max(maxLon, n.lon);
    }
    center = { lat: (minLat + maxLat) / 2, lon: (minLon + maxLon) / 2 };
    const W = el.clientWidth, H = el.clientHeight;
    let z = 1;
    for (; z <= 12; z++) {
      const a = project(maxLat, minLon, z), b = project(minLat, maxLon, z);
      if (Math.abs(b.x - a.x) > W - 100 || Math.abs(b.y - a.y) > H - 100) break;
    }
    zoom = Math.max(1, z - 1);
    render();
  }

  function zoomBy(delta) {
    const next = Math.min(18, Math.max(1, zoom + delta));
    if (next === zoom) return;
    zoom = next;
    render();
  }

  function onPointerDown(e) {
    dragging = { x: e.clientX, y: e.clientY };
    el.classList.add("dragging");
    el.setPointerCapture(e.pointerId);
  }
  function onPointerMove(e) {
    if (!dragging) return;
    const dx = e.clientX - dragging.x, dy = e.clientY - dragging.y;
    dragging = { x: e.clientX, y: e.clientY };
    const c = project(center.lat, center.lon, zoom);
    center = unproject(c.x - dx, c.y - dy, zoom);
    center.lat = Math.max(-85, Math.min(85, center.lat));
    render();
  }
  function onPointerUp(e) {
    dragging = null;
    el.classList.remove("dragging");
    if (e.pointerId != null && el.hasPointerCapture?.(e.pointerId)) el.releasePointerCapture(e.pointerId);
  }

  function init(tileUrlCfg) {
    el = $("#map");
    if (!el) return;
    tileUrl = tileUrlCfg || "";
    layer = document.createElement("div");
    layer.style.cssText = "position:absolute;inset:0;pointer-events:none;";
    el.appendChild(layer);
    el.addEventListener("pointerdown", onPointerDown);
    el.addEventListener("pointermove", onPointerMove);
    el.addEventListener("pointerup", onPointerUp);
    el.addEventListener("pointercancel", onPointerUp);
    el.addEventListener("wheel", (e) => { e.preventDefault(); zoomBy(e.deltaY < 0 ? 1 : -1); }, { passive: false });
    window.addEventListener("resize", () => { if (ready && !$("#view-map").classList.contains("hidden")) render(); });
    ready = true;
  }

  function setNodes(list) {
    nodes = list.filter((n) => n.lat != null && n.lon != null);
    const has = nodes.length > 0;
    $("#mapEmpty").classList.toggle("hidden", has);
    if (ready && has) fit();
  }

  return {
    init,
    render: () => { if (!ready) init(state.config.map_tile_url); setTimeout(render, 0); },
    setNodes,
    zoomIn: () => zoomBy(1),
    zoomOut: () => zoomBy(-1),
    fit,
  };
})();

/* ------------------------------------------------------------------ *
 * View/navigation wiring
 * ------------------------------------------------------------------ */
function switchTab(tab) {
  state.tab = tab;
  document.querySelectorAll(".tab").forEach((b) => b.classList.toggle("active", b.dataset.tab === tab));
  ["clients", "events", "map", "stats"].forEach((v) =>
    $(`#view-${v}`).classList.toggle("hidden", v !== tab));
  if (tab === "events") loadEvents();
  if (tab === "map") { MapView.render(); MapView.setNodes(state.clients); }
  if (tab === "stats") { loadHistory(); renderStats(); }
}

async function switchServer(id) {
  state.server = id;
  state.events.page = 1;
  state.events.items = [];
  state.history = [];
  updateOnline();
  try {
    await loadClients();
    if (state.tab === "events") await loadEvents();
    if (state.tab === "stats") await loadHistory();
  } catch (e) { toast(String(e), "disconnect"); }
}

/* ------------------------------------------------------------------ *
 * Boot
 * ------------------------------------------------------------------ */
async function boot() {
  applyTheme(localStorage.getItem("theme") || "");
  bindEvents();
  try {
    state.config = await api("/api/config");
    $("#buildInfo").textContent = `v${state.config.version}`;
    const t = await api("/api/time");
    serverTime = t.time || 0;
  } catch { /* ignore */ }

  try {
    await refreshServers();
    if (!state.servers.length) {
      toast("No servers configured", "disconnect");
    } else {
      if (!state.servers.some((s) => s.id === state.server)) state.server = state.servers[0].id;
      document.querySelectorAll(".tab").forEach((b) => b.classList.toggle("active", b.dataset.tab === state.tab));
      await switchServer(state.server);
    }
  } catch (e) { toast(`Failed to load: ${e}`, "disconnect"); }

  connectWs();
  let ticks = 0;
  setInterval(() => {
    if (state.tab === "clients") refreshDurations();
    if (state.tab === "stats") renderStats();
    tickTime();
    if (++ticks % 5 === 0) {
      refreshServers().catch(() => {});
      if (state.tab === "stats") loadHistory();
    }
  }, 1000);
}

let serverTime = 0;
function tickTime() {
  serverTime += 1;
  $("#serverTime").textContent = serverTime ? fmtTime(serverTime) : "";
}

function bindEvents() {
  $("#themeToggle").addEventListener("click", cycleTheme);
  $("#serverSelect").addEventListener("change", (e) => switchServer(Number(e.target.value)));
  document.querySelectorAll(".tab").forEach((b) => b.addEventListener("click", () => switchTab(b.dataset.tab)));

  let debounce;
  $("#search").addEventListener("input", (e) => {
    state.search = e.target.value.trim();
    if (state.tab === "clients") renderClients();
    if (state.tab === "map") MapView.setNodes(state.clients.filter(matches));
    clearTimeout(debounce);
    debounce = setTimeout(() => { if (state.tab === "events") { state.events.page = 1; loadEvents(); } }, 300);
  });

  $("#clientsBody").addEventListener("click", async (e) => {
    const btn = e.target.closest(".disc");
    if (!btn) return;
    const cid = btn.dataset.cid;
    btn.disabled = true;
    try {
      await api(`/api/servers/${state.server}/clients/${cid}/disconnect`, { method: "POST" });
      toast("Disconnect requested", "reconnect");
    } catch (err) {
      toast(`Disconnect failed: ${err}`, "disconnect");
    } finally {
      btn.disabled = false;
    }
  });

  document.querySelectorAll(".grid th[data-sort]").forEach((th) =>
    th.addEventListener("click", () => {
      const col = th.dataset.sort;
      state.sortDir = state.sortCol === col ? -state.sortDir : 1;
      state.sortCol = col;
      renderClients();
    }));

  $("#prevPage").addEventListener("click", () => { if (state.events.page > 1) { state.events.page--; loadEvents(); } });
  $("#nextPage").addEventListener("click", () => { state.events.page++; loadEvents(); });
  $("#pageSize").addEventListener("change", (e) => { state.events.size = Number(e.target.value); state.events.page = 1; loadEvents(); });

  $("#zoomIn").addEventListener("click", () => MapView.zoomIn());
  $("#zoomOut").addEventListener("click", () => MapView.zoomOut());
  $("#mapFit").addEventListener("click", () => MapView.fit());

  $("#refreshNow").addEventListener("click", async () => {
    try {
      await api(`/api/servers/${state.server}/refresh`, { method: "POST" });
      toast("Refresh requested", "connect");
      setTimeout(() => { loadClients(); loadHistory(); }, 600);
    } catch (err) { toast(`Refresh failed: ${err}`, "disconnect"); }
  });

  $("#exportCsv").addEventListener("click", () => exportEvents("csv"));
  $("#exportJson").addEventListener("click", () => exportEvents("json"));

  $("#disconnectAll").addEventListener("click", async () => {
    if (!confirm("Disconnect ALL clients from this server?")) return;
    try {
      await api(`/api/servers/${state.server}/clients/disconnect-all`, { method: "POST" });
      toast("Disconnecting all clients…", "reconnect");
    } catch (err) { toast(`Failed: ${err}`, "disconnect"); }
  });
}

function exportEvents(fmt) {
  const qs = new URLSearchParams({ format: fmt, search: state.search });
  window.location.href = `/api/servers/${state.server}/events/export?${qs}`;
}

window.addEventListener("resize", () => { if (state.tab === "stats") drawChart(); });

boot();
