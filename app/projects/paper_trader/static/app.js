"use strict";
const money = new Intl.NumberFormat("en-US", {style: "currency", currency: "USD"});
function dollars(value) { return money.format(value); }
function percent(value) { return (value >= 0 ? "+" : "") + value.toFixed(2) + "%"; }
function date(value) { return new Date(value * 1000).toLocaleString(); }
function put(id, value) { document.getElementById(id).textContent = value; }
function signed(id, value, formatted) {
  const node = document.getElementById(id);
  node.textContent = formatted;
  node.classList.toggle("negative", value < 0);
  node.classList.toggle("positive", value > 0);
}
function cell(row, value) {
  const td = document.createElement("td"); td.textContent = value; row.append(td);
}
function svgElement(name, attributes, text) {
  const node = document.createElementNS("http://www.w3.org/2000/svg", name);
  for (const [key, value] of Object.entries(attributes)) node.setAttribute(key, value);
  if (text !== undefined) node.textContent = text;
  return node;
}
function chart(history) {
  const node = document.getElementById("chart"); node.replaceChildren();
  node.append(svgElement("title", {}, "Paper account and Bitcoin benchmark, in dollars"));
  if (!history.length) {
    node.append(svgElement("text", {x: 30, y: 110}, "The chart starts at the first fresh observation."));
    return;
  }
  const values = history.flatMap(p => [p.equity, p.benchmark, 1000]);
  const low = Math.min(...values) - 5, high = Math.max(...values) + 5;
  const first = history[0].at, last = history[history.length - 1].at;
  for (let i = 0; i < 5; i++) {
    const y = 16 + i * 48;
    node.append(svgElement("line", {x1: 65, y1: y, x2: 890, y2: y, class: "grid"}));
    node.append(svgElement("text", {x: 0, y: y + 4}, dollars(high - i * (high - low) / 4)));
  }
  for (const key of ["benchmark", "equity"]) {
    const points = history.map(p => [
      65 + (p.at - first) / Math.max(1, last - first) * 825,
      16 + (high - p[key]) / (high - low) * 192
    ]);
    const d = points.map((p, i) => (i ? "L" : "M") + p.join(",")).join(" ");
    node.append(svgElement("path", {d, class: key}));
    const end = points[points.length - 1];
    node.append(svgElement("circle", {cx: end[0], cy: end[1], r: 3,
      fill: key === "equity" ? "#70a7ff" : "#8291a6"}));
  }
  put("chart-start", date(first)); put("chart-end", date(last));
}
function render(state) {
  const a = state.account;
  put("equity", dollars(a.equity)); signed("return", a.return_pct, percent(a.return_pct) + " since inception");
  signed("total-pnl", a.equity - state.rules.starting_cash, dollars(a.equity - state.rules.starting_cash) + " total profit / loss");
  put("cash", dollars(a.cash));
  put("exposure", dollars(a.equity - a.cash) + " invested · " + dollars(a.fees) + " fees");
  signed("realized", a.realized_pnl, dollars(a.realized_pnl));
  put("unrealized", dollars(a.unrealized_pnl) + " unrealized");
  put("drawdown", (a.max_drawdown * 100).toFixed(2) + "%");
  put("benchmark", percent(a.benchmark_return_pct) + " since inception");
  const observedNow = a.mode === "replay" ? a.last_at : Date.now() / 1000;
  const elapsedDay = a.started_at ? Math.max(1, Math.floor((observedNow - a.started_at) / 86400) + 1) : 0;
  put("day", "Day " + Math.min(60, elapsedDay) + " / 60");
  put("risk", dollars(a.planned_risk) + " / " + dollars(a.equity * 0.03));
  put("status", a.mode === "replay" ? "Replay · not live performance" : a.halted ? "Experiment halted" : state.health.status === "ok" ? "Worker active · fresh data" : "Worker " + state.health.status);
  document.getElementById("status").classList.toggle("warning", a.halted || state.health.status !== "ok" || a.mode === "replay");
  put("updated", a.last_at ? "Last market check " + date(a.last_at) : "First market check pending");
  const notice = document.getElementById("notice");
  notice.hidden = !a.halted && state.health.status === "ok" && a.mode !== "replay";
  notice.textContent = a.mode === "replay" ? "REPLAY ACCOUNT · Scenario inputs, not live performance." :
    a.halted ? a.halt_reason + ". No new entries. Dashboard remains available." :
    "Market checks are not current. Values reflect the last recorded observation. " + (state.health.message || "Waiting for fresh data.");
  put("holdings-count", state.positions.length + " positions");
  document.getElementById("empty").hidden = state.positions.length > 0;
  put("empty", "No open positions. The account is in simulated cash.");
  const positions = document.getElementById("positions"); positions.replaceChildren();
  for (const p of state.positions) {
    const row = document.createElement("tr");
    [p.symbol, dollars(p.market_value), dollars(p.unrealized_pnl),
      dollars(p.stop) + " / " + dollars(p.target)].forEach(v => cell(row, v));
    positions.append(row);
  }
  const pending = Object.entries(state.pending).map(([symbol, p]) => p.side + " " + symbol);
  put("pending", pending.length ? "Queued, not filled: " + pending.join(" · ") : "No pending orders.");
  const fills = document.getElementById("fills"); fills.replaceChildren();
  document.getElementById("no-fills").hidden = state.fills.length > 0;
  put("no-fills", "No fills recorded. Signals wait for a later fresh market observation.");
  for (const f of state.fills) {
    const row = document.createElement("tr");
    [date(f.at), f.side, f.symbol, f.quantity.toFixed(6), dollars(f.price),
      dollars(f.fee), f.side === "SELL" ? dollars(f.realized_pnl) : "—"].forEach(v => cell(row, v));
    fills.append(row);
  }
  const journal = document.getElementById("decisions"); journal.replaceChildren();
  if (!state.decisions.length) journal.textContent = "No decisions recorded yet.";
  for (const d of state.decisions) {
    const row = document.createElement("div"); row.className = "entry";
    const time = document.createElement("time"); time.textContent = date(d.at);
    const tag = document.createElement("strong"); tag.textContent = d.symbol + " · " + d.action;
    const reason = document.createElement("p"); reason.textContent = d.reason;
    const evidence = JSON.parse(d.evidence);
    if (evidence.momentum24h !== undefined) {
      const detail = document.createElement("small");
      detail.textContent = "24h " + percent(evidence.momentum24h * 100) +
        " · SMA20 " + dollars(evidence.sma20) + " · SMA50 " + dollars(evidence.sma50);
      reason.append(detail);
    }
    row.append(time, tag, reason); journal.append(row);
  }
  chart(state.history);
  const events = document.getElementById("events"); events.replaceChildren();
  if (!state.events.length) events.textContent = "No operational events recorded.";
  for (const event of state.events) {
    const row = document.createElement("div"); row.className = "entry";
    const time = document.createElement("time"); time.textContent = date(event.at);
    const kind = document.createElement("strong"); kind.textContent = event.kind;
    const message = document.createElement("p"); message.textContent = event.message;
    row.append(time, kind, message); events.append(row);
  }
  put("received", "Dashboard snapshot received " + new Date().toLocaleString() + ". Times use your device’s timezone.");
  put("health-detail", "Kraken public data · " + (a.mode || "not started") + " · " +
    (a.last_at ? "observation age " + Math.floor(state.health.age_seconds / 60) + " min" : "no market observation yet"));
}
async function refresh() {
  try {
    const response = await fetch("/api/state", {cache: "no-store", signal: AbortSignal.timeout(15000)});
    if (!response.ok) throw new Error("HTTP " + response.status);
    render(await response.json());
  } catch (error) {
    const notice = document.getElementById("notice"); notice.hidden = false;
    notice.textContent = "Dashboard disconnected. Displayed values may be stale. Retrying automatically.";
    put("status", "Connection lost");
    document.getElementById("status").classList.add("warning");
  }
}
refresh();
setInterval(refresh, 30000);
