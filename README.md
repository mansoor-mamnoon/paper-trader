<div align="center">

<img src="docs/logo.svg" alt="Paper Trader logo" width="84">

<img src="docs/banner-v2.svg" alt="Paper Trader, the $1,000 experiment" width="100%">

**A $1,000 paper-trading account run by an investment committee of nine open-weights models from nine different companies. They research with tools, argue with each other, remember their mistakes and grade every call, on free GitHub runners with no API key.**

[![Live dashboard](https://img.shields.io/badge/live-dashboard-ffe14d?style=for-the-badge&labelColor=0f1115)](https://mansoor-mamnoon.github.io/paper-trader/)
[![Tests](https://img.shields.io/github/actions/workflow/status/mansoor-mamnoon/paper-trader/rust.yml?branch=main&style=for-the-badge&label=tests&labelColor=0f1115)](https://github.com/mansoor-mamnoon/paper-trader/actions/workflows/rust.yml)
![Rust](https://img.shields.io/badge/rust-2024-ffe14d?style=for-the-badge&logo=rust&logoColor=white&labelColor=0f1115)
![Models](https://img.shields.io/badge/models-9%20companies-ffe14d?style=for-the-badge&labelColor=0f1115)
![Cost](https://img.shields.io/badge/cost-%240-ffe14d?style=for-the-badge&labelColor=0f1115)

[Live dashboard](https://mansoor-mamnoon.github.io/paper-trader/) · [The desk](#the-desk) · [Independent models](#independent-models) · [Deterministic runs](#deterministic-runs) · [Logs and metrics](#logs-and-metrics) · [Free and reliable](#free-and-reliable) · [Run it](#run-it) · [Limits](#limits)

</div>

---

<div align="center">
<img src="docs/overview.svg" alt="Overview: data sources feed the screen and the investment desk, the live and shadow accounts trade, ledgers are committed, and a static dashboard publishes the results" width="100%">
</div>

## What it is

Paper Trader starts with $1,000 of simulated money and looks at US stocks once a day. A quantitative screen finds stocks in an uptrend. Then a desk of small open-weights language models decides which of them to buy. Every signal, every skipped stock, every argument, every model call and every fill goes into SQLite, and a read-only dashboard on GitHub Pages shows all of it.

The question is whether this beats buying SPY after fees and slippage over 60 days, and which parts of the AI deserve the credit if it does. It places no real orders and has no broker connection. It runs on GitHub's free runners with no API key, and the backend is Rust.

<div align="center">
<img src="docs/dashboard.gif" alt="A tour through the dashboard tabs: overview, the desk, strategy lab, ops, journal and how it works" width="760">
<br><sub>A real run from 8 November 2026. No trades have closed yet, so the portfolio tabs are still empty.</sub>
</div>

| Setting | Value |
|---|---|
| Starting capital | $1,000, long only, fractional shares, fully invested across five slots |
| Universe | US exchange-listed symbols from the Nasdaq directory, ETFs included, that pass the hard gates ($5, $5M a day, no event within 7 days) |
| Schedule | Each weekday session once it settles at 16:30 ET, started by an outside trigger every 30 minutes, with three GitHub cron backups and a watchdog |
| Costs | 0.10% fee per side and 0.20% slippage against each fill |
| Benchmark | SPY, price only |
| Models | Nine open-weights models, one per decision role, 4-bit, on llama.cpp |
| Hosting | GitHub Actions and GitHub Pages |

## The desk

One prompt that asks a small model to rank a dozen headlines wastes most of what a language model can do. GitHub's free runners give the job hours of CPU, so the desk spends that on deliberation.

<div align="center">
<img src="docs/desk.svg" alt="The desk: screen, triage, research agents with tools, bull and bear debate, judge, then the position doctor, memory, scorecard and strategy lab feed back in" width="100%">
</div>

1. **Screen.** About 13,000 tickers go in. The hard gates remove anything under $5, under $5M a day of trading, or within a week of earnings, an FDA decision or a restructuring, and the stocks left in an uptrend come out.
2. **Triage.** A model scores the survivors from 0 to 10 for catalyst quality, eight at a time, and the best few become finalists.
3. **Research agents.** Each finalist gets its own research loop. The agent picks its next tool: read the start of an SEC filing, pull older headlines, or inspect the last 15 sessions. It stops when it knows enough or after four steps, and it cannot repeat a call.
4. **Bull and bear.** Two arguments per finalist, one told to make the case for buying and one told to find the flaw.
5. **The judge.** The portfolio manager writes its reasoning first, then picks stocks for the open slots and calls the market regime from breadth and trend. A formula sizes each slot (volatility and the industry cap), not the judge. A risk-off call allows one new position at half size. Choosing nothing is a valid answer.
6. **Position doctor.** Every day it re-reads each holding against the news since entry and answers hold, exit, or tighten the stop. It can raise a stop and never lower one.
7. **Memory.** When a trade closes, a post-mortem writes a one-sentence lesson. The latest lessons go into the judge's prompt.
8. **Scorecard.** Every pick, veto and near-miss is scored against SPY after 1, 3, 5 and 10 trading days. That gives hundreds of samples where three trades give none, plus a calibration curve of conviction against outcome. The judge reads its own track record.

Replies are constrained to JSON schemas with length limits, so a small model cannot return malformed or runaway answers. Text from news and filings is treated as untrusted data. The hard gates, position size, the five-slot cap, the industry cap, the circuit breaker, costs, the 60-day end and the direction of stop changes are written in code, and nothing a model says can change them. Every stage has a wall-clock budget and falls back to the stage before it when it fails or runs out of time.

<div align="center">
<img src="docs/desk.png" alt="The Desk tab: market regime, the funnel from 219 scanned stocks to the picks, and a research trace with a bull and bear case" width="760">
<br><sub>The Desk tab from a real run: a risk-off call, the funnel, and one finalist's research trace.</sub>
</div>

## Independent models

One model making every decision would bring its habits and blind spots to every stage. So each decision role runs on a model from a different company, and a unit test fails if two roles ever share a company.

| Role | Model | Company | License | Size |
|---|---|---|---|---|
| Triage | Granite 4.0 H Micro | IBM | Apache-2.0 | 1.9 GB |
| Research agents | Gemma 4 E4B | Google | Apache-2.0 | 5.0 GB |
| Bull case | Llama 3.2 3B Instruct | Meta | Llama 3.2 Community | 2.0 GB |
| Bear case | Phi-4 mini Instruct | Microsoft | MIT | 2.5 GB |
| Judge | Qwen3 4B Instruct 2507 | Alibaba | Apache-2.0 | 2.5 GB |
| Position doctor | Ministral 3 3B Instruct | Mistral AI | Apache-2.0 | 2.1 GB |
| Post-mortems | SmolLM3 3B | Hugging Face | Apache-2.0 | 1.9 GB |
| Strategy proposals | Nemotron 3 Nano 4B | NVIDIA | NVIDIA Open Model | 2.8 GB |
| Single-shot ablation | LFM2.5 2.6B | Liquid AI | LFM Open | 1.7 GB |

The runner has 16 GB of RAM and 14 GB of disk, which fits one model at a time. A pool in Rust starts one `llama-server`, and when a role needs a different model it downloads that file, stops the old server and starts the next. The desk runs its work in blocks, one model per block, so a full day needs about a dozen swaps.

## Deterministic runs

The same prompt on the same weights should give the same answer, and a reader should be able to check that it did.

- **Greedy decoding.** Temperature 0, `top_k` 1, a fixed seed and no prompt cache on every request.
- **Pinned weights.** Each model file is fetched from an exact Hugging Face commit and its SHA-256 is checked as it downloads. A mismatch deletes the file and fails the load.
- **Pinned runtime.** llama.cpp build `b12211`, with four CPU threads fixed per machine.
- **A canary on every load.** Each model answers the same small prompt twice. If the two answers differ, the run records `UNSTABLE`. The answer hash is stored, so a change in the same model's hash from one day to the next shows up on the Ops tab.
- **Every call is hashed.** The prompt and the output each get a 64-bit hash in the call log, so a call can be replayed and compared.

GitHub's runners are not all the same machine. The CPU model is recorded with every run, so a hash that moves when the CPU changes is explained rather than mysterious.

## Does the AI help?

Most AI trading demos never test that. Here the live account sits at the top of a depth ladder, and a shadow account trades the same data one rung down at every level:

| Depth | What runs |
|---|---|
| 0 | No AI. The same screen ranked by 20-day gain. |
| 1 | One prompt ranks the headlines. |
| 2 | The committee: triage, bull and bear, judge. |
| 3 | The full desk, with research agents and filings. |

The dashboard compares them by portfolio return and by scorecard. If a lower rung wins by a clear margin over two weeks, the strategy lab promotes it, so the system switches off AI that does not pay for itself. The default screen is a plain trend filter with no proven edge, so a good result at depth 0 is also a finding.

## Strategy

Two layers decide what is bought. Hard gates come first and are fixed in code (`src/gates.rs`): no model can loosen them, and a strategy from the lab can only make them stricter. The strategy's own screen comes second and the lab may tune it within bounds.

### Hard gates

| # | Gate | Rule |
|---|---|---|
| 1 | Price | Last close of at least $5. |
| | Liquidity | Average daily traded value (close × volume) of at least $5M over 30 sessions. |
| | Events | No buy within 7 calendar days either side of an earnings report (the Nasdaq calendar, looking a week back and two weeks ahead), an FDA decision or a restructuring. FDA and restructuring events are read from SEC filings (8-K items 1.03 bankruptcy, 2.05 exit costs, 3.01 delisting, 3.03 and 5.03 the usual route of a reverse split; forms S-4, DEFM14A, PREM14A, 425, SC TO-T, SC 13E3, 15-12B, 15-12G, 25-NSE) and from headline phrases (FDA, PDUFA, advisory committee, complete response letter, reverse split, share consolidation, bankruptcy, chapter 11, restructuring, delisting, going private, merger, to be acquired). An undated headline counts, so the gate errs toward blocking. |
| 2 | Volatility sizing | A slot is 20% of equity up to a 14-day daily volatility (σ of close-to-close returns) of 8%. Above that it shrinks to 20% × 8% / σ, so every volatile position carries the same daily dollar risk. The bars have no intraday high and low, so σ stands in for ATR. |
| | Caps | 20% of equity per position, five positions, long only, no leverage, fractional shares. |
| | Industry | At most 30% of equity in one SEC industry (the SIC code from EDGAR), counting held positions at their marks and queued buys at their size. A buy is trimmed to fit, or recorded as `INDUSTRY_CAP` when there is no room. A stock with no SIC code (funds, some foreign issuers) counts as its own industry. |
| 3 | Fills | A buy signalled after a session settles (16:30 ET) fills at the next session's open, never at the signal's close. Effective price = open × (1 ± 0.20% slippage), plus a 0.10% fee per side. If a run is missed, the order still fills at the first open after its signal. |
| | Re-check | A queued buy is checked against the price, liquidity and event gates again before it fills, on sessions before the fill's open, and is `CANCELLED` with the reason if it fails. A buy whose open has not printed yet is checked on the latest bars every run, so a failing one frees its slot at once instead of holding it until the open. |
| 4 | Exits | A stop at −4% and a target at +8% from the entry fill, checked on daily closes; a crossing queues a sell for the next open, and a gap through the stop is taken at that open. The position doctor may raise a stop toward the price and never lower it. |
| 5 | Circuit breaker | When equity is at least 10% below its high over the last five sessions, every queued buy is cancelled and nothing new opens for 48 hours (`CASH_LOCK`). Held positions keep following their stops and targets. |

**First runs with the gates (10 November 2026).** Of the four buys queued on 6 November under the old $1M floor, AEYE ($2.9M a day), APLZ ($2.7M) and GIBO ($3.0M) were cancelled and only AAOZ filled, at $32.34 on 9 November. AAOZ closed at $29.88 that day, below its $31.05 stop, so its sell is queued for the next open. The screen removed 457 stocks before any model saw them: 380 for thin trading, 86 for an earnings report within a week and a few under $5. The desk then had three open slots. The judge called the market risk-off, which allows one new entry at half a slot, and picked REAL (The RealReal, σ 4.9%, so no volatility cut) for about $100. REPL (Replimune), picked by the earlier run, also waits for the open at half a slot. No industry cap or cash lock fired.

### The strategy's screen

The defaults, which the lab may change within fixed bounds:

- close above the 20-day average, with the 20-day average above the 50-day average
- 20-day return above 2%
- price and liquidity floors of its own, which only apply when they are stricter than the hard gates
- no earnings report inside the strategy's own blackout window (off by default; the hard gate's 7 days always apply)

Up to 25 survivors reach triage. The judge picks which stocks; it never sizes them, and a risk-off call halves a slot and allows one new entry. A session counts as completed from 16:30 New York time, so each evening run sees that day's bar and the following evening's run records the fill. Apart from the circuit breaker there is no loss halt: the drawdown is reported on the dashboard, and the experiment ends after 60 days.

### Strategy lab

The loop follows the recursive self-improvement pattern of the [KISS `rsi7d` agent](https://github.com/ksenxx/kiss_ai/tree/main/src/kiss/agents/seas/rsi7d): mine recent runs for evidence, propose a bounded edit, test it, and keep it only if the evidence supports it. Here the edit target is the strategy and the test is a live shadow account.

1. Once a week the system reviews how the live strategy traded and how its picks scored.
2. The proposer model suggests one small change: numeric parameters inside fixed bounds, or the text of the judge's playbook. Anything out of bounds is rejected and logged.
3. The change trades a separate $1,000 shadow account on the same daily data.
4. After 14 days a challenger replaces the live strategy only if it beats it by at least one percentage point with at least two trades and a drawdown no more than three points worse.
5. The replaced strategy keeps trading as a shadow. If the promotion was luck, it wins the seat back under the same rule.

The registry, the lessons, the telemetry and every shadow ledger are committed to the `account` branch, so each change and its evidence stay public.

## Logs and metrics

Every run writes to one SQLite file, `telemetry.sqlite`, and the dashboard's **Ops** tab reads it. It answers three questions at three levels.

| Level | What it shows |
|---|---|
| Run | Status (ok, degraded, failed), minutes used against the budget, trigger, CPU, cores and RAM, and a history of the last 30 runs. |
| Stage | A waterfall of where the time went (scan, intel, filings, each desk block with triage, research, bull and bear inside the prepare block, each strategy's cycle, post-mortems, the lab), and a bottleneck table of mean, p95 and worst time per stage over 30 runs. |
| Call | Per model and per role: calls, success rate, latency, tokens per second for prompt and generation, errors, and the determinism check. Data sources get the same treatment: requests, failure rate and latency for Nasdaq, Yahoo, SEC EDGAR, the earnings calendar and Hugging Face. |

While a run works, its Actions step log shows live progress: each stage, model download (in tenths), model load and model call as it happens, the finalists and each research step, the judge's picks, and a heartbeat every minute that names what is still open and for how long. Lines look like this:

```
[04:26:10Z +7m33s] start desk.research
[04:27:14Z +8m37s] research call on gemma-4-e4b took 31.0 s, 214 tokens at 12.2/s
[04:27:14Z +8m37s] research GIBO step 1: read_filing(0) - Strong momentum and AI news; check the latest 6-K
[04:28:10Z +9m33s] still working: desk.prepare (9m32s) > desk.research (2m00s) > research call on gemma-4-e4b (25s)
```

A heartbeat that keeps counting on one model call means a slow model; a log with no heartbeat at all means the process stopped.

Raw tables download as `runs.csv`, `stages.csv`, `llm_calls.csv`, `events.csv` and `fetches.csv`. Each Actions run also writes the same report to its job summary, so a run can be judged from the Actions page without opening the site. Prompts and outputs are stored as hashes plus the first 300 characters of any error, which keeps the file small enough to commit daily.

## Free and reliable

The question was whether a free GitHub account can run this every day. For a public repository it can.

| Question | Answer |
|---|---|
| Cost | Standard GitHub-hosted runners are free and unlimited in public repositories. Private repositories on the free plan get 2,000 minutes a month on smaller machines. |
| Machine | `ubuntu-latest` in a public repository is 4 vCPU, 16 GB RAM and 14 GB of disk. A private repository gets 2 vCPU and 8 GB. |
| Job length | Up to 6 hours. A full desk run is well inside that. |
| Schedule precision | Cron runs can start late when GitHub is busy, and a run can be dropped without an error. |
| Inactivity | GitHub disables scheduled workflows in a public repository after 60 days without activity. |

The two weak points are the last two, so the repository handles both. On 9 November GitHub dropped both of the evening's scheduled runs without a trace and started the third 54 minutes late, so the schedule is now a backup and not the main trigger. Starting a run through GitHub's API (`workflow_dispatch`) is immediate, so an outside clock does that instead (next section). The account job is still scheduled three times a day, and it skips itself when the account already ran after the latest session settled at 16:30 ET, so every extra trigger costs nothing. Counting from the session and not from a fixed number of hours means a run that starts late one night cannot make the next evening's run skip. A separate watchdog checks every weekday morning that a run happened after the previous US session and starts one if it did not. Every run and the watchdog re-enable the schedules, which resets the inactivity clock. A webhook (`repository_dispatch` with event type `run-account`) can also start a run from outside.

### Starting the run from outside GitHub

Two free triggers apply the same rule every 30 minutes: if no run has started, or finished successfully, since the latest weekday session settled at 16:30 ET, start one. Each gives up after three failed runs for one session, so a broken day cannot loop all night. Use either or both; a duplicate only finds the account current and stops.

| Trigger | Runs on | Setup |
|---|---|---|
| `scripts/trigger.sh` with launchd | Your Mac. launchd runs a missed interval when the Mac wakes, so a laptop asleep at the close still starts the run when it opens. | Log in with `gh auth login`, then `scripts/install-trigger.sh`. Log: `~/Library/Logs/paper-trader-trigger.log`. Remove with `scripts/install-trigger.sh uninstall`. |
| `trigger/worker.js` on Cloudflare Workers | Cloudflare's cron, free plan. Does not depend on any computer being on. | Create a fine-grained GitHub token with *Actions: read and write* on this repository. In `trigger/`, run `npx wrangler deploy`, then `npx wrangler secret put GITHUB_TOKEN`. Logs: `npx wrangler tail`. |

The rule is tested in `trigger/worker.test.mjs` (run it with `node`).

I also looked at other free routes: an always-free cloud VM, free Hugging Face Spaces, Kaggle scheduled notebooks, and free hosted inference. Each needs an account, a card or a token set up by hand, sleeps when idle, or is hosted rather than local. None beats a runner that already sits next to the repository, so everything stays on Actions. Sources: GitHub's pages on [runner hardware](https://docs.github.com/en/actions/reference/runners/github-hosted-runners) and on [disabling and enabling workflows](https://docs.github.com/en/actions/how-tos/manage-workflow-runs/disable-and-enable-workflows).

On a real runner the full desk, with all nine models, nine swaps and four strategies, took 28 minutes with nothing skipped, against 7 minutes on an Apple M2 Pro. The 9 November run took 58 minutes, 49 of them preparing the finalists, and 40 of those were Gemma's research calls. Gemma 4 thinks before it answers, and the server strips that text, so it only costs time: about 80% of its research tokens were thinking nobody reads (0.78 visible characters per token, against about 4 for every other model). Its thinking is now capped at 128 tokens (`pool::reasoning_budget`). On test prompts that made the same research choices as unlimited thinking at about half the tokens, and the answers stayed deterministic. On the runner it did not save time: the 10 November run still spent 39 minutes researching 8 finalists. Each call now writes only about 210 tokens, but at about 8 tokens a second a call takes 30 to 100 seconds, so the time goes to reading long filing prompts on a CPU, not to thinking. Shorter filing excerpts or fewer finalists are the next levers. Turning thinking off was four times faster but repeated tools and ended research early, so it is not used. The Ops tab now times triage, research, bull and bear separately inside the prepare stage, so the next bottleneck shows by name. The runner had 88 GB of disk free, so pruning each model file after use is a safety margin and not a need. If the model server fails to start, the account still trades by momentum ranking and the dashboard says so. The `Model smoke test` workflow runs the whole desk on a scratch account to check timing, disk and determinism on a real runner without touching the live ledger.

## Dashboard

<div align="center">
<img src="docs/ops.png" alt="The Ops tab: run status, a waterfall of stage durations, the bottleneck table and per-model latency" width="760">
<br><sub>The Ops tab from a real run on a Mac: stage waterfall, models and data sources.</sub>
</div>

Eight tabs. Overview opens with a one-line verdict against SPY and a 60-day progress bar. The Desk shows today's committee transcript and which model decided each part: regime, funnel, each finalist's research trace and debate, the judge's reasoning, the doctor's notes and the lessons in memory. Holdings shows each stock's price between its stop and its target. Trades lists every fill. Journal explains each decision in plain English with a ticker search. Strategy lab shows the live strategy, the challengers, the depth ladder and the calibration chart. Ops covers performance and bottlenecks. How it works covers the rules and the limits.

Full histories download as `decisions.csv`, `fills.csv`, `equity.csv` and `events.csv`, and `api/state` returns the raw snapshot.

## Crypto account

A second $1,000 account trades BTC, ETH, SOL and ADA from public Kraken data, in its own database.

- Signal: the 20-hour average of closed hourly candles above the 50-hour average, with 24-hour momentum above 1%
- Orders queue on one observation and fill on a later fresh one, at the ask plus 0.10% slippage and a 0.40% fee
- Sizing: at most 25% of equity per position, about 1% of equity at risk per trade and 3% in total
- Exits: -4% stop, +8% target, a seven-day holding limit, or a trend break

```bash
paper-trader crypto once   --db crypto.sqlite    # observe Kraken and apply one cycle
paper-trader crypto replay --db replay.sqlite --input snapshots.json
paper-trader crypto status --db crypto.sqlite
```

## Run it

You need a stable Rust toolchain, 1.88 or newer, and about 25 GB of disk if you keep all nine models.

```bash
git clone https://github.com/mansoor-mamnoon/paper-trader.git
cd paper-trader

# The Rust binary downloads, verifies and swaps the pinned models itself.
export PAPER_TRADER_LLAMA_SERVER=$(scripts/llm.sh install)
export PAPER_TRADER_MODELS_DIR=$HOME/.cache/paper-trader-llm/models
cargo run --release -- stocks --db account.sqlite --output site

cp web/* site/        # the dashboard; serve site/ with any static file server
```

To check every model on a new machine, run `cargo run --release --example pool_check`. It loads each role's model, asks it a small question under a JSON schema, and prints load time, speed and the determinism check.

Without `PAPER_TRADER_LLAMA_SERVER` the account trades by momentum ranking. A single OpenAI-compatible server (Ollama, LM Studio, vLLM) works through `PAPER_TRADER_LLM_URL`, and a paid model through `ANTHROPIC_API_KEY`. Both use one model for every role.

| Setting | Meaning |
|---|---|
| `--batch-size N` | Symbols per run. 20000 covers the full directory with many requests. |
| `--replay FILE` | Apply a saved observation to a separate account. |
| `--strategies DIR` | Where the registry, shadow ledgers and telemetry live. Defaults to next to `--db`. |
| `PAPER_TRADER_DESK_MINUTES` | Wall-clock budget for all model calls in a run. Default 120. |
| `PAPER_TRADER_PRUNE_MODELS` | Delete each model file after the pool moves on, to bound disk use. |
| `PAPER_TRADER_SKIP_IF_CURRENT` | `1`: do nothing if the account already ran after the latest session settled (16:30 ET on the last weekday). |
| `PAPER_TRADER_SEC_CONTACT` | An email address for SEC requests. SEC filings stay off without one. |

To host your own copy, fork the repo and create an `account` branch containing an initialised `account.sqlite` and an `INITIALIZED` file. Enable GitHub Pages with the Actions source and the workflows take over.

```
src/
  main.rs       command line, run metadata, skip guard
  daily.rs      one daily run: blocks of model calls, shadows, memory, the lab
  desk.rs       triage, research agents, debate, judge, doctor, post-mortems
  models.rs     the nine pinned models and which role each plays
  pool.rs       downloads, verifies, swaps and canary-checks model servers
  telemetry.rs  runs, stages, model calls, data fetches and events
  progress.rs   live progress lines and the once-a-minute heartbeat
  scorecard.rs  forward returns of every pick, veto and near-miss
  gates.rs      the hard gates: floors, events, volatility sizing, industry cap, circuit breaker
  account.rs    screen, queued orders, sizing, fills, risk limits
  analyst.rs    model clients and the single-shot analyst
  edgar.rs      SEC filings and filing text
  intel.rs      earnings calendar and headlines
  strategy.rs   the bounded, editable strategy
  lab.rs        shadow accounts, promotion rule, proposals
  data.rs       Nasdaq directory and Yahoo daily bars
  crypto.rs     crypto account and Kraken data
  ledger.rs     SQLite schema, state and reports
  export.rs     JSON and CSV snapshot
web/            the dashboard (plain HTML, CSS and JavaScript)
scripts/        llm.sh installs llama.cpp; trigger.sh and install-trigger.sh start runs from a Mac
trigger/        a Cloudflare Worker cron that starts runs from the cloud
examples/       pool_check and edgar_live diagnostics
docs/           README images
.github/        scheduled run, watchdog, model smoke test and CI
```

The crate is a library plus a thin binary on edition 2024 with 109 unit tests, and it passes `cargo clippy -- -D warnings` with the pedantic group enabled. Backend code is Rust only, apart from the small shell and Worker scripts that start runs.

## Limits

- Sixty days is too short to separate skill from luck, so a good or bad result says little about the rules. The scorecard exists to get more samples, not to remove this limit.
- Models of 2 to 5 billion parameters are weak readers of financial news. The depth ladder measures whether they add anything.
- Determinism holds on one machine. Across runners with different CPUs, floating-point differences can change a token, which the CPU field and the canary hash are there to expose.
- SEC filings are off until you set `PAPER_TRADER_SEC_CONTACT` (a repository variable named `SEC_CONTACT` in Actions) to a real email address. The SEC's servers refuse requests that do not identify their owner, and the project does not invent a contact for you. Without it the desk works from headlines and says so in the transcript. Without it the event gate also loses its filing checks (it still reads earnings dates and headlines), and the industry cap cannot see industries, so every stock counts as its own.
- Prices come from a free Yahoo endpoint with no guarantees. Missing quotes are logged and the symbol is skipped.
- Stops and targets are checked on daily closes, so a gap can lose more than the stop level.
- The event gate only knows what the earnings calendar, EDGAR and the headline feed report. An FDA date that appears in none of them, or a restructuring announced in words the phrase list does not contain, gets through.
- Execution is simplified. Liquidity, queue priority, outages and market impact are not modeled.
- GitHub schedules can run late or fail. The outside trigger, the backups and the watchdog make a missed day unlikely, and the last good snapshot stays online until the next run succeeds. The Mac trigger needs the Mac to wake at some point that evening; the Cloudflare one needs nothing.

This is a simulation and not financial advice.

If you find it interesting, a star helps other people find it. Issues and critiques of the strategy are welcome.
