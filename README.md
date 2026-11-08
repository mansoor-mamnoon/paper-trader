<div align="center">

<img src="docs/logo.svg" alt="Paper Trader logo" width="84">

<img src="docs/banner.svg" alt="Paper Trader, the $1,000 experiment" width="100%">

**A $1,000 paper-trading account run by an AI investment committee. It researches with tools, argues with itself, remembers its mistakes, grades every call, and has to prove each layer of AI beats the layer below. All on a free open-weights model.**

[![Live dashboard](https://img.shields.io/badge/live-dashboard-ffe14d?style=for-the-badge&labelColor=0f1115)](https://mansoor-mamnoon.github.io/paper-trader/)
[![Tests](https://img.shields.io/github/actions/workflow/status/mansoor-mamnoon/paper-trader/rust.yml?branch=main&style=for-the-badge&label=tests&labelColor=0f1115)](https://github.com/mansoor-mamnoon/paper-trader/actions/workflows/rust.yml)
![Rust](https://img.shields.io/badge/rust-2024-ffe14d?style=for-the-badge&logo=rust&logoColor=white&labelColor=0f1115)
![Model](https://img.shields.io/badge/model-Qwen3%204B%20open%20weights-ffe14d?style=for-the-badge&labelColor=0f1115)
![Cost](https://img.shields.io/badge/cost-%240-ffe14d?style=for-the-badge&labelColor=0f1115)

[Live dashboard](https://mansoor-mamnoon.github.io/paper-trader/) · [The desk](#the-desk) · [Does the AI help?](#does-the-ai-help) · [Strategy lab](#strategy-lab) · [Free and autonomous](#free-and-autonomous) · [Run it](#run-it) · [Limits](#limits)

</div>

---

<div align="center">
<img src="docs/overview.svg" alt="Overview: data sources feed the screen and the investment desk, the live and shadow accounts trade, ledgers are committed, and a static dashboard publishes the results" width="100%">
</div>

## What it is

Paper Trader starts with $1,000 of simulated money and looks at US stocks once a day. A quantitative screen finds stocks in an uptrend. Then an investment desk of small open-weights models (Qwen3 4B, Apache-2.0) decides which of them to buy. Every signal, every skipped stock, every argument and every fill goes into a SQLite ledger, and a read-only dashboard on GitHub Pages shows all of it.

The question is whether this beats buying SPY after fees and slippage over 60 days, and which parts of the AI deserve the credit if it does. It places no real orders and has no broker connection. Everything runs on GitHub's free runners with no API key, and the backend is Rust.

<div align="center">
<img src="docs/dashboard.gif" alt="A tour through the dashboard tabs: overview, the desk, strategy lab, journal and how it works" width="760">
<br><sub>A real run from 8 November 2026. No trades have closed yet, so the portfolio tabs are still empty.</sub>
</div>

| Setting | Value |
|---|---|
| Starting capital | $1,000, long only, whole shares |
| Universe | US exchange-listed symbols from the Nasdaq directory, ETFs included |
| Schedule | Weekdays at 22:37 UTC, after the US close, or on a webhook |
| Costs | 0.10% fee per side and 0.20% slippage against each fill |
| Benchmark | SPY, price only |
| Model | Qwen3-4B-Instruct-2507, 4-bit, on llama.cpp, CPU only |
| Hosting | GitHub Actions and GitHub Pages |

## The desk

One prompt that asks a small model to rank a dozen headlines wastes most of what a language model can do. GitHub's free runners give the job hours of CPU, so the desk spends that on deliberation.

<div align="center">
<img src="docs/desk.svg" alt="The desk: screen, triage, research agents with tools, bull and bear debate, judge, then the position doctor, memory, scorecard and strategy lab feed back in" width="100%">
</div>

1. **Screen.** About 13,000 tickers go in, and the stocks in a liquid uptrend come out.
2. **Triage.** The model scores the survivors from 0 to 10 for catalyst quality, eight at a time, and the best few become finalists.
3. **Research agents.** Each finalist gets its own research loop. The agent picks its next tool: read the start of an SEC filing, pull older headlines, or inspect the last 15 sessions. It stops when it knows enough or after four steps, and it cannot repeat a call.
4. **Bull and bear.** Two arguments per finalist, one told to make the case for buying and one told to find the flaw.
5. **The judge.** The portfolio manager writes its reasoning first, then picks, sizes each position from 25% to 100% of the cap, and calls the market regime from breadth and trend. In a risk-off call the account allows one new position. Choosing nothing is a valid answer.
6. **Position doctor.** Every day it re-reads each holding against the news since entry and answers hold, exit, or tighten the stop. It can raise a stop and never lower one.
7. **Memory.** When a trade closes, a post-mortem writes a one-sentence lesson. The latest lessons go into the judge's prompt.
8. **Scorecard.** Every pick, veto and near-miss is scored against SPY after 1, 3, 5 and 10 trading days. That gives hundreds of samples where three trades give none, plus a calibration curve of conviction against outcome. The judge reads its own track record.

Replies are constrained to JSON schemas with length limits, so a 4B model cannot return malformed or runaway answers. Text from news and filings is treated as untrusted data. Position size, the three-position cap, costs, the 10% loss halt and the direction of stop changes are written in code, and nothing the model says can change them. Every stage has a wall-clock budget and falls back to the stage before it when it fails or runs out of time.

<div align="center">
<img src="docs/desk.png" alt="The Desk tab: market regime, the funnel from 219 scanned stocks to the picks, and a research trace with a bull and bear case" width="760">
<br><sub>The Desk tab from a real run: a risk-off call, the funnel, and one finalist's research trace.</sub>
</div>

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

The screen is the starting point and the strategy lab can change its values within fixed bounds. The defaults:

- price of at least $5
- 20-day average dollar volume of at least $1M
- close above the 20-day average, with the 20-day average above the 50-day average
- 20-day return above 2%
- no earnings report inside the blackout window (off by default)

Up to 25 survivors reach triage. Three positions at most, each capped at 20% of equity. A close at or below the stop (-4% by default) or at or above the target (+8%) queues a sell for the next session. A buy never fills at the price that triggered it: it queues and fills at a later open with fees and slippage. A 10% drawdown permanently stops new entries, and the experiment ends after 60 days.

## Strategy lab

The loop follows the recursive self-improvement pattern of the [KISS `rsi7d` agent](https://github.com/ksenxx/kiss_ai/tree/main/src/kiss/agents/seas/rsi7d): mine recent runs for evidence, propose a bounded edit, test it, and keep it only if the evidence supports it. Here the edit target is the strategy and the test is a live shadow account.

1. Once a week the system reviews how the live strategy traded and how its picks scored.
2. The model proposes one small change. It can adjust at most two things: numeric parameters inside fixed bounds, or the text of the judge's playbook. Anything out of bounds is rejected and logged.
3. The change trades a separate $1,000 shadow account on the same daily data.
4. After 14 days a challenger replaces the live strategy only if it beats it by at least one percentage point with at least two trades and a drawdown no more than three points worse.
5. The replaced strategy keeps trading as a shadow. If the promotion was luck, it wins the seat back under the same rule.

The registry, the lessons and every shadow ledger are committed to the `account` branch, so each change and its evidence stay public.

## Free and autonomous

There is no API key and no account to set up. Each scheduled run does this on a free GitHub runner:

1. Restores the cached model. The first run downloads about 2.5 GB from Hugging Face and later runs reuse it.
2. Starts `llama-server` from [llama.cpp](https://github.com/ggml-org/llama.cpp) with [Qwen3-4B-Instruct-2507](https://huggingface.co/Qwen/Qwen3-4B-Instruct-2507) quantized to 4 bits.
3. Runs the day: scan, gather news, earnings dates and SEC filings, run the desk for the live account and each shadow, post-mortems, weekly proposal, export.
4. Commits the ledgers and redeploys the dashboard.

On an Apple M2 the full desk takes about four minutes. A CPU-only runner is several times slower, so the run has a three-hour model budget and the job a four-and-a-half-hour limit. Triggers are the weekday schedule, a manual run, or an external webhook (`repository_dispatch` with event type `run-account`). GitHub pauses scheduled workflows in quiet repositories, so every run also re-enables its own schedule. If the model server fails to start, the account still trades by momentum ranking and the dashboard says so. The `Model smoke test` workflow exercises the whole desk on a scratch account without touching the live ledger.

SEC EDGAR answers only some networks. When it is unreachable the desk works from headlines and says so in the transcript.

## Dashboard

Seven tabs. Overview opens with a one-line verdict against SPY and a 60-day progress bar. The Desk shows today's committee transcript: regime, funnel, each finalist's research trace and debate, the judge's reasoning, the doctor's notes and the lessons in memory. Holdings shows each stock's price between its stop and its target. Trades lists every fill. Journal explains each decision in plain English with a ticker search. Strategy lab shows the live strategy, the challengers, the depth ladder and the calibration chart. How it works covers the rules and the limits.

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

You need a stable Rust toolchain, 1.88 or newer.

```bash
git clone https://github.com/mansoor-mamnoon/paper-trader.git
cd paper-trader

scripts/llm.sh        # downloads llama.cpp and the model once, then starts the server on port 8080
PAPER_TRADER_LLM_URL=http://127.0.0.1:8080 \
  cargo run --release -- stocks --db account.sqlite --output site
cp web/* site/        # the dashboard; serve site/ with any static file server
scripts/llm.sh stop
```

Without `PAPER_TRADER_LLM_URL` the account trades by momentum ranking. Any server that speaks the OpenAI chat protocol works (Ollama, LM Studio, vLLM). Set `PAPER_TRADER_LLM_URL`, and optionally `PAPER_TRADER_LLM_MODEL` and `PAPER_TRADER_LLM_KEY`. A paid model is also supported through `ANTHROPIC_API_KEY`, and the local server wins when both are set.

| Setting | Meaning |
|---|---|
| `--batch-size N` | Symbols per run. 20000 covers the full directory with many requests. |
| `--replay FILE` | Apply a saved observation to a separate account. |
| `--strategies DIR` | Where the strategy registry and shadow ledgers live. Defaults to next to `--db`. |
| `PAPER_TRADER_DESK_MINUTES` | Wall-clock budget for all model calls in a run. Default 120. |

To host your own copy, fork the repo and create an `account` branch containing an initialised `account.sqlite` and an `INITIALIZED` file. Enable GitHub Pages with the Actions source and the workflow takes over.

```
src/
  main.rs       command line
  daily.rs      one daily run: live account, shadows, memory, the improvement step
  desk.rs       triage, research agents, debate, judge, doctor, post-mortems
  scorecard.rs  forward returns of every pick, veto and near-miss
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
scripts/        llm.sh installs and starts the local model
docs/           README images
.github/        scheduled run, model smoke test and CI
```

The crate is a library plus a thin binary on edition 2024, with 62 unit tests, and it passes `cargo clippy -- -D warnings` with the pedantic group enabled. Backend code is Rust only.

## Limits

- Sixty days is too short to separate skill from luck, so a good or bad result says little about the rules. The scorecard exists to get more samples, not to remove this limit.
- A 4B model is a weak reader of financial news. The depth ladder measures whether it adds anything.
- Prices come from a free Yahoo endpoint with no guarantees. Missing quotes are logged and the symbol is skipped.
- Stops and targets are checked on daily closes, so a gap can lose more than the stop level.
- Execution is simplified. Liquidity, queue priority, outages and market impact are not modeled.
- GitHub schedules can run late or fail. The last good snapshot stays online until the next run succeeds.

This is a simulation and not financial advice.

If you find it interesting, a star helps other people find it. Issues and critiques of the strategy are welcome.
