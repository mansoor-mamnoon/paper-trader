"use strict";

const USD = new Intl.NumberFormat("en-US", {style: "currency", currency: "USD"});
const money = (v) => USD.format(v);
const qty = (v) => new Intl.NumberFormat("en-US", {maximumFractionDigits: 4}).format(v);
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
  DOCTOR_EXIT: ["Doctor: exit", "sell"], DOCTOR_TIGHTEN: ["Doctor: tighten", "info"], DOCTOR_HOLD: ["Doctor: hold", ""],
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
    case "DOCTOR_EXIT": return `${d.symbol}: the position doctor sold early. ${(d.reason || "").replace(/^Position doctor: /, "")}`;
    case "DOCTOR_TIGHTEN": return `${d.symbol}: stop moved up to protect the gain. ${d.reason}`;
    case "DOCTOR_HOLD": return `${d.symbol}: thesis still intact, holding. ${d.reason}`;
    case "AVOID": return `${d.symbol} passed the screen but the analyst passed on it: ${d.reason}`;
    case "SELL_SIGNAL": return `${d.symbol} hit its stop or its target, or the experiment ended. A sell is queued.`;
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
const TABS = ["overview", "desk", "holdings", "trades", "journal", "lab", "ops", "about"];
function showTab(name, push = true) {
  if (!TABS.includes(name)) name = "overview";
  for (const id of TABS) $(id).hidden = id !== name;
  for (const b of document.querySelectorAll(".tabs button")) b.setAttribute("aria-selected", String(b.dataset.tab === name));
  if (push) history.replaceState(null, "", "#" + name);
  if (window.ptTrack) window.ptTrack(name);
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
  const W = 900, H = 280, L = 92, R = 16, T = 16, B = 28;
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

  let lead = "", marked = "";
  if (a.halted) { lead = "Finished: "; marked = a.halt_reason; }
  else if (!s.history.length) { marked = "Waiting for the first scan"; }
  else if (Math.abs(diff) < 0.5) { lead = "Level with "; marked = `SPY at ${money(a.equity)}`; }
  else { lead = diff > 0 ? "Ahead of " : "Behind "; marked = `SPY by ${money(Math.abs(diff))}`; }
  $("headline").replaceChildren(lead, h("span", {class: "mark"}, marked));
  text("script-note", a.halted ? "the 60 days are over" : !s.history.length ? "no trades yet, just watching" :
    Math.abs(diff) < 0.5 ? "dead even with the market" : diff > 0 ? "beating the market, so far" : "not winning yet");
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
  const items = (s.activity ?? []).filter((d) => d.action !== "DOCTOR_HOLD").slice(0, 5);
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
  if (e.key_risk) extra.push(h("small", {class: "muted"}, `Risk: ${e.key_risk}`));
  if (e.invalidation) extra.push(h("small", {class: "muted"}, `Wrong if: ${e.invalidation}`));
  if (!list.length && !extra.length) return null;
  return h("div", {class: "heads"}, list.map((t) => h("small", {class: "muted"}, `“${t}”`)), extra);
}

function renderStatus(s) {
  const a = s.account;
  const age = Date.now() / 1000 - a.last_at;
  const stale = a.mode !== "stocks-replay" && a.last_at && age > 4 * 86400;
  let kind = "ok", words = "Live daily account";
  if (a.mode === "stocks-replay") { kind = "warn"; words = "Replay scenario, not live results"; }
  else if (a.halted) { kind = "bad"; words = "Experiment ended, no new buys"; }
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
        h("dt", {}, "Shares"), h("dd", {}, qty(p.quantity)),
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
      h("td", {class: "num"}, qty(f.quantity)),
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

const DEPTHS = ["No AI", "Single-shot", "Committee", "Full desk"];
const depthOf = (s, id) => {
  const sc = (s.strategy?.scorecards ?? []).find((x) => x.id === id);
  if (sc) return sc.depth;
  const m = /^depth(\d)/.exec(id);
  return m ? Number(m[1]) : "?";
};

function renderLadder(s) {
  const body = $("ladder"); body.replaceChildren();
  const cards = s.strategy?.scorecards ?? [];
  const shadows = new Map((s.strategy?.shadows ?? []).map((x) => [x.id, x]));
  const rows = cards.map((c) => {
    const p5 = c.scorecard?.kinds?.pick?.["5"] ?? {n: 0};
    return {c, p5, sh: shadows.get(c.id)};
  }).sort((a, b) => b.c.depth - a.c.depth);
  const eligible = rows.filter((r) => r.p5.n >= 5 && r.p5.mean_excess_pct !== null);
  const best = eligible.length ? eligible.reduce((a, b) => (b.p5.mean_excess_pct > a.p5.mean_excess_pct ? b : a)) : null;
  for (const r of rows) {
    const name = `${r.c.depth} · ${DEPTHS[r.c.depth] ?? "?"}`;
    const portfolio = r.c.kind === "champion" ? `${pct(s.account.return_pct)} (live)`
      : r.sh && r.sh.return_pct !== null && r.sh.champion_return_pct !== null
        ? `${pct(r.sh.return_pct)} vs ${pct(r.sh.champion_return_pct)}` : "waiting";
    const nameCell = h("td", {}, r === best ? h("span", {class: "mark"}, name) : name, r.c.kind === "champion" ? h("span", {class: "badge buy tiny"}, "live") : null);
    body.append(h("tr", {}, nameCell,
      h("td", {class: "num"}, portfolio),
      h("td", {class: "num"}, String(r.p5.n)),
      h("td", {class: `num ${r.p5.n ? sign(r.p5.mean_excess_pct) : ""}`}, r.p5.n ? pct(r.p5.mean_excess_pct) : "—"),
      h("td", {class: "num"}, r.p5.n ? `${Math.round(r.p5.hit_rate * 100)}%` : "—")));
  }
  if (!rows.length) body.append(h("tr", {}, h("td", {colspan: 5, class: "empty"}, "The ladder appears after the next daily run.")));
  const total = rows.reduce((m, r) => Math.max(m, r.p5.n), 0);
  text("ladder-note", total < 20
    ? `Needs about a week of picks to mean anything. The most any row has is ${total} scored so far.`
    : "Scores are each pick's return over 5 trading days minus SPY's over the same days. Picks, vetoes and near misses are all tracked, not just trades.");
  const box = $("calibration"); box.replaceChildren();
  const cal = (cards.find((c) => c.kind === "champion")?.scorecard?.calibration) ?? [];
  if (!cal.length) { box.append(h("p", {class: "empty"}, "Calibration shows up once the champion's picks have 5 days of results.")); return; }
  const scale = Math.max(1, ...cal.map((b) => Math.abs(b.mean_excess_pct ?? 0)));
  for (const b of cal) {
    const v = b.mean_excess_pct ?? 0;
    const width = `${Math.min(50, (Math.abs(v) / scale) * 50)}%`;
    const bar = h("i", {class: v >= 0 ? "up" : "down", style: `width:${width}`});
    box.append(h("div", {class: "bar-row"}, h("span", {class: "label"}, `${b.bucket} · n=${b.n}`),
      h("div", {class: "bar-track2"}, bar), h("strong", {class: sign(v)}, pct(v))));
  }
}

function renderLab(s) {
  const lab = s.strategy;
  if (!lab) {
    for (const id of ["analyst-view", "champion-why"]) text(id, "Strategy details appear after the next daily run.");
    return;
  }
  const a = lab.analyst;
  const ai = a.source === "llm";
  text("analyst-badge", ai ? "AI analyst" : "Momentum ranking");
  $("analyst-badge").className = `badge ${ai ? "buy" : ""}`;
  text("analyst-view", ai ? (a.market_view || "No market view recorded.") : "No AI analysis today.");
  text("analyst-note", ai ? `Model: ${a.model}. It read headlines and earnings dates for the stocks that passed the screen.`
    : a.error ? `The analyst failed, so stocks were ranked by 20-day gain. Reason: ${a.error}`
      : "The analyst is off or the model server was not available, so stocks were ranked by 20-day gain.");

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
      h("div", {class: "head"}, h("strong", {}, sh.id), h("span", {class: `badge ${sh.kind === "previous" ? "" : "info"}`}, sh.kind === "previous" ? "Previous strategy" : sh.kind === "ablation" ? `Ablation: depth ${depthOf(s, sh.id)}` : "Challenger")),
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

/* ---------- the desk ---------- */
const REGIMES = {risk_on: "Risk on", neutral: "Neutral", risk_off: "Risk off"};
const TOOLS = {read_filing: "Read filing", more_news: "More news", price_action: "Price action", finish: "Done"};
const clip = (t, n) => (String(t ?? "").length > n ? `${String(t).slice(0, n - 1)}…` : String(t ?? ""));
const nz = (v, f) => (v === null || v === undefined ? "n/a" : f(v));

function finalistCard(f, d, s) {
  const judge = d.judge ?? {};
  const tri = (d.triage ?? []).find((t) => t.symbol === f.symbol);
  const picked = (judge.picks ?? []).find((p) => p.symbol === f.symbol);
  const vetoed = (judge.avoid ?? []).find((p) => p.symbol === f.symbol);
  const verdict = picked
    ? h("span", {class: "badge buy"}, `PICKED · conviction ${Number(picked.conviction).toFixed(2)} · size ${Math.round((picked.size_fraction ?? 1) * 100)}%`)
    : vetoed ? h("span", {class: "badge sell"}, "VETOED")
      : h("span", {class: "badge"}, judge.reasoning ? "PASSED OVER" : "NOT DECIDED");
  const steps = (f.research?.steps ?? []);
  const trace = steps.length
    ? h("ol", {class: "trace"}, steps.map((st) => h("li", {},
      h("span", {class: "badge info"}, TOOLS[st.action] ?? st.action),
      h("p", {}, st.thought || ""),
      st.observation ? h("pre", {class: "obs"}, clip(st.observation, 260)) : null)))
    : h("p", {class: "muted small"}, "No research steps on this run (desk depth below 3, or no tools available).");
  const fi = f.research?.findings ?? {};
  const facts = (fi.key_facts ?? []).length ? h("ul", {class: "facts-list"}, fi.key_facts.map((x) => h("li", {}, x))) : null;
  const flags = (fi.red_flags ?? []).length ? h("ul", {class: "flags-list"}, fi.red_flags.map((x) => h("li", {}, h("span", {class: "badge sell tiny"}, "flag"), ` ${x}`))) : null;
  return h("article", {class: "card finalist"},
    h("div", {class: "head"}, h("h2", {}, f.symbol, tri ? h("span", {class: "score"}, `triage ${Number(tri.score).toFixed(0)}/10`) : null), verdict),
    tri?.why ? h("p", {class: "muted small"}, tri.why) : null,
    h("pre", {class: "dossier"}, (f.dossier ?? "").trim()),
    h("h3", {class: "sub-h"}, "Research trace"), trace,
    fi.summary ? h("p", {class: "lead2"}, fi.summary) : null, facts, flags,
    f.bull || f.bear ? h("div", {class: "debate"},
      h("div", {class: "side bull"}, h("span", {class: "label"}, "Bull"), h("p", {}, f.bull || "No argument returned.")),
      h("div", {class: "side bear"}, h("span", {class: "label"}, "Bear"), h("p", {}, f.bear || "No argument returned."))) : null,
    picked ? h("p", {class: "small verdict-line"}, h("strong", {}, "Thesis: "), picked.thesis,
      picked.key_risk ? h("span", {}, h("br"), h("strong", {}, "Key risk: "), picked.key_risk) : null,
      picked.invalidation ? h("span", {}, h("br"), h("strong", {}, "Wrong if: "), picked.invalidation) : null) : null,
    vetoed ? h("p", {class: "small verdict-line"}, h("strong", {}, "Why not: "), vetoed.reason) : null);
}

function renderDesk(s) {
  const d = s.strategy?.desk;
  const regime = $("desk-regime"); regime.replaceChildren();
  $("desk-funnel").replaceChildren(); $("desk-finalists").replaceChildren();
  $("desk-judge").replaceChildren(); $("desk-doctor").replaceChildren(); $("desk-memory").replaceChildren();
  $("desk-degraded").replaceChildren();
  $("desk-skipped-card").hidden = true;
  if (!d || !d.at) {
    text("desk-note", "The desk has not run yet.");
    regime.append(h("p", {class: "empty"}, "Today's committee transcript appears here after the next daily run."));
    return;
  }
  text("desk-note", `model ${d.model} · desk depth ${d.depth} · ran ${ago(d.at)} · ${Math.max(0, Math.round(d.minutes_left))} min of budget left`);
  const judge = d.judge ?? {};
  const rs = d.regime_stats ?? {};
  const label = REGIMES[judge.regime] ?? "No call";
  regime.append(
    h("div", {}, h("span", {class: "label"}, "Market regime"), h("strong", {class: "regime-label"}, h("span", {class: "mark"}, label)),
      judge.market_view ? h("p", {class: "script small-script"}, judge.market_view) : null),
    h("dl", {class: "regime-stats"},
      h("div", {}, h("dt", {}, "Stocks above 50-day avg"), h("dd", {}, nz(rs.pct_above_50d_avg, (v) => `${v.toFixed(0)}%`))),
      h("div", {}, h("dt", {}, "Median 20-day move"), h("dd", {}, nz(rs.median_20d_return_pct, (v) => pct(v)))),
      h("div", {}, h("dt", {}, "SPY 20-day move"), h("dd", {}, nz(rs.spy_20d_return_pct, (v) => pct(v)))),
      h("div", {}, h("dt", {}, "Stocks scanned"), h("dd", {}, nz(rs.stocks_scanned, String)))));

  const picks = (judge.picks ?? []).length;
  const funnel = [
    [s.last_scan ? (s.last_scan.candidates ?? 0) + (s.last_scan.rejected ?? 0) : rs.stocks_scanned, "scanned"],
    [s.last_scan?.candidates ?? "?", "passed the screen"],
    [(d.triage ?? []).length, "triaged"],
    [(d.finalists ?? []).length, "finalists"],
    [picks, "picked"],
  ];
  funnel.forEach(([n, name], i) => $("desk-funnel").append(h("li", {}, h("span", {class: "n"}, `0${i + 1}`), h("strong", {}, String(n ?? "?")), h("span", {}, name))));

  const box = $("desk-finalists");
  if (!(d.finalists ?? []).length) box.append(h("div", {class: "card"}, h("p", {class: "empty"}, "No stock needed a decision today: the account had no open slots or nothing passed the screen.")));
  for (const f of d.finalists ?? []) box.append(finalistCard(f, d, s));

  const jb = $("desk-judge");
  jb.append(h("div", {class: "head"}, h("h2", {}, "The judge's reasoning"), judge.regime ? h("span", {class: "badge"}, REGIMES[judge.regime] ?? judge.regime) : null));
  if (judge.reasoning) {
    jb.append(h("blockquote", {class: "quote"}, judge.reasoning));
    for (const p of judge.picks ?? []) {
      jb.append(h("div", {class: "pick-row"}, h("strong", {}, p.symbol), h("span", {class: "badge buy"}, `conviction ${Number(p.conviction).toFixed(2)} · size ${Math.round((p.size_fraction ?? 1) * 100)}%`),
        h("p", {}, p.thesis), h("p", {class: "small muted"}, `Key risk: ${p.key_risk || "none given"}. Wrong if: ${p.invalidation || "none given"}.`)));
    }
    if (!(judge.picks ?? []).length) jb.append(h("p", {class: "muted"}, "The judge chose no stock today. Choosing nothing is allowed."));
  } else {
    jb.append(h("p", {class: "muted"}, "The judge did not return a usable answer, so the triage scores decided instead. See “What was skipped” below."));
  }

  const dd = $("desk-doctor");
  if (!(d.doctor ?? []).length) dd.append(h("p", {class: "empty"}, "No holdings to review today."));
  for (const c of d.doctor ?? []) {
    const kind = c.action === "exit" ? "sell" : c.action === "tighten" ? "info" : "";
    dd.append(h("div", {class: "doc-row"}, h("div", {class: "head"}, h("strong", {}, c.symbol), h("span", {class: `badge ${kind}`}, c.action === "tighten" && c.stop_pct ? `tighten to ${(c.stop_pct * 100).toFixed(0)}% below` : c.action)), h("p", {class: "small"}, c.assessment)));
  }

  const mem = $("desk-memory");
  if (d.feedback) mem.append(h("p", {class: "small"}, h("strong", {}, "Its own track record: "), d.feedback));
  else mem.append(h("p", {class: "small muted"}, "Its track record shows up once it has about five scored picks."));
  const lessons = s.strategy?.lessons ?? [];
  if (!lessons.length) mem.append(h("p", {class: "small muted"}, "No lessons yet. After a trade closes, the model writes one sentence about what to do differently."));
  for (const l of lessons) mem.append(h("div", {class: "doc-row"}, h("div", {class: "head"}, h("strong", {}, l.symbol), h("span", {class: `badge ${l.pnl_pct >= 0 ? "buy" : "sell"}`}, pct(l.pnl_pct))), h("p", {class: "small"}, l.lesson)));

  if ((d.degraded ?? []).length) {
    $("desk-skipped-card").hidden = false;
    for (const x of d.degraded) $("desk-degraded").append(h("li", {}, x));
  }
}

/* ---------- roster and ops ---------- */
const ROLE_NAMES = {
  triage: "Triage", research: "Research agents", bull: "Bull case", bear: "Bear case", judge: "The judge",
  doctor: "Position doctor", post_mortem: "Post-mortem", proposer: "Strategy proposer", single_shot: "Single-shot analyst",
};
const roleName = (r) => ROLE_NAMES[r] ?? r;

function dur(ms) {
  if (ms === null || ms === undefined) return "n/a";
  if (ms < 1000) return `${Math.round(ms)} ms`;
  if (ms < 59500) return `${(ms / 1000).toFixed(1)} s`;
  const total = Math.round(ms / 1000);
  return `${Math.floor(total / 60)} min ${String(total % 60).padStart(2, "0")} s`;
}
const rate = (v) => (v === null || v === undefined ? "n/a" : `${(v * 100).toFixed(v >= 0.995 || v === 0 ? 0 : 1)}%`);
const fixed = (v, n = 1) => (v === null || v === undefined ? "n/a" : Number(v).toFixed(n));

function renderRoster(s) {
  const roster = s.strategy?.roster ?? [];
  $("desk-roster").hidden = !roster.length;
  const list = $("desk-roster-list"); list.replaceChildren();
  for (const r of roster) {
    list.append(h("li", {},
      h("span", {class: "label"}, roleName(r.role)),
      h("strong", {}, r.name),
      h("span", {class: "mono-note"}, `${r.company} · ${r.license} · ${r.size_mb >= 1000 ? `${(r.size_mb / 1000).toFixed(1)} GB` : `${r.size_mb} MB`}`)));
  }
}

function numCell(value, cls = "") { return h("td", {class: `num ${cls}`.trim()}, value); }

function renderOps(s) {
  const t = s.strategy?.telemetry;
  const ok = !!t?.current;
  $("ops-empty").hidden = ok;
  $("ops-content").hidden = !ok;
  if (!ok) return;
  const c = t.current;

  const status = (c.status ?? "ok").toUpperCase();
  const used = c.minutes_used ?? 0, budget = c.minutes_budget || 0;
  const stat = $("ops-status"); stat.replaceChildren(
    h("div", {class: "head"},
      h("div", {}, h("span", {class: "label"}, "Latest run"),
        h("strong", {class: "regime-label"}, h("span", {class: c.status === "failed" ? "mark ink" : "mark"}, status))),
      h("span", {class: "mono-note"}, c.started_at ? `started ${ago(c.started_at)}` : "")),
    h("div", {class: "budget"},
      h("div", {class: "row"}, h("span", {}, `${fixed(used, 1)} of ${fixed(budget, 0)} minutes of budget`), h("span", {class: "muted"}, budget ? `${Math.round((used / budget) * 100)}% used` : "")),
      h("div", {class: "bar-track", role: "progressbar", "aria-label": "Time budget used", "aria-valuemin": "0", "aria-valuemax": String(budget), "aria-valuenow": String(Math.round(used))},
        h("div", {class: "fill", style: `width:${budget ? Math.min(100, (used / budget) * 100) : 0}%`}))),
    h("dl", {class: "regime-stats"},
      h("div", {}, h("dt", {}, "Trigger"), h("dd", {}, c.trigger ?? "n/a")),
      h("div", {}, h("dt", {}, "Machine"), h("dd", {class: "small-dd"}, [c.cpu, c.cores ? `${c.cores} cores` : null, c.ram_mb ? `${Math.round(c.ram_mb / 1024)} GB RAM` : null].filter(Boolean).join(" · ") || "n/a")),
      h("div", {}, h("dt", {}, "Code version"), h("dd", {}, c.git_sha ?? "n/a")),
      h("div", {}, h("dt", {}, "Model calls"), h("dd", {}, String(t.runs?.[0]?.calls ?? "n/a")))));

  // Where the time went: the current run's stages.
  const stages = t.stages ?? [];
  const peak = Math.max(1, ...stages.map((x) => x.ms ?? 0));
  const topTotal = stages.filter((x) => !x.name.startsWith("cycle:") && x.name !== "cycles").reduce((a, x) => a + (x.ms ?? 0), 0) + (stages.find((x) => x.name === "cycles")?.ms ?? 0);
  text("ops-stage-total", stages.length ? `${dur(topTotal)} across ${stages.length} stages` : "");
  const wf = $("ops-waterfall"); wf.replaceChildren();
  if (!stages.length) wf.append(h("p", {class: "empty"}, "No stage timings were recorded for this run."));
  for (const x of stages) {
    const nested = x.name.startsWith("cycle:");
    wf.append(h("div", {class: `wf-row${nested ? " nested" : ""}`},
      h("span", {class: "wf-name"}, nested ? x.name.slice(6) : x.name),
      h("span", {class: "wf-track"}, h("span", {class: `wf-bar${x.ok === false ? " hatch" : ""}`, style: `width:${Math.max(0.6, ((x.ms ?? 0) / peak) * 100)}%`, title: x.note || ""})),
      h("span", {class: "wf-ms"}, dur(x.ms)),
      x.ok === false ? h("span", {class: "badge sell"}, "failed") : null));
  }

  // Bottlenecks over many runs.
  const hist = t.stage_history ?? [];
  const rows = $("ops-stage-rows"); rows.replaceChildren();
  const real = hist.filter((x) => !x.name.startsWith("cycle:"));
  const top = real[0];
  const topSum = real.filter((x) => x.name !== "cycles").reduce((a, x) => a + (x.mean_ms ?? 0), 0) || 1;
  text("ops-bottleneck", top
    ? `${top.name} is the slowest stage: ${dur(top.mean_ms)} on average, about ${Math.round(((top.mean_ms ?? 0) / topSum) * 100)}% of a typical run, and ${dur(top.p95_ms)} on a bad day.`
    : "Not enough runs yet to name a bottleneck.");
  const widest = Math.max(1, ...hist.map((x) => x.mean_ms ?? 0));
  for (const x of hist) {
    rows.append(h("tr", {class: (x.fail_rate ?? 0) > 0 ? "flag" : ""},
      h("td", {}, h("code", {}, x.name)), numCell(dur(x.mean_ms)), numCell(dur(x.p95_ms)), numCell(dur(x.max_ms)),
      numCell(rate(x.fail_rate ?? 0), (x.fail_rate ?? 0) > 0 ? "bad" : ""),
      h("td", {}, h("span", {class: "mini-track"}, h("span", {class: "mini-bar", style: `width:${Math.max(1, ((x.mean_ms ?? 0) / widest) * 100)}%`})))));
  }
  if (!hist.length) rows.append(h("tr", {}, h("td", {colspan: "6", class: "empty"}, "History builds up over the next few runs.")));

  // Run history strip.
  const runs = [...(t.runs ?? [])].reverse();
  const strip = $("ops-runs"); strip.replaceChildren();
  const top_ = Math.max(1, ...runs.map((r) => Math.max(r.minutes_used ?? 0, r.minutes_budget ?? 0)));
  const budgetNow = runs.find((r) => r.minutes_budget)?.minutes_budget;
  if (budgetNow) strip.append(h("span", {class: "budget-line", style: `bottom:${(budgetNow / top_) * 100}%`}));
  for (const r of runs) {
    const when = r.started_at ? new Date(r.started_at * 1000).toLocaleString(undefined, {dateStyle: "medium", timeStyle: "short"}) : r.run_id;
    const note = `${when}\n${r.status}: ${fixed(r.minutes_used, 1)} min, ${r.calls ?? 0} calls, ${r.errors ?? 0} errors${(r.degraded ?? []).length ? `\n${r.degraded.join("; ")}` : ""}`;
    strip.append(h("span", {class: `run-col ${r.status}`, style: `height:${Math.max(2, ((r.minutes_used ?? 0) / top_) * 100)}%`, title: note, tabindex: "0", "aria-label": note.replace(/\n/g, ", ")}));
  }
  if (!runs.length) strip.append(h("p", {class: "empty"}, "No runs recorded yet."));

  // By role.
  const rr = $("ops-role-rows"); rr.replaceChildren();
  for (const r of t.roles ?? []) {
    rr.append(h("tr", {class: (r.ok_rate ?? 1) < 1 ? "flag" : ""},
      h("td", {}, h("strong", {}, roleName(r.role))), h("td", {}, h("code", {}, r.model_id)), h("td", {}, r.company),
      numCell(String(r.calls ?? 0)), numCell(rate(r.ok_rate), (r.ok_rate ?? 1) < 1 ? "bad" : ""), numCell(dur(r.mean_latency_ms)), numCell(`${fixed(r.total_minutes, 1)} min`)));
  }
  if (!(t.roles ?? []).length) rr.append(h("tr", {}, h("td", {colspan: "7", class: "empty"}, "No model calls recorded yet.")));

  // Models with determinism.
  const det = Object.fromEntries((t.determinism ?? []).map((d) => [d.model_id, d]));
  const mr = $("ops-model-rows"); mr.replaceChildren();
  for (const m of t.models ?? []) {
    const d = det[m.model_id];
    const badge = d?.stable === true ? h("span", {class: "badge buy", title: d.canary_hash_latest}, "Stable")
      : d?.stable === false ? h("span", {class: "badge sell", title: `${d.canary_hash_previous} → ${d.canary_hash_latest}`}, "Drift")
        : h("span", {class: "badge", title: d?.canary_hash_latest ?? ""}, "Unchecked");
    mr.append(h("tr", {class: (m.errors ?? 0) > 0 || d?.stable === false ? "flag" : ""},
      h("td", {}, h("strong", {}, m.model_id), h("br"), h("span", {class: "mono-note"}, (m.roles ?? []).map(roleName).join(", "))), h("td", {}, m.company),
      numCell(String(m.calls ?? 0)), numCell(rate(m.ok_rate), (m.ok_rate ?? 1) < 1 ? "bad" : ""), numCell(dur(m.mean_latency_ms)), numCell(dur(m.p95_latency_ms)),
      numCell(fixed(m.mean_gen_tps)), numCell(fixed(m.mean_prompt_tps, 0)), numCell(String(m.errors ?? 0), (m.errors ?? 0) > 0 ? "bad" : ""), h("td", {}, badge)));
  }
  if (!(t.models ?? []).length) mr.append(h("tr", {}, h("td", {colspan: "10", class: "empty"}, "No model calls recorded yet.")));

  // Data sources.
  const fr = $("ops-fetch-rows"); fr.replaceChildren();
  for (const f of t.fetches ?? []) {
    fr.append(h("tr", {class: (f.ok_rate ?? 1) < 1 ? "flag" : ""},
      h("td", {}, h("code", {}, f.source)), numCell(String(f.requests ?? 0)), numCell(rate(f.ok_rate), (f.ok_rate ?? 1) < 1 ? "bad" : ""), numCell(dur(f.mean_ms)), numCell(dur(f.p95_ms))));
  }
  if (!(t.fetches ?? []).length) fr.append(h("tr", {}, h("td", {colspan: "5", class: "empty"}, "No data requests recorded yet.")));

  // Events, with problems first.
  const priority = {error: 0, degraded: 1, determinism: 2};
  const events = [...(t.events ?? [])].sort((a, b) => (priority[a.kind] ?? 9) - (priority[b.kind] ?? 9) || (b.at ?? 0) - (a.at ?? 0));
  const problems = events.filter((e) => e.kind === "error" || e.kind === "degraded").length;
  text("ops-event-count", `${problems} problem${problems === 1 ? "" : "s"} · ${events.length} events`);
  const ev = $("ops-events"); ev.replaceChildren();
  const kindClass = {error: "sell", degraded: "buy", determinism: "info"};
  for (const e of events) {
    ev.append(h("li", {}, h("span", {class: `badge ${kindClass[e.kind] ?? ""}`}, e.kind),
      h("time", {}, `${e.subject || "run"} · ${e.at ? ago(e.at) : ""}`), h("p", {}, e.detail)));
  }
  if (!events.length) ev.append(h("li", {}, h("span", {}), h("p", {class: "muted"}, "No events recorded. That is the good case.")));
}

function render(s) {
  state = s;
  renderLab(s); renderLadder(s); renderDesk(s); renderRoster(s); renderOps(s);
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

document.addEventListener("click", (e) => {
  const link = e.target.closest("a[href]");
  if (!link || !window.ptEvent) return;
  if (link.classList.contains("star")) window.ptEvent("click-star-on-github");
  else if (link.href.includes("github.com")) window.ptEvent("click-github");
  else if (link.href.includes("/export/") || link.href.includes("/api/")) window.ptEvent("download-data");
});
{
  const pa = window.ptAnalytics;
  $("privacy").textContent = pa && pa.ignored ? "This browser is excluded from visit counts."
    : pa && pa.providers.length ? "Anonymous visit statistics." : "";
}
showTab(location.hash.slice(1), false);
refresh();
setInterval(refresh, 60000);
