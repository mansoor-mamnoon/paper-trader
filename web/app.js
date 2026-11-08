"use strict";

const USD = new Intl.NumberFormat("en-US", {style: "currency", currency: "USD"});
const money = (v) => USD.format(v);
const signedMoney = (v) => (v > 0 ? "+" : v < 0 ? "−" : "") + USD.format(Math.abs(v));
const pct = (v) => (v > 0 ? "+" : v < 0 ? "−" : "") + Math.abs(v).toFixed(2) + "%";
const $ = (id) => document.getElementById(id);
const sign = (v) => (v > 0.005 ? "pos" : v < -0.005 ? "neg" : "");

const rtf = new Intl.RelativeTimeFormat("en", {numeric: "auto"});
function ago(seconds) {
  const diff = seconds - Date.now() / 1000;
  const steps = [[86400, "day"], [3600, "hour"], [60, "minute"]];
  for (const [size, unit] of steps) if (Math.abs(diff) >= size) return rtf.format(Math.round(diff / size), unit);
  return "just now";
}
const when = (s) => new Date(s * 1000).toLocaleString(undefined, {dateStyle: "medium", timeStyle: "short"});

/** Build a DOM node. Text is always set as textContent, never as HTML. */
function h(tag, props = {}, ...children) {
  const node = document.createElement(tag);
  for (const [key, value] of Object.entries(props)) {
    if (key === "class") node.className = value;
    else if (value !== undefined && value !== null) node.setAttribute(key, value);
  }
  for (const child of children.flat()) if (child !== null && child !== undefined) node.append(child);
  return node;
}
const text = (id, value) => { $(id).textContent = value; };
function tone(id, value) {
  const node = $(id);
  node.classList.remove("pos", "neg");
  const cls = sign(value);
  if (cls) node.classList.add(cls);
}

const ACTIONS = {
  BUY_SIGNAL: ["Will buy", "buy"], SELL_SIGNAL: ["Will sell", "sell"],
  BUY_FILLED: ["Bought", "buy"], SELL_FILLED: ["Sold", "sell"],
  CANDIDATE: ["Passed screen", "info"], PASS: ["Did not qualify", ""],
  REJECT: ["Rejected", ""], EXPIRED: ["Expired", ""], FREEZE: ["Paused", ""],
  SKIP: ["Skipped", ""], CORPORATE_ACTION: ["Split or dividend", "info"], AVOID: ["Analyst veto", ""],
};
const label = (action) => ACTIONS[action] ?? [action, ""];

function parseEvidence(d) {
  try { return typeof d.evidence === "string" ? JSON.parse(d.evidence || "{}") : d.evidence ?? {}; }
  catch { return {}; }
}

/** One plain-English line for a decision row. */
function describe(d) {
  const e = parseEvidence(d);
  switch (d.action) {
    case "BUY_SIGNAL": return e.thesis ? `${d.symbol}: ${e.thesis}` : `${d.symbol} qualified. A buy is queued for the next session's open.`;
    case "AVOID": return `${d.symbol} passed the screen but the analyst passed on it: ${d.reason}`;
    case "SELL_SIGNAL": return `${d.symbol} hit its stop, its target, or the account halted. A sell is queued.`;
    case "BUY_FILLED": return `Bought ${d.symbol} at ${e.price ? money(e.price) : "the next open"}, after fees and slippage.`;
    case "SELL_FILLED": return `Sold ${d.symbol} at ${e.price ? money(e.price) : "the next open"}, after fees and slippage.`;
    case "CANDIDATE": return e.momentum20d === undefined ? d.reason
      : `${d.symbol} is up ${pct(e.momentum20d * 100)} over 20 days and trades above its 20 and 50-day averages.`;
    case "PASS": return e.momentum20d === undefined ? d.reason
      : `${d.symbol} did not meet every rule (20-day move ${pct(e.momentum20d * 100)}).`;
    default: return d.reason;
  }
}

let state = null;
let journalFilter = "activity";
let journalLimit = 25;

/* ---------- tabs ---------- */
const TABS = ["overview", "holdings", "trades", "journal", "lab", "about"];
function showTab(name, push = true) {
  if (!TABS.includes(name)) name = "overview";
  for (const id of TABS) $(id).hidden = id !== name;
  for (const b of document.querySelectorAll(".tabs button")) b.setAttribute("aria-selected", String(b.dataset.tab === name));
  if (push) history.replaceState(null, "", "#" + name);
  window.scrollTo({top: 0});
}
document.querySelector(".tabs").addEventListener("click", (e) => { const b = e.target.closest("button"); if (b) showTab(b.dataset.tab); });
document.addEventListener("click", (e) => { const b = e.target.closest("[data-goto]"); if (b) showTab(b.dataset.goto); });
window.addEventListener("hashchange", () => showTab(location.hash.slice(1), false));

/* ---------- tooltips ---------- */
const tooltip = $("tooltip");
function showTip(target) {
  tooltip.textContent = target.dataset.tip;
  tooltip.hidden = false;
  const r = target.getBoundingClientRect();
  const w = tooltip.offsetWidth;
  tooltip.style.left = Math.max(8, Math.min(window.innerWidth - w - 8, r.left + r.width / 2 - w / 2)) + "px";
  tooltip.style.top = r.bottom + 8 + "px";
}
for (const t of document.querySelectorAll(".tip")) {
  t.addEventListener("mouseenter", () => showTip(t));
  t.addEventListener("focus", () => showTip(t));
  t.addEventListener("click", () => showTip(t));
  t.addEventListener("mouseleave", () => { tooltip.hidden = true; });
  t.addEventListener("blur", () => { tooltip.hidden = true; });
}

/* ---------- chart ---------- */
const SVG = "http://www.w3.org/2000/svg";
function svg(name, attrs, content) {
  const n = document.createElementNS(SVG, name);
  for (const [k, v] of Object.entries(attrs)) n.setAttribute(k, v);
  if (content !== undefined) n.textContent = content;
  return n;
}

function drawChart(points) {
  const chart = $("chart");
  const tip = $("chart-tip");
  chart.replaceChildren(svg("title", {id: "chart-title"}, "Account value and SPY, in dollars"));
  tip.hidden = true;
  const W = 900, H = 280, L = 64, R = 16, T = 16, B = 28;
  if (points.length < 2) {
    chart.append(svg("text", {x: W / 2, y: H / 2, "text-anchor": "middle"},
      "The chart fills in after the second daily scan."));
    return;
  }
  const values = points.flatMap((p) => [p.equity, p.benchmark]).concat(1000);
  const pad = Math.max(2, (Math.max(...values) - Math.min(...values)) * 0.15);
  const lo = Math.min(...values) - pad, hi = Math.max(...values) + pad;
  const t0 = points[0].at, t1 = points[points.length - 1].at;
  const x = (t) => L + ((t - t0) / Math.max(1, t1 - t0)) * (W - L - R);
  const y = (v) => T + ((hi - v) / (hi - lo)) * (H - T - B);
  for (let i = 0; i <= 4; i++) {
    const v = hi - (i * (hi - lo)) / 4;
    chart.append(svg("line", {x1: L, x2: W - R, y1: y(v), y2: y(v), class: "grid"}));
    chart.append(svg("text", {x: L - 8, y: y(v) + 4, "text-anchor": "end"}, money(v)));
  }
  chart.append(svg("text", {x: L, y: H - 6}, new Date(t0 * 1000).toLocaleDateString()));
  chart.append(svg("text", {x: W - R, y: H - 6, "text-anchor": "end"}, new Date(t1 * 1000).toLocaleDateString()));
  const line = (key) => points.map((p, i) => `${i ? "L" : "M"}${x(p.at).toFixed(1)},${y(p[key]).toFixed(1)}`).join(" ");
  chart.append(svg("path", {d: `${line("equity")} L${x(t1)},${H - B} L${x(t0)},${H - B} Z`, class: "area"}));
  chart.append(svg("path", {d: line("benchmark"), class: "spy"}));
  chart.append(svg("path", {d: line("equity"), class: "acct"}));
  const cross = svg("line", {y1: T, y2: H - B, class: "cross", visibility: "hidden"});
  chart.append(cross);
  chart.onpointermove = (event) => {
    const box = chart.getBoundingClientRect();
    const px = ((event.clientX - box.left) / box.width) * W;
    let best = points[0];
    for (const p of points) if (Math.abs(x(p.at) - px) < Math.abs(x(best.at) - px)) best = p;
    cross.setAttribute("x1", x(best.at)); cross.setAttribute("x2", x(best.at));
    cross.setAttribute("visibility", "visible");
    tip.hidden = false;
    tip.replaceChildren(h("div", {}, new Date(best.at * 1000).toLocaleDateString()),
      h("div", {}, `Account ${money(best.equity)}`), h("div", {}, `SPY ${money(best.benchmark)}`));
    tip.style.left = `${Math.max(70, Math.min(box.width - 70, (x(best.at) / W) * box.width))}px`;
  };
  chart.onpointerleave = () => { cross.setAttribute("visibility", "hidden"); tip.hidden = true; };
}

/* ---------- next scheduled scan (22:37 UTC on weekdays) ---------- */
function nextScan() {
  const d = new Date();
  d.setUTCHours(22, 37, 0, 0);
  if (d <= new Date()) d.setUTCDate(d.getUTCDate() + 1);
  while ([0, 6].includes(d.getUTCDay())) d.setUTCDate(d.getUTCDate() + 1);
  return d;
}

/* ---------- render ---------- */
function renderOverview(s) {
  const a = s.account;
  const diff = a.equity - a.benchmark;
  const held = s.positions.length;
  const queued = Object.keys(s.pending).length;
  const replay = a.mode === "stocks-replay";

  let headline;
  if (a.halted) headline = `Halted: ${a.halt_reason}`;
  else if (!s.history.length) headline = "The account is waiting for its first scan";
  else if (Math.abs(diff) < 0.5) headline = `Level with SPY at ${money(a.equity)}`;
  else headline = `${diff > 0 ? "Ahead of" : "Behind"} SPY by ${money(Math.abs(diff))}`;
  text("headline", headline);
  text("subline", held
    ? `Holding ${held} stock${held > 1 ? "s" : ""}${queued ? ` with ${queued} order${queued > 1 ? "s" : ""} waiting for the next session.` : "."}`
    : queued ? `Holding cash. ${queued} order${queued > 1 ? "s are" : " is"} queued and will fill at the next session's open.`
      : "Holding cash. No stock has qualified yet, so nothing is queued.");

  text("v-account", money(a.equity));
  text("v-account-pct", pct(a.return_pct));
  text("v-spy", money(a.benchmark));
  text("v-spy-pct", pct(a.benchmark_return_pct));
  tone("v-account-pct", a.return_pct); tone("v-spy-pct", a.benchmark_return_pct);

  const day = a.started_at ? Math.min(60, Math.max(1, Math.floor(((replay ? a.last_at : Date.now() / 1000) - a.started_at) / 86400) + 1)) : 0;
  text("day", `Day ${day} of 60`);
  text("ends", a.end_at ? `Ends ${new Date(a.end_at * 1000).toLocaleDateString(undefined, {dateStyle: "medium"})}` : "");
  $("progress-fill").style.width = `${(day / 60) * 100}%`;
  $("progress").setAttribute("aria-valuenow", String(day));

  text("cash", money(a.cash));
  text("invested", `${money(a.equity - a.cash)} invested`);
  const total = a.equity - s.rules.starting_cash;
  text("pnl", signedMoney(total)); tone("pnl", total);
  text("pnl-detail", `${signedMoney(a.realized_pnl)} locked in · ${signedMoney(a.unrealized_pnl)} open`);
  text("drawdown", `${(a.max_drawdown * 100).toFixed(2)}%`);
  text("fees", money(a.fees));
  text("costs", `${(s.rules.fee * 100).toFixed(2)}% fee + ${(s.rules.slippage * 100).toFixed(2)}% slippage`);

  drawChart(s.history);

  const next = $("next"); next.replaceChildren();
  for (const [symbol, order] of Object.entries(s.pending)) {
    next.append(h("li", {}, h("strong", {}, `${order.side === "BUY" ? "Buy" : "Sell"} ${symbol}`),
      ` at the next session's open. Signal ${ago(order.at)}.`));
  }
  for (const p of s.positions) {
    next.append(h("li", {}, h("strong", {}, `Watch ${p.symbol}`),
      `: sell if it closes below ${money(p.stop)} or above ${money(p.target)}.`));
  }
  if (!next.children.length) next.append(h("li", {}, "Nothing is queued."));
  next.append(h("li", {}, h("strong", {}, "Next scan"), ` ${nextScan().toLocaleString(undefined, {weekday: "long", hour: "numeric", minute: "2-digit"})} (after the US close).`));
  const scan = s.last_scan ?? {};
  text("scan", a.last_at
    ? `Last scan ${ago(a.last_at)}: ${scan.candidates ?? 0} stocks passed the screen, ${scan.rejected ?? 0} did not.`
    : "No scan has run yet.");

  const recent = $("recent"); recent.replaceChildren();
  const items = (s.activity ?? []).slice(0, 5);
  for (const d of items) recent.append(entry(d));
  if (!items.length) recent.append(h("li", {}, h("span", {}), h("p", {class: "muted"}, "No trades or signals yet. The account only acts when a stock passes every rule.")));
}

function entry(d, big = false) {
  const [name, kind] = label(d.action);
  return h("li", {}, h("span", {class: `badge ${kind}`}, name),
    h("time", {datetime: new Date(d.at * 1000).toISOString()}, `${d.symbol === "ACCOUNT" ? "Account" : d.symbol} · ${big ? when(d.at) : ago(d.at)}`),
    h("p", {}, describe(d)), headlines(d));
}

function headlines(d) {
  const e = parseEvidence(d);
  const list = Array.isArray(e.headlines) ? e.headlines.slice(0, 2) : [];
  const extra = [];
  if (e.earnings_date) extra.push(h("small", {class: "muted"}, `Earnings ${e.earnings_date}`));
  if (!list.length && !extra.length) return null;
  return h("div", {class: "heads"}, list.map((t) => h("small", {class: "muted"}, `“${t}”`)), extra);
}

function renderStatus(s) {
  const a = s.account;
  const age = Date.now() / 1000 - a.last_at;
  const stale = a.mode !== "stocks-replay" && a.last_at && age > 4 * 86400;
  let kind = "ok", words = "Live daily account";
  if (a.mode === "stocks-replay") { kind = "warn"; words = "Replay scenario, not live results"; }
  else if (a.halted) { kind = "bad"; words = "Halted, no new buys"; }
  else if (stale) { kind = "bad"; words = "Snapshot is out of date"; }
  else if (s.health.status === "error") { kind = "bad"; words = "Last scan failed"; }
  else if (s.health.status === "degraded") { kind = "warn"; words = "Live, some prices missing"; }
  text("status", `${words}${a.last_at ? ` · updated ${ago(a.last_at)}` : ""}`);
  $("status-dot").className = `dot ${kind}`;
  const notice = $("notice");
  const warn = kind === "bad" || kind === "warn";
  notice.hidden = !warn;
  notice.textContent = a.halted ? `${a.halt_reason}. The dashboard stays up, and the account makes no new buys.`
    : stale ? "The published snapshot is more than four days old. Check the run history on GitHub."
      : s.health.message ?? "";
  text("health-detail", `Delayed daily stock data · ${s.coverage?.usable ?? 0} usable prices in the last scan`);
}

function renderHoldings(s) {
  const root = $("holdings-list"); root.replaceChildren();
  if (!s.positions.length) {
    root.append(h("div", {class: "card empty"}, "No stocks held. The account is in cash until something passes the screen and its order fills."));
    return;
  }
  for (const p of s.positions) {
    const span = p.target - p.stop;
    const at = Math.max(0, Math.min(100, ((p.entry - p.stop) / span) * 100));
    const ret = (p.unrealized_pnl / p.cost) * 100;
    root.append(h("article", {class: "card hold"},
      h("h3", {}, p.symbol, h("span", {class: `pl ${sign(p.unrealized_pnl)}`}, `${signedMoney(p.unrealized_pnl)} (${pct(ret)})`)),
      h("div", {class: "range", role: "img", "aria-label": `Entry sits ${at.toFixed(0)}% of the way from stop to target`}, h("i", {style: `left:${at}%`})),
      h("div", {class: "range-labels"}, h("span", {}, `Stop ${money(p.stop)}`), h("span", {}, `Target ${money(p.target)}`)),
      h("dl", {},
        h("dt", {}, "Shares"), h("dd", {}, String(p.quantity)),
        h("dt", {}, "Entry price"), h("dd", {}, money(p.entry)),
        h("dt", {}, "Value now"), h("dd", {}, money(p.market_value)),
        h("dt", {}, "Bought"), h("dd", {}, when(p.opened_at)))));
  }
}

function renderTrades(s) {
  const body = $("fills"); body.replaceChildren();
  $("fills-empty").hidden = s.fills.length > 0;
  text("fills-empty", "No trades yet. An order fills at a later session's open, so the first trade comes after the first signal.");
  for (const f of s.fills) {
    const result = f.side === "SELL" ? signedMoney(f.realized_pnl) : "";
    body.append(h("tr", {},
      h("td", {}, when(f.at)),
      h("td", {}, h("span", {class: `badge ${f.side === "BUY" ? "buy" : "sell"}`}, f.side === "BUY" ? "Bought" : "Sold")),
      h("td", {}, h("strong", {}, f.symbol)),
      h("td", {class: "num"}, String(f.quantity)),
      h("td", {class: "num"}, money(f.price)),
      h("td", {class: "num"}, money(f.fee)),
      h("td", {class: `num ${f.side === "SELL" ? sign(f.realized_pnl) : ""}`}, result)));
  }
}

function renderJournal() {
  if (!state) return;
  const query = $("search").value.trim().toUpperCase();
  const source = journalFilter === "activity" ? state.activity ?? [] : state.decisions;
  const rows = source.filter((d) => !query || d.symbol.includes(query));
  const list = $("journal-list"); list.replaceChildren();
  for (const d of rows.slice(0, journalLimit)) list.append(entry(d, true));
  $("journal-empty").hidden = rows.length > 0;
  text("journal-empty", query ? `No decisions found for “${query}”.`
    : journalFilter === "activity" ? "No trades or signals yet." : "No screening results yet.");
  $("more").hidden = rows.length <= journalLimit;
}
for (const chip of document.querySelectorAll(".chip")) {
  chip.addEventListener("click", () => {
    journalFilter = chip.dataset.filter; journalLimit = 25;
    for (const c of document.querySelectorAll(".chip")) c.setAttribute("aria-pressed", String(c === chip));
    renderJournal();
  });
}
$("search").addEventListener("input", () => { journalLimit = 25; renderJournal(); });
$("more").addEventListener("click", () => { journalLimit += 25; renderJournal(); });

function renderAbout(s) {
  const c = s.coverage ?? {};
  text("coverage", `${(c.universe_count ?? 0).toLocaleString()} US-listed stocks and ETFs are in the directory. The last scan requested ${c.attempted ?? 0}, got ${c.usable ?? 0} usable prices, and hit ${c.failed ?? 0} provider errors. Covering everything takes about ${c.rotation_cycles ?? "?"} daily scans, and holdings are checked every time.`);
}

const PARAM_LABELS = {
  min_price: ["Minimum price", (v) => money(v)],
  min_dollar_volume: ["Minimum daily trading", (v) => `$${(v / 1e6).toFixed(1)}M`],
  min_momentum: ["Minimum 20-day gain", (v) => `${(v * 100).toFixed(1)}%`],
  require_trend: ["Requires 20/50-day uptrend", (v) => (v ? "Yes" : "No")],
  stop_pct: ["Stop", (v) => `−${(v * 100).toFixed(1)}%`],
  target_pct: ["Target", (v) => `+${(v * 100).toFixed(1)}%`],
  earnings_blackout_days: ["Earnings blackout", (v) => (v ? `${v} days before a report` : "Off")],
  shortlist_size: ["Stocks shown to the analyst", (v) => String(v)],
  analyst_enabled: ["AI analyst", (v) => (v ? "On" : "Off")],
};
const EVENT_LABELS = {proposed: ["New challenger", "info"], promoted: ["Promoted", "buy"], reverted: ["Reverted", "sell"], retired: ["Retired", ""], rejected: ["Proposal rejected", ""], error: ["Error", "sell"]};

function renderLab(s) {
  const lab = s.strategy;
  if (!lab) {
    for (const id of ["analyst-view", "champion-why"]) text(id, "Strategy details appear after the next daily run.");
    return;
  }
  const a = lab.analyst;
  const claude = a.source === "claude";
  text("analyst-badge", claude ? "Claude" : "Momentum ranking");
  $("analyst-badge").className = `badge ${claude ? "buy" : ""}`;
  text("analyst-view", claude ? (a.market_view || "No market view recorded.") : "No AI analysis today.");
  text("analyst-note", claude ? `Model ${a.model.replace(/^claude \(|\)$/g, "")}. It read headlines and earnings dates for the stocks that passed the screen.`
    : a.error ? `The analyst failed, so stocks were ranked by 20-day gain. Reason: ${a.error}`
      : "The analyst is off or has no API key, so stocks were ranked by 20-day gain.");

  const c = lab.champion;
  text("champion-id", c.id);
  text("champion-why", c.rationale || "Original rules.");
  const params = $("champion-params"); params.replaceChildren();
  for (const [key, [name, fmt]] of Object.entries(PARAM_LABELS)) {
    if (c[key] !== undefined) params.append(h("div", {}, h("dt", {}, name), h("dd", {}, fmt(c[key]))));
  }
  text("champion-playbook", c.playbook);

  const box = $("shadows"); box.replaceChildren();
  text("lab-rule", `A challenger needs ${lab.rules.min_age_days} days and a ${lab.rules.margin_points}-point edge to be adopted.`);
  if (!lab.shadows.length) box.append(h("p", {class: "empty"}, "No challengers yet. The first proposal comes after the account has some history, then one new idea is tested each week."));
  for (const sh of lab.shadows) {
    const diff = sh.return_pct !== null && sh.champion_return_pct !== null ? sh.return_pct - sh.champion_return_pct : null;
    box.append(h("article", {class: "shadow"},
      h("div", {class: "head"}, h("strong", {}, sh.id), h("span", {class: `badge ${sh.kind === "previous" ? "" : "info"}`}, sh.kind === "previous" ? "Previous strategy" : "Challenger")),
      h("p", {class: "muted"}, sh.kind === "previous" ? "The strategy that was replaced. It keeps trading so a bad promotion can be undone." : sh.rationale),
      h("p", {class: "small"}, sh.changes.length ? `Changes: ${sh.changes.map((k) => PARAM_LABELS[k]?.[0] ?? k).join(", ")}` : ""),
      h("div", {class: "versus mini-versus"},
        h("div", {}, h("span", {class: "label"}, "Challenger"), h("strong", {}, sh.return_pct === null ? "waiting" : pct(sh.return_pct))),
        h("div", {class: "vs"}, "vs"),
        h("div", {}, h("span", {class: "label"}, "Live strategy"), h("strong", {}, sh.champion_return_pct === null ? "waiting" : pct(sh.champion_return_pct)))),
      h("p", {class: "small muted"}, `${Math.max(0, sh.age_days).toFixed(0)} days in${sh.days_to_decision > 0 ? `, ${Math.ceil(sh.days_to_decision)} more before a decision` : ", eligible for a decision"}${diff === null ? "" : `. Edge so far: ${diff >= 0 ? "+" : "−"}${Math.abs(diff).toFixed(2)} points.`}`)));
  }
  const log = $("lab-log"); log.replaceChildren();
  for (const e of lab.log) {
    const [name, kind] = EVENT_LABELS[e.event] ?? [e.event, ""];
    log.append(h("li", {}, h("span", {class: `badge ${kind}`}, name),
      h("time", {}, `${e.id} · ${ago(e.at)}`), h("p", {}, e.detail)));
  }
  if (!lab.log.length) log.append(h("li", {}, h("span", {}), h("p", {class: "muted"}, "Nothing has changed yet.")));
}

function render(s) {
  state = s;
  renderLab(s);
  renderStatus(s); renderOverview(s); renderHoldings(s); renderTrades(s); renderJournal(); renderAbout(s);
}

async function refresh() {
  try {
    const response = await fetch("api/state", {cache: "no-store", signal: AbortSignal.timeout(15000)});
    if (!response.ok) throw new Error(`HTTP ${response.status}`);
    render(await response.json());
  } catch {
    const notice = $("notice");
    notice.hidden = false;
    notice.textContent = "Could not load the latest snapshot. Showing the last values, retrying soon.";
    if (!state) text("headline", "Could not load the account");
  }
}

showTab(location.hash.slice(1), false);
refresh();
setInterval(refresh, 60000);
