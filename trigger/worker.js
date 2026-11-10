// A Cloudflare Worker cron that starts the stock account run when it is due.
//
// The same rule as scripts/trigger.sh, but in the cloud, so it does not depend on a Mac
// being awake: every 30 minutes, if no run has started or succeeded since the latest
// weekday session settled (16:30 New York time), start one; give up after three failed
// attempts per session. Workers' free plan covers this (a few dozen requests a day).
//
// Deploy: see the README section "Starting the run from outside GitHub".
// Secret: GITHUB_TOKEN, a fine-grained token with "Actions: read and write" on this repo.

const NY = "America/New_York";
const SETTLED = [16, 30];
const MAX_TRIES = 3;

function nyParts(date) {
  const parts = new Intl.DateTimeFormat("en-US", {
    timeZone: NY, year: "numeric", month: "2-digit", day: "2-digit",
    hour: "2-digit", minute: "2-digit", hourCycle: "h23", weekday: "short",
  }).formatToParts(date);
  return Object.fromEntries(parts.map((p) => [p.type, p.value]));
}

// The UTC instant when New York's wall clock reads y-m-d h:mi (EDT is UTC-4, EST is UTC-5).
function nyInstant(y, m, d, h, mi) {
  for (const offset of [4, 5]) {
    const t = new Date(Date.UTC(y, m - 1, d, h + offset, mi));
    const p = nyParts(t);
    if (+p.year === y && +p.month === m && +p.day === d && +p.hour === h && +p.minute === mi) return t;
  }
  return null;
}

// The latest weekday 16:30 ET at or before `now`.
export function lastSettled(now) {
  const today = nyParts(now);
  for (let back = 0; back <= 7; back++) {
    const day = new Date(Date.UTC(+today.year, +today.month - 1, +today.day - back, 12));
    const weekday = day.getUTCDay();
    if (weekday === 0 || weekday === 6) continue;
    const at = nyInstant(day.getUTCFullYear(), day.getUTCMonth() + 1, day.getUTCDate(), ...SETTLED);
    if (at && at <= now) return at;
  }
  return null;
}

// What to do given the runs created since the session settled.
export function decide(runs) {
  const alive = runs.filter((r) => r.status !== "completed" || r.conclusion === "success").length;
  if (alive > 0) return "up_to_date";
  if (runs.length >= MAX_TRIES) return "gave_up";
  return "start";
}

async function github(env, path, init = {}) {
  const response = await fetch(`https://api.github.com/repos/${env.REPO}${path}`, {
    ...init,
    headers: {
      authorization: `Bearer ${env.GITHUB_TOKEN}`,
      accept: "application/vnd.github+json",
      "user-agent": "paper-trader-trigger",
      "x-github-api-version": "2022-11-28",
      ...(init.body ? {"content-type": "application/json"} : {}),
    },
  });
  if (!response.ok) throw new Error(`GitHub ${path}: ${response.status} ${await response.text()}`);
  return response.status === 204 ? null : response.json();
}

export async function check(env, now = new Date()) {
  const settled = lastSettled(now);
  const since = settled.toISOString().replace(/\.\d+Z$/, "Z");
  const listed = await github(env, `/actions/workflows/account.yml/runs?per_page=20&created=%3E%3D${since}`);
  const runs = listed.workflow_runs.filter((r) => r.created_at >= since);
  const action = decide(runs);
  if (action === "start") {
    await github(env, "/actions/workflows/account.yml/dispatches", {method: "POST", body: JSON.stringify({ref: "main"})});
  }
  return {since, runs: runs.length, action};
}

export default {
  async scheduled(_event, env, ctx) {
    ctx.waitUntil(check(env).then((r) => console.log(JSON.stringify(r))));
  },
  // GET /  shows the repository and the session the next check covers; it never starts a run.
  async fetch(_request, env) {
    const settled = lastSettled(new Date());
    return new Response(JSON.stringify({repo: env.REPO, last_settled: settled}), {headers: {"content-type": "application/json"}});
  },
};
