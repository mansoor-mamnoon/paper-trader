<div align="center">

<img src="docs/banner.svg" alt="Paper Trader, the $1,000 experiment" width="100%">

**A $1,000 simulated US-stock account that trades by fixed rules and publishes every decision.**

[![Live dashboard](https://img.shields.io/badge/live-dashboard-4f8cff?style=for-the-badge)](https://mansoor-mamnoon.github.io/paper-trader/)
![Python](https://img.shields.io/badge/python-3.12-3776ab?style=for-the-badge&logo=python&logoColor=white)
![Rust](https://img.shields.io/badge/rust-2024-dea584?style=for-the-badge&logo=rust&logoColor=white)
![Real money](https://img.shields.io/badge/real%20money-none-lightgrey?style=for-the-badge)

[Live dashboard](https://mansoor-mamnoon.github.io/paper-trader/) · [How it works](#how-it-works) · [Strategy](#strategy) · [Crypto account](#crypto-account) · [Rust](#rust) · [Run it](#run-it) · [Limits](#limits)

</div>

---

<div align="center">
<img src="docs/overview.svg" alt="Overview of data sources, the two accounts, the SQLite ledger and the static dashboard" width="100%">
</div>

## What it is

Paper Trader starts with $1,000 of simulated money and looks at US stocks once a day. A short rulebook decides what to buy and sell. Every signal, every skipped stock and every fill goes into a SQLite ledger, and a read-only dashboard on GitHub Pages shows the whole ledger.

The experiment asks whether a plain trend rule can beat buying SPY after fees and slippage over 60 days. It has no broker connection and places no orders.

<div align="center">
<img src="docs/dashboard.gif" alt="Scrolling through the live Paper Trader dashboard" width="760">
<br><sub>Captured from the live dashboard while the account still held only cash.</sub>
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

The rules are deterministic and no language model picks trades.

Entry requires all of the following:

- price of at least $5
- 20-day average dollar volume of at least $1M
- close above the 20-day average, and the 20-day average above the 50-day average
- 20-day return above 2%

Candidates are ranked by 20-day return and the best ones fill up to three slots, each capped at 20% of equity. A close at or below -4% from entry, or at or above +8%, queues a sell for the next session. A drawdown of 10% permanently stops new entries, and the experiment ends after 60 days.

## Dashboard

<div align="center">
<img src="docs/dashboard.png" alt="Dashboard with equity, cash, drawdown, the SPY comparison, holdings and guardrails" width="760">
</div>

The page shows equity against SPY, current holdings with stop and target levels, the latest 120 decisions with their evidence, the latest 200 fills, scan coverage and provider errors. Full histories download as `decisions.csv`, `fills.csv`, `equity.csv` and `events.csv`, and `api/state` returns the raw snapshot.

## Crypto account

A second $1,000 account trades BTC, ETH, SOL and ADA from public Kraken data. It uses a separate database, because the ledger refuses to mix data sources.

- Signal: the 20-hour average of closed hourly candles above the 50-hour average, with 24-hour momentum above 1%
- Orders queue on one observation and fill on a later fresh one, at the ask plus 0.10% slippage and a 0.40% fee
- Sizing: at most 25% of equity per position, with about 1% of equity at risk per trade and 3% in total
- Exits: -4% stop, +8% target, a seven-day holding limit, or a trend break

The Rust binary runs it directly:

```bash
paper-trader crypto once   --db crypto.sqlite    # observe Kraken and apply one cycle
paper-trader crypto replay --db replay.sqlite --input snapshots.json
paper-trader crypto status --db crypto.sqlite
```

The original Python version, with its local dashboard server, is in `app/`.

## Rust

`rust/` is a Rust implementation of both accounts. It uses the same SQLite schema and the same JSON state as the Python code, so either can open the other's database.

```bash
cd rust
cargo test                                   # 16 unit tests
cargo run --release -- stocks --db account.sqlite --output site
```

I checked the stock account by replaying four identical observations through both implementations. The fills and the equity curve matched to six decimals. The decision rows matched except for insertion order within a day, because the Python code iterates symbols in dictionary order and the Rust code in alphabetical order. The Rust port has no local server yet, and the crypto account is covered by unit tests but has not been compared against Python.

## Run it

Python 3.12 and the standard library are enough:

```bash
git clone https://github.com/mansoor-mamnoon/paper-trader.git
cd paper-trader
python -m projects.paper_trader.stocks --db account.sqlite --output site
python -m http.server --directory site 8000
```

| Flag | Meaning |
|---|---|
| `--batch-size N` | Symbols per run. 20000 covers the full directory with many requests. |
| `--replay FILE` | Apply a saved observation to a separate account. |

To host your own copy, fork the repo and create an `account` branch containing an initialised `account.sqlite` and an `INITIALIZED` file. Enable GitHub Pages with the Actions source and the workflow takes over.

```
.github/workflows/    account.yml (scheduled run), rust.yml (tests)
projects/paper_trader Python stock account and dashboard
rust/                 Rust implementation of both accounts
app/                  original Python crypto account and local server
docs/                 README images
```

## Limits

- Sixty days is too short to separate skill from luck, so a good or bad result says little about the rules.
- Prices come from a free Yahoo endpoint with no guarantees. Missing quotes are logged and the symbol is skipped.
- Stops and targets are checked on daily closes, so a gap can lose more than the stop level.
- Execution is simplified. Liquidity, queue priority, outages and market impact are not modeled.
- GitHub schedules can run late or fail. The last good snapshot stays online until the next run succeeds.

This is a simulation and not financial advice.

If you find it interesting, a star helps other people find it. Issues and critiques of the strategy are welcome.
