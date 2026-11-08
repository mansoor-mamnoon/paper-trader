<div align="center">

<img src="docs/logo.svg" alt="Paper Trader logo" width="84">

<img src="docs/banner.svg" alt="Paper Trader, the $1,000 experiment" width="100%">

**A $1,000 paper-trading account run by a free, open-weights AI. It reads the news and the earnings calendar, tests its own changes, and publishes every decision.**

[![Live dashboard](https://img.shields.io/badge/live-dashboard-ffe14d?style=for-the-badge&labelColor=0f1115)](https://mansoor-mamnoon.github.io/paper-trader/)
[![Tests](https://img.shields.io/github/actions/workflow/status/mansoor-mamnoon/paper-trader/rust.yml?branch=main&style=for-the-badge&label=tests&labelColor=0f1115)](https://github.com/mansoor-mamnoon/paper-trader/actions/workflows/rust.yml)
![Rust](https://img.shields.io/badge/rust-2024-ffe14d?style=for-the-badge&logo=rust&logoColor=white&labelColor=0f1115)
![Model](https://img.shields.io/badge/model-Qwen3%204B%20open%20weights-ffe14d?style=for-the-badge&labelColor=0f1115)
![Cost](https://img.shields.io/badge/cost-%240-ffe14d?style=for-the-badge&labelColor=0f1115)

[Live dashboard](https://mansoor-mamnoon.github.io/paper-trader/) · [How it works](#how-it-works) · [Strategy](#strategy) · [Strategy lab](#strategy-lab) · [Free and autonomous](#free-and-autonomous) · [Run it](#run-it) · [Limits](#limits)

</div>

---

<div align="center">
<img src="docs/overview.svg" alt="Overview: data sources feed the screen and the AI analyst, the live and shadow accounts trade, ledgers are committed, and a static dashboard publishes the results" width="100%">
</div>

## What it is

Paper Trader starts with $1,000 of simulated money and looks at US stocks once a day. A quantitative screen finds stocks in an uptrend. Then a small open-weights language model (Qwen3 4B, Apache-2.0) reads their recent headlines and upcoming earnings dates and ranks the ones it believes in. A fixed rulebook handles sizing and exits. Every signal, every skipped stock and every fill goes into a SQLite ledger, and a read-only dashboard on GitHub Pages shows all of it.

The question is whether this beats buying SPY after fees and slippage over 60 days. It places no real orders and has no broker connection. Everything runs on GitHub's free runners with no API key, and the whole backend is Rust.

<div align="center">
<img src="docs/dashboard.gif" alt="A tour through the six tabs of the dashboard" width="760">
<br><sub>The six tabs, shown with a replayed scenario and synthetic prices.</sub>
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

## How it works

<div align="center">
<img src="docs/pipeline.svg" alt="Daily pipeline: scan, screen, read, model picks, queue and fill, publish" width="100%">
</div>

1. The run downloads the Nasdaq symbol directory and a rotating batch of delayed daily bars from Yahoo Finance (800 symbols by default). Held positions, queued orders and SPY are always included.
2. Each stock is screened and logged as `CANDIDATE` or `PASS` with the numbers behind the call.
3. For the best candidates it fetches the next two weeks of earnings dates from Nasdaq and the latest headlines from Yahoo Finance.
4. The model ranks the candidates, writes a short thesis for each pick and can veto stocks. It can only choose from the list it was shown.
5. A buy never fills at the price that triggered it. It queues for a later session and fills at the open with fees and slippage applied.
6. The ledgers are exported as JSON and CSV and the dashboard is redeployed.

The durable account lives on a dedicated `account` branch. The workflow restores it, runs `PRAGMA integrity_check`, and stops with an error if the database is missing, so a lost file cannot silently reset the account.

## Strategy

The screen is the starting point and the strategy lab can change its values within fixed bounds. The defaults:

- price of at least $5
- 20-day average dollar volume of at least $1M
- close above the 20-day average, with the 20-day average above the 50-day average
- 20-day return above 2%
- no earnings report inside the blackout window (off by default)

The best 25 survivors go to the model. Three positions at most, each capped at 20% of equity. A close at or below the stop (-4% by default) or at or above the target (+8%) queues a sell for the next session. A 10% drawdown permanently stops new entries, and the experiment ends after 60 days. If the model is unavailable, stocks are ranked by 20-day return, which is how the first version traded.

## Strategy lab

Nothing here is meant to stay fixed. The loop follows the recursive self-improvement pattern of the [KISS `rsi7d` agent](https://github.com/ksenxx/kiss_ai/tree/main/src/kiss/agents/seas/rsi7d): mine recent runs for evidence, propose a bounded edit, test it, and keep it only if the evidence supports it. Here the edit target is the strategy and the test is a live shadow account.

1. Once a week the system reviews how the live strategy traded: closed trades, win rate, what the model said, how buys near earnings dates behaved.
2. The model proposes one small change. It can adjust at most two things: numeric parameters inside fixed bounds, or the text of the analyst's playbook. Anything out of bounds is rejected and logged.
3. The change trades a separate $1,000 shadow account on the same daily data.
4. After 14 days a challenger replaces the live strategy only if it beats it by at least one percentage point with at least two trades and a drawdown no more than three points worse.
5. The replaced strategy keeps trading as a shadow. If the promotion was luck, it wins the seat back under the same rule.

On its first run the lab also adds a control shadow: the same screen ranked by 20-day return with no AI. If the model does not beat the control, the control becomes the live strategy.

Position size, the three-position cap, costs and the loss halt are written in code and no proposal can change them. The registry and every shadow ledger are committed to the `account` branch, so each change and its evidence stay public.

## Free and autonomous

There is no API key and no account to set up. Each scheduled run does this on a free GitHub runner:

1. Restores the cached model. The first run downloads about 2.5 GB from Hugging Face and later runs reuse it.
2. Starts `llama-server` from [llama.cpp](https://github.com/ggml-org/llama.cpp) with [Qwen3-4B-Instruct-2507](https://huggingface.co/Qwen/Qwen3-4B-Instruct-2507) quantized to 4 bits. Output is constrained to a JSON schema, so the model cannot return malformed answers.
3. Runs the day: scan, analyst, shadow accounts, weekly proposal, export.
4. Commits the ledgers and redeploys the dashboard.

A full run on a CPU-only runner takes about five minutes. Triggers are the weekday schedule, a manual run, or an external webhook (`repository_dispatch` with event type `run-account`). GitHub pauses scheduled workflows in quiet repositories, so every run also re-enables its own schedule. If the model server fails to start, the account still trades by momentum ranking and the dashboard says so. The `Model smoke test` workflow exercises the model on a scratch account without touching the live ledger.

## Dashboard

<div align="center">
<img src="docs/dashboard.png" alt="Overview tab with the headline result, equity chart against SPY and what happens next" width="760">
</div>

Six tabs. Overview opens with a one-line verdict against SPY, a 60-day progress bar, the equity chart with hover values, and what happens at the next session. Holdings shows each stock's price between its stop and its target. Trades lists every fill. Journal explains each decision in plain English with the headlines the model saw, plus a ticker search and a filter that hides routine "did not qualify" rows. Strategy lab shows today's analyst view, the live strategy, the challengers being tested and the change log. How it works covers the rules and the limits. Hover the `?` icons for definitions.

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

| Flag | Meaning |
|---|---|
| `--batch-size N` | Symbols per run. 20000 covers the full directory with many requests. |
| `--replay FILE` | Apply a saved observation to a separate account. |
| `--strategies DIR` | Where the strategy registry and shadow ledgers live. Defaults to next to `--db`. |

To host your own copy, fork the repo and create an `account` branch containing an initialised `account.sqlite` and an `INITIALIZED` file. Enable GitHub Pages with the Actions source and the workflow takes over.

```
src/
  main.rs      command line
  daily.rs     one daily run across the live account and its shadows
  account.rs   screen, queued orders, fills, risk limits
  analyst.rs   model clients, the analyst and its output checks
  intel.rs     earnings calendar and headlines
  strategy.rs  the bounded, editable strategy
  lab.rs       shadow accounts, promotion rule, proposals
  data.rs      Nasdaq directory and Yahoo daily bars
  crypto.rs    crypto account and Kraken data
  ledger.rs    SQLite schema, state and reports
  export.rs    JSON and CSV snapshot
web/           the dashboard (plain HTML, CSS and JavaScript)
scripts/       llm.sh installs and starts the local model
docs/          README images
.github/       scheduled run, model smoke test and CI
```

The crate is a library plus a thin binary on edition 2024, with 34 unit tests, and it passes `cargo clippy -- -D warnings` with the pedantic group enabled. Backend code is Rust only.

## Limits

- Sixty days is too short to separate skill from luck, so a good or bad result says little about the rules.
- A 4B model is a weak reader of financial news. The control shadow exists to measure whether it adds anything.
- Prices come from a free Yahoo endpoint with no guarantees. Missing quotes are logged and the symbol is skipped.
- Stops and targets are checked on daily closes, so a gap can lose more than the stop level.
- Execution is simplified. Liquidity, queue priority, outages and market impact are not modeled.
- GitHub schedules can run late or fail. The last good snapshot stays online until the next run succeeds.

This is a simulation and not financial advice.

If you find it interesting, a star helps other people find it. Issues and critiques of the strategy are welcome.
