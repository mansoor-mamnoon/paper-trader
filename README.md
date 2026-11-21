<div align="center">
<img src="docs/logo.svg" alt="Paper Trader" width="72">

# Paper Trader

A $1,000 paper account that trades US stocks from news and market attention. NVIDIA Nemotron 3 Ultra handles the research and decisions after each US close. The dashboard publishes holdings, orders, results and model transcripts.

[Dashboard](https://mansoor-mamnoon.github.io/paper-trader/) · [Ops](https://mansoor-mamnoon.github.io/paper-trader/#ops) · [Actions](https://github.com/mansoor-mamnoon/paper-trader/actions)
</div>

## Daily process

<img src="docs/architecture.svg" alt="Rust gathers data, six model agents decide, Rust keeps the books" width="100%">

Rust settles queued orders at the next session's open, applies fees and slippage, and updates the account. It screens US stocks using Reddit mentions, trending lists, price moves and trading volume. Eligible companies must be worth at least $5 billion, cost at least $3 a share and trade at least $25 million a day. Funds and other non-company securities are excluded.

FinBERT scores headline sentiment across the screened stocks and holdings. Twelve sector scouts write the newsbook, and a strategist selects one to three sectors. The research pool includes eight stocks from those sectors and four from the rest of the market.

A classifier labels headlines. An analyst builds each stock's case from a cited headline or a bullish sector driver. A skeptic checks the case against the same evidence. The manager reviews every holding and chooses any purchases. Rust checks the decisions, queues orders for the next open and saves the account.

## Trading rules

- Up to eight individual stocks, with cash allowed.
- A purchase cannot take a stock above 25% of account equity. Kept holdings receive no automatic top-ups.
- Purchases require a bullish case and completed skeptic research with a verdict of `stands` or `weakened`.
- Ordinary sales require a separate factual check with a supporting source quotation. Failed checks retain the holding; position-count trims remain possible.
- Sales fill before purchases, with a 0.1% fee and 0.2% adverse slippage on each side.
- Holdings can be sold at any age. There are no automatic stop-loss or take-profit orders.

## Models and memory

| Work | Model | Runtime |
|---|---|---|
| Scout, strategist, classifier, analyst, skeptic, manager | NVIDIA Nemotron 3 Ultra 550B | NVIDIA hosted API |
| Fallback for failed hosted questions | Qwen3.5 9B | llama.cpp on the runner |
| Headline sentiment | ProsusAI FinBERT | Rust with Candle |

The hosted client validates JSON responses, shares rate limits across workers and retries transient failures. Nemotron's constrained requests disable thinking and request nonempty content. Without an API key, the desk uses local inference. The first local run downloads llama.cpp and a 5.7 GB model.

SQLite stores the account, fills, headlines, sector notes, decisions, closed trades and transcripts on the `account` branch. Research includes earlier notes, original entry evidence and order-review outcomes. Current ownership comes from the ledger. The Ops tab shows call timings, failures, data-source requests and the model used for each answer.

## Run locally

Rust 1.88 or later is required.

```sh
cargo test --locked
cargo build --release --locked
./target/release/paper-trader run --db account.sqlite --output site
cp -r web/* site/
python3 -m http.server 8000 --directory site
```

Open `http://localhost:8000`. A new database starts a separate $1,000 account. Existing databases retain their holdings and history.

Set `PAPER_TRADER_API_KEY` in your environment for hosted inference. The hosted configuration uses these defaults:

| Variable | Default |
|---|---|
| `PAPER_TRADER_API_URL` | `https://integrate.api.nvidia.com/v1` |
| `PAPER_TRADER_API_MODEL` | `nvidia/nemotron-3-ultra-550b-a55b` |
| `PAPER_TRADER_API_RPM` | `20` |
| `PAPER_TRADER_API_TPM` | `60000` |
| `PAPER_TRADER_API_TIMEOUT_SECONDS` | `900` |
| `PAPER_TRADER_WORKERS` | `1` |

Use `--minutes` to set a research budget in minutes; the default, `0`, researches every candidate. `--force` repeats an already processed session. `--as-of` accepts an RFC3339 cutoff for historical runs.

## Scheduled runs

GitHub Actions runs after the US close at 22:37 UTC, with catch-up triggers at 00:47 and 03:23 UTC. Completed sessions skip model work. A watchdog checks for missed runs at 07:17 UTC. The account is saved before GitHub Pages publication.

```sh
gh secret set PAPER_TRADER_API_KEY
gh workflow run account.yml --ref main
```

Schedules can be delayed or dropped. The project uses GitHub's public runners and NVIDIA's free prototyping endpoint; no paid fallback is configured.

## Records and limits

Detailed reviews remain in [docs](docs/), including the [November 19 decision review](docs/DECISIONS-2026-11-19.md), [exit review](docs/DECISIONS-2026-11-20.md) and [November 20 session](docs/SESSION-2026-11-20.md). Earlier provider measurements and replay tooling used DeepSeek. Their validator needs updating before it can validate a Nemotron replay.

No broker is connected. Free news feeds can be late or incomplete, and models can misread them. Daily bars cannot reproduce intraday execution. Historical runs use a current stock universe and models with later knowledge. A short record across changing desk versions cannot establish a trading edge. Nothing here is investment advice.
