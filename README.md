<div align="center">

<img src="docs/banner.svg" alt="Paper Trader, the $1,000 experiment" width="100%">

**A $1,000 simulated US-stock account run by a free, open-weights AI analyst that reads the news and earnings calendar, tests its own improvements, and publishes every decision.**

[![Live dashboard](https://img.shields.io/badge/live-dashboard-4f8cff?style=for-the-badge)](https://mansoor-mamnoon.github.io/paper-trader/)
![Rust](https://img.shields.io/badge/rust-2024-dea584?style=for-the-badge&logo=rust&logoColor=white)
[![CI](https://img.shields.io/github/actions/workflow/status/mansoor-mamnoon/paper-trader/rust.yml?branch=main&style=for-the-badge&label=tests)](https://github.com/mansoor-mamnoon/paper-trader/actions/workflows/rust.yml)
![Real money](https://img.shields.io/badge/real%20money-none-lightgrey?style=for-the-badge)

[Live dashboard](https://mansoor-mamnoon.github.io/paper-trader/) · [How it works](#how-it-works) · [Strategy](#strategy) · [Crypto account](#crypto-account) · [Rust](#rust) · [Run it](#run-it) · [Limits](#limits)

</div>

---

<div align="center">
<img src="docs/overview.svg" alt="Overview of data sources, the two accounts, the SQLite ledger and the static dashboard" width="100%">
</div>

## What it is

Paper Trader starts with $1,000 of simulated money and looks at US stocks once a day. A quantitative screen finds stocks in an uptrend, then an open-weights language model (Qwen3 4B, Apache-2.0) reads their recent headlines and upcoming earnings dates and decides which to buy. A short rulebook handles sizing and exits. Every signal, every skipped stock and every fill goes into a SQLite ledger, and a read-only dashboard on GitHub Pages shows the whole ledger.

The experiment asks whether a plain trend rule can beat buying SPY after fees and slippage over 60 days. It has no broker connection and places no orders.

<div align="center">
<img src="docs/dashboard.gif" alt="Scrolling through the live Paper Trader dashboard" width="760">
<br><sub>A tour of the five tabs, using a replayed scenario with synthetic prices.</sub>
</div>

| Setting | Value |
|---|---|
| Starting capital | $1,000, long only, whole shares |
| Universe | US exchange-listed symbols from the Nasdaq directory, ETFs included |
| Schedule | Daily at 22:37 UTC, Monday to Friday |
| Costs | 0.10% fee per side, 0.20% slippage against each fill |
| Benchmark | SPY, price only |
| Hosting | GitHub Actions and GitHub Pages |

## How it works

<div align="center">
<img src="docs/pipeline.svg" alt="Daily pipeline from universe to published dashboard" width="100%">
</div>

1. The worker downloads the Nasdaq symbol directory and removes test issues.
2. It fetches delayed daily bars from Yahoo Finance for a rotating batch of symbols (800 by default). Held positions, queued orders and SPY are always included, so the whole market is covered over several runs.
3. Each symbol is checked against the entry rules and logged as `CANDIDATE` or `PASS` with the numbers behind the call.
4. A signal creates a queued order. It fills at the open of a later completed session, after slippage and fees.
5. The account is exported as JSON and CSV and the dashboard is redeployed.

The durable account lives on a dedicated `account` branch. The workflow restores it, runs `PRAGMA integrity_check`, and stops with an error if the database is missing, so a lost file cannot silently reset the account.

## Strategy

The screen is the starting point. Each day a stock has to pass it before the analyst sees it. These are the defaults, and the strategy lab can change them within fixed bounds.

- price of at least $5
- 20-day average dollar volume of at least $1M
- close above the 20-day average, and the 20-day average above the 50-day average
- 20-day return above 2%
- no earnings report inside the blackout window (off by default)

The best 25 survivors go to the analyst with their headlines and earnings dates. It ranks the ones it believes in and can veto the rest, but it can only pick from the list it was given and cannot exceed the open slots. Three positions at most, each capped at 20% of equity. A close at or below the stop (-4% by default) or at or above the target (+8%) queues a sell for the next session. A drawdown of 10% permanently stops new entries, and the experiment ends after 60 days.

If the analyst is unavailable, the account falls back to ranking by 20-day return, which is how the original version traded.

## Strategy lab

Nothing here is meant to stay fixed. The loop follows the pattern of recursive self-improvement in the [KISS `rsi7d` agent](https://github.com/ksenxx/kiss_ai/tree/main/src/kiss/agents/seas/rsi7d): mine recent runs for evidence, propose a bounded edit, test it, and keep it only if the evidence supports it. Here the edit target is the strategy and the test is a live shadow account.

1. Once a week the system reviews how the live strategy traded: closed trades, win rate, what the analyst said, how buys near earnings dates behaved.
2. The model proposes one small change. It can adjust at most two things: numeric parameters inside fixed bounds, or the text of the analyst's playbook. Anything out of bounds is rejected and logged.
3. The change trades a separate $1,000 shadow account on the same daily data.
4. After 14 days, a challenger replaces the live strategy only if it beats it by at least one percentage point with at least two trades and a drawdown no more than three points worse.
5. The replaced strategy keeps trading as a shadow. If the promotion was luck, it wins the seat back under the same rule.

Position size, the three-position cap, costs and the loss halt live in code and are not editable. The registry and every shadow ledger are committed to the `account` branch, so each change and its evidence stay public.

Two caveats apply. Two weeks of paper trades is a small sample, so the loop will sometimes promote noise, and the margin rules reduce that without removing it. And a language model reading headlines has no proven edge over the screen alone. The shadow accounts exist to measure that.

## Free and autonomous

There is no API key and no account to set up. Each scheduled run does this on a free GitHub runner:

1. Restores the cached model (first run downloads about 2.5 GB from Hugging Face, later runs reuse it).
2. Starts `llama-server` from [llama.cpp](https://github.com/ggml-org/llama.cpp) with [Qwen3-4B-Instruct-2507](https://huggingface.co/Qwen/Qwen3-4B-Instruct-2507) quantized to 4 bits. Output is constrained to a JSON schema, so a 4B model cannot return malformed answers.
3. Runs the day: scan, analyst, shadow accounts, weekly proposal, export.
4. Commits the ledgers and redeploys the dashboard.

Triggers: the weekday schedule, a manual run, or an external webhook (`repository_dispatch` with event type `run-account`). GitHub pauses scheduled workflows in quiet repositories, so every run also re-enables its own schedule. If the model server fails to start, the account still trades by momentum ranking and the dashboard says so.

A 4B model is a weak reader of financial news, and this setup is built to find that out. On its first run the lab adds a no-AI control shadow (the same screen ranked by 20-day gain). If the analyst does not beat the control, the control becomes the live strategy.

Run it yourself:

```bash
scripts/llm.sh                                   # download and start the model on port 8080
PAPER_TRADER_LLM_URL=http://127.0.0.1:8080 cargo run --release -- stocks --db account.sqlite --output site
scripts/llm.sh stop
```

Any server that speaks the OpenAI chat protocol works (Ollama, LM Studio, vLLM). Set `PAPER_TRADER_LLM_URL`, and optionally `PAPER_TRADER_LLM_MODEL` and `PAPER_TRADER_LLM_KEY`. A paid model is also supported: set `ANTHROPIC_API_KEY` and, if you like, `PAPER_TRADER_MODEL`. When both are present the local server wins.

## Dashboard

<div align="center">
<img src="docs/dashboard.png" alt="Overview tab with the headline result, equity chart against SPY and what happens next" width="760">
</div>

The site has six tabs. Overview opens with a one-line verdict (ahead of or behind SPY, and by how much), a 60-day progress bar, the equity chart with hover values, and a list of what happens at the next session. Holdings shows each stock's price between its stop and its target. Trades lists every fill. Journal explains each decision in plain English, with the headlines the analyst saw, a ticker search, and a filter that hides the thousands of routine "did not qualify" rows. Strategy lab shows today's analyst view, the live strategy, the challengers being tested and the change log. How it works covers the rules, the guardrails and the limits. Hover the `?` icons for definitions of cash, drawdown, fees and profit and loss.

Full histories download as `decisions.csv`, `fills.csv`, `equity.csv` and `events.csv`, and `api/state` returns the raw snapshot. The layout works on phones and follows your light or dark setting.

## Crypto account

A second $1,000 account trades BTC, ETH, SOL and ADA from public Kraken data. It uses a separate database, because the ledger refuses to mix data sources.

- Signal: the 20-hour average of closed hourly candles above the 50-hour average, with 24-hour momentum above 1%
- Orders queue on one observation and fill on a later fresh one, at the ask plus 0.10% slippage and a 0.40% fee
- Sizing: at most 25% of equity per position, with about 1% of equity at risk per trade and 3% in total
- Exits: -4% stop, +8% target, a seven-day holding limit, or a trend break

The binary runs it directly:

```bash
paper-trader crypto once   --db crypto.sqlite    # observe Kraken and apply one cycle
paper-trader crypto replay --db replay.sqlite --input snapshots.json
paper-trader crypto status --db crypto.sqlite
```

## Run it

You need a recent stable Rust toolchain (1.88 or newer).

```bash
git clone https://github.com/mansoor-mamnoon/paper-trader.git
cd paper-trader
cargo run --release -- stocks --db account.sqlite --output site
# serve site/ with any static file server
cp web/* site/                                  # the dashboard
```

| Flag | Meaning |
|---|---|
| `--batch-size N` | Symbols per run. 20000 covers the full directory with many requests. |
| `--replay FILE` | Apply a saved observation to a separate account. |

To host your own copy, fork the repo and create an `account` branch containing an initialised `account.sqlite` and an `INITIALIZED` file. Enable GitHub Pages with the Actions source and the workflow takes over.

```
src/
  main.rs      command line
  account.rs   screen, queued orders, fills, risk limits
  analyst.rs   model clients, the analyst and its output checks
  intel.rs     earnings calendar and headlines
  strategy.rs  the bounded, editable strategy
  lab.rs       shadow accounts, promotion rule, proposals
  daily.rs     one daily run across all strategies
  data.rs      Nasdaq directory and Yahoo daily bars
  crypto.rs    crypto account and Kraken data
  ledger.rs    SQLite schema, state and reports
  export.rs    JSON and CSV snapshot
web/           the dashboard (plain HTML, CSS and JavaScript)
docs/          README images
.github/       scheduled run and CI
scripts/       llm.sh installs and starts the local model
```

The crate is a library plus a thin binary, builds on edition 2024, and passes `cargo clippy -- -D warnings` with the pedantic group enabled. It began as a Python program. Replaying the same four observations through both versions gave identical fills and equity curves to six decimals, and the Python code has since been removed.

## Limits

- Sixty days is too short to separate skill from luck, so a good or bad result says little about the rules.
- Prices come from a free Yahoo endpoint with no guarantees. Missing quotes are logged and the symbol is skipped.
- Stops and targets are checked on daily closes, so a gap can lose more than the stop level.
- Execution is simplified. Liquidity, queue priority, outages and market impact are not modeled.
- GitHub schedules can run late or fail. The last good snapshot stays online until the next run succeeds.

This is a simulation and not financial advice.

If you find it interesting, a star helps other people find it. Issues and critiques of the strategy are welcome.
