"use strict";
// Paper Trader dashboard: reads api/state.json written by the Rust desk.

const $ = (id) => document.getElementById(id);
const esc = (v) => String(v ?? "").replace(/[&<>"']/g, (c) => ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;", "'": "&#39;" }[c]));
const usd = (v) => (v == null || isNaN(v) ? "–" : "$" + Number(v).toLocaleString("en-US", { minimumFractionDigits: 2, maximumFractionDigits: 2 }));
const pct = (v) => (v == null || isNaN(v) ? "–" : (v >= 0 ? "+" : "") + Number(v).toFixed(2) + "%");
const tone = (v) => (v > 0 ? "pos" : v < 0 ? "neg" : "");
const date = (t) => (t ? new Date(t * 1000).toLocaleDateString("en-US", { month: "short", day: "numeric" }) : "–");
const when = (t) => (t ? new Date(t * 1000).toLocaleString("en-US", { month: "short", day: "numeric", hour: "numeric", minute: "2-digit" }) : "–");
let DATA = null;
let SECTOR_NAMES = {};
const sectorName = (id) => SECTOR_NAMES[id] || id || "";
let CARD_SECTOR = {};
const sectorOf = (symbol, id) => sectorName(id || CARD_SECTOR[symbol]);

function tabs() {
  const buttons = [...document.querySelectorAll(".tabs button")];
  const show = (name) => {
    buttons.forEach((b) => b.setAttribute("aria-selected", String(b.dataset.tab === name)));
    document.querySelectorAll(".view").forEach((v) => (v.hidden = v.id !== "view-" + name));
    if (name === "history") loadCalls();
  };
  buttons.forEach((b) => b.addEventListener("click", () => { history.replaceState(null, "", "#" + b.dataset.tab); show(b.dataset.tab); }));
  const start = location.hash.slice(1);
  show(buttons.some((b) => b.dataset.tab === start) ? start : "today");
}

function stats(d) {
  const a = d.account;
  const positions = d.positions.reduce((s, p) => s + (p.market_value || 0), 0);
  const cells = [
    ["Equity", usd(a.equity), `<span class="${tone(a.return_pct)}">${pct(a.return_pct)}</span> since ${usd(a.start)}`],
    ["SPY yardstick", usd(a.benchmark), `${pct(a.benchmark_return_pct)} over the same days`],
    ["Invested", usd(positions), `cash ${usd(a.cash)}`],
    ["Realized", usd(a.realized_pnl), `fees ${usd(a.fees)}, max drawdown ${(a.max_drawdown * 100).toFixed(1)}%`],
  ];
  $("stats").innerHTML = cells.map(([l, v, s]) => `<div class="card stat"><span class="label">${l}</span><strong>${v}</strong><small>${s}</small></div>`).join("");
}

function chart(points) {
  const svg = $("chart");
  if (!points.length) { svg.innerHTML = '<text x="20" y="40">No history yet</text>'; return; }
  const W = 900, H = 260, L = 56, R = 12, T = 14, B = 28;
  const values = points.flatMap((p) => [p.equity, p.benchmark]);
  let lo = Math.min(...values), hi = Math.max(...values);
  const pad = Math.max((hi - lo) * 0.12, 5); lo -= pad; hi += pad;
  const t0 = points[0].at, t1 = points[points.length - 1].at || t0 + 1;
  const x = (t) => L + ((t - t0) / Math.max(t1 - t0, 1)) * (W - L - R);
  const y = (v) => T + (1 - (v - lo) / (hi - lo)) * (H - T - B);
  const line = (k) => points.map((p, i) => (i ? "L" : "M") + x(p.at).toFixed(1) + " " + y(p[k]).toFixed(1)).join(" ");
  let grid = "";
  for (let i = 0; i <= 4; i++) {
    const v = lo + ((hi - lo) * i) / 4;
    grid += `<line class="grid" x1="${L}" x2="${W - R}" y1="${y(v)}" y2="${y(v)}"/><text x="4" y="${y(v) + 4}">$${v.toFixed(0)}</text>`;
  }
  grid += `<text x="${L}" y="${H - 6}">${date(t0)}</text><text x="${W - R - 50}" y="${H - 6}">${date(t1)}</text>`;
  svg.innerHTML = grid + `<path class="spy" d="${line("benchmark")}"/><path class="acct" d="${line("equity")}"/>`;
}

function holdings(d) {
  $("held-count").textContent = `${d.positions.length} of ${d.rules.max_positions}`;
  if (!d.positions.length) { $("holdings").innerHTML = '<p class="muted small">Nothing held yet. Purchases fill at the next open.</p>'; return; }
  const rows = d.positions
    .slice()
    .sort((a, b) => b.market_value - a.market_value)
    .map((p) => {
      const r = p.cost ? (p.market_value / p.cost - 1) * 100 : 0;
      return `<tr><td><b class="mono">${esc(p.symbol)}</b><div class="small muted">${esc(sectorOf(p.symbol, p.sector))}</div>${p.thesis ? `<div class="small">${esc(p.thesis)}</div>` : ""}</td>
        <td class="num">${usd(p.market_value)}</td><td class="num ${tone(r)}">${pct(r)}</td><td class="num">${date(p.opened_at)}</td></tr>`;
    }).join("");
  $("holdings").innerHTML = `<table><thead><tr><th>Stock</th><th class="num">Value</th><th class="num">Return</th><th class="num">Since</th></tr></thead><tbody>${rows}</tbody></table>`;
}

function pending(d) {
  const orders = Object.entries(d.pending || {});
  if (!orders.length) { $("pending").innerHTML = '<p class="muted small">No orders waiting.</p>'; return; }
  const total = orders.filter(([, o]) => o.side === "BUY").reduce((s, [, o]) => s + (o.weight || 1), 0);
  $("pending").innerHTML = '<ul class="list">' + orders
    .sort((a, b) => a[1].side.localeCompare(b[1].side) * -1)
    .map(([s, o]) => {
      const share = o.side === "BUY" && total ? ` · ${Math.round(((o.weight || 1) / total) * 100)}% of the cash` : "";
      return `<li><div class="line"><span class="badge ${o.side.toLowerCase()}">${o.side}</span><b>${esc(s)}</b><span class="small muted">${esc(sectorOf(s, o.sector))}${share}</span></div><div class="small">${esc(o.reason)}</div></li>`;
    }).join("") + '</ul><p class="small muted" style="margin-top:8px">Orders fill at the next session\'s open. Sales fill first, and purchases share all the cash by weight.</p>';
}

function today(d) {
  const r = d.latest;
  if (!r) { $("decisions").innerHTML = '<li class="muted">The desk has not run yet.</li>'; return; }
  $("today-day").textContent = r.day;
  const focus = (r.focus?.sectors || []).map(sectorName);
  $("focus").innerHTML = focus.length ? `<b>Sector focus:</b> ${esc(focus.join(", "))}. ${esc(r.focus.thesis)}` : "";
  const order = { SELL: 0, BUY: 1, TOP_UP: 2, KEEP: 3 };
  const list = (r.decisions || []).slice().sort((a, b) => (order[a.kind] ?? 9) - (order[b.kind] ?? 9));
  $("decisions").innerHTML = list.length
    ? list.map((e) => `<li><div class="line"><span class="badge ${e.kind.toLowerCase()}">${e.kind.replace("_", " ")}</span><b>${esc(e.symbol)}</b></div><div class="small">${esc(e.text)}</div></li>`).join("")
    : '<li class="muted small">No decisions recorded for this day.</li>';
  $("note").innerHTML = r.note ? `<b>Manager's note for tomorrow:</b> ${esc(r.note)}` : "";
  const problems = r.problems || [];
  if (problems.length) {
    $("notice").hidden = false;
    $("notice").innerHTML = `${problems.length} problem${problems.length > 1 ? "s" : ""} in the ${esc(r.day)} run: ` + problems.slice(0, 3).map(esc).join("; ") + (problems.length > 3 ? " …" : "");
  }
}

function newsbook(d) {
  const focus = new Set(d.latest?.focus?.sectors || []);
  const bySector = {};
  for (const n of d.newsbook || []) (bySector[n.sector] ||= []).push(n);
  const ids = Object.keys(bySector).sort((a, b) => (focus.has(b) - focus.has(a)) || (bySector[b][0].heat - bySector[a][0].heat));
  $("sectors").innerHTML = ids.length ? ids.map((id) => {
    const [now, ...before] = bySector[id];
    return `<div class="card sector ${focus.has(id) ? "focus" : ""}">
      <div class="head"><h3>${esc(sectorName(id))}</h3><span class="badge ${now.outlook === "bullish" ? "buy" : now.outlook === "bearish" ? "hot" : ""}">${esc(now.outlook)}</span></div>
      <div class="label">${esc(now.day)} · heat ${now.heat}/10</div><div class="heat"><span style="width:${now.heat * 10}%"></span></div>
      <p class="small"><b>${esc(now.driver)}.</b> ${esc(now.entry)}</p>
      ${before.length ? `<details><summary>Earlier entries (${before.length})</summary><ul class="list">${before.map((n) => `<li><span class="label">${esc(n.day)} · heat ${n.heat}</span><br>${esc(n.entry)}</li>`).join("")}</ul></details>` : ""}
    </div>`;
  }).join("") : '<div class="card muted">The newsbook is empty until the desk runs.</div>';
}

function factsLine(f) {
  const bits = [`${pct(f.change_1d)} today`];
  if (f.change_5d != null) bits.push(`${pct(f.change_5d)} 5d`);
  if (f.relative_volume != null) bits.push(`volume ${f.relative_volume.toFixed(1)}x`);
  if (f.reddit) bits.push(`Reddit ${f.reddit} (was ${f.reddit_before})`);
  if (f.trending?.length) bits.push("trending on " + f.trending.join(", "));
  return bits.join(" · ");
}

function desk(d) {
  const r = d.latest;
  const decided = {};
  for (const e of r?.decisions || []) decided[e.symbol] = e.kind;
  const cards = r?.cards || [];
  $("cards").innerHTML = cards.length ? cards.map((c) => {
    const heads = (c.headlines || []).slice(0, 6).map((h) => `<li><span class="badge ${["noise", "unrelated"].includes(h.label) ? "" : h.label === "bad_news" ? "hot" : "buy"}">${esc(h.label || "unlabelled")}${h.label ? " " + h.buzz : ""}</span> ${h.link ? `<a href="${esc(h.link)}" rel="noopener">${esc(h.title)}</a>` : esc(h.title)} <span class="muted">${esc(h.day)}</span></li>`).join("");
    const verdict = decided[c.symbol];
    return `<div class="card" style="margin-top:18px">
      <div class="head"><div class="line"><b class="mono" style="font-size:1.1rem">${esc(c.symbol)}</b><span>${esc(c.name)}</span>${c.holding ? '<span class="badge keep">held</span>' : '<span class="badge">candidate</span>'}${verdict ? `<span class="badge ${verdict.toLowerCase()}">${verdict.replace("_", " ")}</span>` : ""}</div><span class="label">${esc(sectorName(c.sector))}</span></div>
      <p class="small mono">${esc(factsLine(c.facts))}</p>
      ${heads ? `<ul class="heads">${heads}</ul>` : '<p class="small muted">No headlines found.</p>'}
      <div class="argue">
        <div><span class="label">Analyst${c.case ? ` · ${esc(c.case.catalyst)} · upside ${c.case.upside}/10` : ""}</span><p class="small">${c.case ? esc(c.case.thesis) + `<br><i>Wrong if: ${esc(c.case.kill)}</i>` : "No case was made."}</p></div>
        <div class="skeptic"><span class="label">Skeptic${c.rebuttal ? ` · case ${esc(c.rebuttal.verdict)}${c.rebuttal.priced_in ? " · priced in" : ""}` : ""}</span><p class="small">${c.rebuttal ? esc(c.rebuttal.objection) : "No rebuttal."}</p></div>
      </div></div>`;
  }).join("") : '<div class="card muted">No stocks were argued yet.</div>';
}

function historyView(d) {
  $("days").innerHTML = (d.days || []).map((day) => {
    const moves = (day.decisions || []).filter((e) => e.kind !== "KEEP").map((e) => `<span class="badge ${e.kind.toLowerCase()}">${e.kind.replace("_", " ")} ${esc(e.symbol)}</span>`).join(" ");
    const kept = (day.decisions || []).filter((e) => e.kind === "KEEP").map((e) => e.symbol);
    return `<li><div class="line"><b>${esc(day.day)}</b><span class="small muted">${esc((day.focus?.sectors || []).map(sectorName).join(", "))}</span></div>
      <div style="margin:6px 0">${moves || '<span class="small muted">No changes.</span>'}${kept.length ? ` <span class="small muted">kept ${esc(kept.join(", "))}</span>` : ""}</div>
      ${day.note ? `<div class="small">${esc(day.note)}</div>` : ""}
      <div class="small muted mono">${day.funnel ? `${day.funnel.eligible} eligible · ${day.funnel.screened} screened · ${day.funnel.candidates} candidates · ${day.funnel.calls} model calls · ${Math.round(day.funnel.minutes)} min` : ""}</div></li>`;
  }).join("") || '<li class="muted">No days yet.</li>';
  const trades = d.trades || [];
  $("trades").innerHTML = trades.length ? `<table><thead><tr><th>Stock</th><th>Catalyst</th><th class="num">Return</th><th class="num">Held</th><th>Why sold</th></tr></thead><tbody>${trades.map((t) => `<tr><td><b class="mono">${esc(t.symbol)}</b></td><td>${esc(t.catalyst || "–")}</td><td class="num ${tone(t.return_pct)}">${pct(t.return_pct)}</td><td class="num">${Math.max(0, Math.round((t.closed_at - t.opened_at) / 86400))}d</td><td class="small">${esc(t.exit_reason)}</td></tr>`).join("")}</tbody></table>` : '<p class="muted small">No closed trades yet.</p>';
  const fills = d.fills || [];
  $("fills").innerHTML = fills.length ? `<table><thead><tr><th>When</th><th>Side</th><th>Stock</th><th class="num">Shares</th><th class="num">Price</th><th class="num">P&amp;L</th></tr></thead><tbody>${fills.slice(0, 60).map((f) => `<tr><td>${when(f.at)}</td><td><span class="badge ${f.side.toLowerCase()}">${f.side}</span></td><td><b class="mono">${esc(f.symbol)}</b></td><td class="num">${f.quantity.toFixed(4)}</td><td class="num">${usd(f.price)}</td><td class="num ${tone(f.realized_pnl)}">${f.side === "SELL" ? usd(f.realized_pnl) : ""}</td></tr>`).join("")}</tbody></table>` : '<p class="muted small">No fills yet.</p>';
}

let callsLoaded = false;
async function loadCalls() {
  if (callsLoaded || !DATA?.latest?.day) return;
  callsLoaded = true;
  try {
    const res = await fetch(`api/calls/${DATA.latest.day}.json`, { cache: "no-store" });
    if (!res.ok) throw new Error(res.status);
    const calls = await res.json();
    $("calls").innerHTML = calls.map((c) => `<details><summary>${esc(c.agent)} · ${esc(c.subject)} · ${esc(c.model)} · ${c.seconds.toFixed(0)}s ${c.ok ? "" : "· FAILED"}</summary><p class="label" style="margin-top:8px">Asked</p><pre class="transcript">${esc(c.user)}</pre><p class="label">Answered</p><pre class="transcript">${esc(c.response)}</pre></details>`).join("");
  } catch {
    $("calls").innerHTML = '<p class="muted small">No transcripts for this day.</p>';
  }
}

function how(d) {
  $("agents").innerHTML = `<table><thead><tr><th>Agent</th><th>Job</th><th>Model</th></tr></thead><tbody>${d.desk.agents.map((a) => `<tr><td><b class="mono">${esc(a.name)}</b></td><td class="small">${esc(a.job)}</td><td class="small">${esc(a.model)}</td></tr>`).join("")}</tbody></table>`;
  const r = d.rules;
  $("rules").innerHTML = [
    `Always fully invested in at most ${r.max_positions} individual US stocks.`,
    "No ETFs, index funds, mutual funds, ETNs, preferreds, warrants or units.",
    `Only companies worth at least $${(r.min_market_cap / 1e9).toFixed(0)} billion, priced at least $${r.min_price} and trading $${(r.min_dollar_volume / 1e6).toFixed(0)} million a day.`,
    "Every holding gets a KEEP or SELL every day. Every sale is replaced by a purchase.",
    `Orders fill at the next open, with ${(r.fee * 100).toFixed(1)}% fees and ${(r.slippage * 100).toFixed(1)}% slippage each way.`,
    "No stop-losses, no momentum fallback and no rule picks a stock: only the models decide.",
  ].map((t) => `<li>${esc(t)}</li>`).join("");
  const sources = d.latest?.sources || [];
  const fixed = ["Nasdaq stock screener: every US-listed stock in one request", "Nasdaq Trader symbol directory: the list of funds to exclude", "Yahoo Finance: daily bars and per-stock headlines", "Google News and Bing News: sector news searches"];
  $("sources").innerHTML = fixed.map((t) => `<li>${esc(t)}</li>`).join("") + sources.map((s) => `<li>${esc(s.name)}: <span class="${s.ok ? "pos" : "neg"}">${s.ok ? "answered" : "failed"}</span> <span class="muted">${esc(s.detail)}</span></li>`).join("");
}

const secs = (v) => {
  if (v == null || isNaN(v)) return "–";
  if (v >= 3600) return `${Math.floor(v / 3600)}h ${Math.round((v % 3600) / 60)}m`;
  if (v >= 60) return `${Math.floor(v / 60)}m ${Math.round(v % 60)}s`;
  return `${v.toFixed(v < 10 ? 1 : 0)}s`;
};
const num = (v) => (v == null || isNaN(v) ? "–" : Number(v).toLocaleString("en-US", { maximumFractionDigits: 1 }));
const table = (head, rows) => rows.length
  ? `<table><thead><tr>${head.map((h) => `<th class="${h.endsWith(" ") ? "" : "num"}">${h.trim()}</th>`).join("")}</tr></thead><tbody>${rows.map((r) => `<tr>${r.map((c, i) => `<td class="${i === 0 ? "" : "num"}">${c}</td>`).join("")}</tr>`).join("")}</tbody></table>`
  : '<p class="muted small">Nothing recorded for this run yet.</p>';

function ops(d) {
  const r = d.latest;
  const m = r?.metrics;
  if (!m || !m.seconds) {
    $("ops-stages").innerHTML = '<p class="muted small">The next run records its metrics here.</p>';
    return;
  }
  $("ops-day").textContent = `${r.day} · run ${secs(m.seconds)}`;
  const agents = m.agents || [];
  const writeTps = m.model_seconds ? agents.reduce((s, a) => s + a.completion_tokens, 0) / Math.max(1, agents.reduce((s, a) => s + (a.tokens_per_second ? a.completion_tokens / a.tokens_per_second : 0), 0)) : 0;
  const calls = agents.reduce((s, a) => s + a.calls, 0);
  const failed = agents.reduce((s, a) => s + a.failures, 0);
  const cells = [
    ["Run time", secs(m.seconds), `${Math.round((m.model_seconds / m.seconds) * 100)}% waiting on the model`],
    ["Model calls", num(calls), `${failed} failed · ${secs(calls ? m.model_seconds / calls : 0)} each on average`],
    ["Tokens", `${num(m.prompt_tokens / 1000)}k in`, `${num(m.completion_tokens / 1000)}k out · writes ${num(writeTps)} tokens/s`],
    ["Turnover", `${num(m.trading?.turnover_pct)}%`, `${m.trading?.fills ?? 0} fills · fees ${usd(m.trading?.fees)}`],
  ];
  $("ops-stats").innerHTML = cells.map(([l, v, s]) => `<div class="card stat"><span class="label">${l}</span><strong>${v}</strong><small>${s}</small></div>`).join("");
  const stages = m.stages || [];
  const top = Math.max(...stages.map((st) => st.seconds), 1);
  $("ops-stages").innerHTML = '<div class="bars">' + stages.map((st) => {
    const sub = st.name.includes(": ");
    return `<div class="barrow ${sub ? "sub" : ""}"><span>${esc(st.name)}</span><span class="track"><i style="width:${Math.max(0.5, (st.seconds / top) * 100)}%"></i></span><span class="num">${secs(st.seconds)}</span></div>`;
  }).join("") + `</div><p class="small muted" style="margin-top:10px">Indented rows are steps inside the first stage. Total run ${secs(m.seconds)}.</p>`;
  $("ops-agents").innerHTML = table(["Agent ", "Calls", "Failed", "Total", "Average", "Slowest", "Tokens in", "Tokens out", "Read tok/s", "Write tok/s"],
    agents.map((a) => [`<b class="mono">${esc(a.agent)}</b><div class="small muted">${esc(a.model)}</div>`, a.calls, a.failures, secs(a.seconds), secs(a.seconds / Math.max(1, a.calls)), secs(a.max_seconds), num(a.prompt_tokens), num(a.completion_tokens), num(a.prompt_per_second), num(a.tokens_per_second)]));
  $("ops-slowest").innerHTML = table(["Call ", "Time", "In", "Out"],
    (m.slowest || []).map((c) => [`${esc(c.agent)} · ${esc(c.subject)}`, secs(c.seconds), num(c.prompt_tokens), num(c.completion_tokens)]));
  $("ops-model").innerHTML = table(["Event ", "Time"],
    (m.model || []).map((e) => [`<b>${esc(e.kind)}</b> ${esc(e.model)}<div class="small muted">${esc(e.detail)}</div>`, secs(e.seconds)]));
  $("ops-sources").innerHTML = table(["Source ", "Requests", "Failed", "Average", "Slowest", "Total", "Data"],
    (m.sources || []).slice().sort((a, b) => b.total_ms - a.total_ms).map((x) => [esc(x.source), x.requests, x.failures, secs(x.total_ms / Math.max(1, x.requests) / 1000), secs(x.max_ms / 1000), secs(x.total_ms / 1000), `${num(x.bytes / 1e6)} MB`]));
  const mc = m.machine || {};
  $("ops-machine").innerHTML = [
    ["Trigger", mc.trigger],
    ["Run", mc.run_id ? `<a href="https://github.com/mansoor-mamnoon/paper-trader/actions/runs/${esc(mc.run_id)}">${esc(mc.run_id)}</a>` : "local"],
    ["Code", mc.git_sha ? `<a href="https://github.com/mansoor-mamnoon/paper-trader/commit/${esc(mc.git_sha)}">${esc(mc.git_sha)}</a>` : "–"],
    ["Machine", `${esc(mc.os)} · ${mc.cores} cores · ${num((mc.ram_mb || 0) / 1024)} GB`],
    ["CPU", esc(mc.cpu || "–")],
    ["Started", when(m.started_at)],
    ["Ledger size", `${num((m.database_bytes || 0) / 1e6)} MB`],
  ].map(([k, v]) => `<li><b>${k}:</b> ${v}</li>`).join("");
  const t = m.trading || {};
  $("ops-trading").innerHTML = [
    ["Fills at this open", t.fills ?? 0],
    ["Bought", usd(t.bought)],
    ["Sold", usd(t.sold)],
    ["Fees", usd(t.fees)],
    ["Turnover", `${num(t.turnover_pct)}% of the account`],
    ["Orders queued for the next open", t.orders_queued ?? 0],
    ["Funnel", r.funnel ? `${num(r.funnel.listed)} listed → ${num(r.funnel.eligible)} eligible → ${r.funnel.screened} screened → ${r.funnel.candidates} candidates · ${r.funnel.headlines} headlines` : "–"],
  ].map(([k, v]) => `<li><b>${k}:</b> ${v}</li>`).join("");
  $("ops-history").innerHTML = table(["Day ", "Run", "Model", "Calls", "Failed", "Tokens in", "Tokens out", "Turnover", "Fees", "Problems"],
    (d.days || []).map((day) => {
      const x = day.metrics || {};
      const n = (x.agents || []).reduce((s, a) => s + a.calls, 0);
      const f = (x.agents || []).reduce((s, a) => s + a.failures, 0);
      return [esc(day.day), secs(x.seconds || (day.funnel?.minutes || 0) * 60), secs(x.model_seconds), n || day.funnel?.calls || 0, f, num(x.prompt_tokens), num(x.completion_tokens), x.trading ? `${num(x.trading.turnover_pct)}%` : "–", x.trading ? usd(x.trading.fees) : "–", (day.problems || []).length];
    }));
  const problems = (r.problems || []).map((p) => `<li><span class="badge hot">problem</span> ${esc(p)}</li>`);
  const events = (d.events || []).map(([at, kind, msg]) => `<li><span class="badge">${esc(kind)}</span> <span class="muted">${when(at)}</span> ${esc(msg)}</li>`);
  $("ops-events").innerHTML = problems.concat(events).join("") || '<li class="muted">None.</li>';
}

async function main() {
  tabs();
  try {
    const res = await fetch("api/state.json", { cache: "no-store" });
    if (!res.ok) throw new Error("HTTP " + res.status);
    DATA = await res.json();
  } catch (error) {
    $("notice").hidden = false;
    $("notice").textContent = "Could not load the account data: " + error.message;
    return;
  }
  for (const s of DATA.desk.sectors) SECTOR_NAMES[s.id] = s.name;
  for (const c of DATA.latest?.cards || []) CARD_SECTOR[c.symbol] = c.sector;
  stats(DATA); chart(DATA.equity || []); holdings(DATA); pending(DATA); today(DATA);
  newsbook(DATA); desk(DATA); historyView(DATA); ops(DATA); how(DATA);
  if (!$("notice").hidden) return;
  if (DATA.account.last_at) { $("notice").hidden = false; $("notice").textContent = "Last run: " + when(DATA.account.last_at); }
}
main();
