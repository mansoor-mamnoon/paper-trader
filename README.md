<div align="center">
<img src="docs/logo.svg" alt="Paper Trader" width="72">

# Paper Trader

A $1,000 simulated account. Nine local models research US stocks, debate the evidence, and record their decisions on GitHub Pages.

[Dashboard](https://mansoor-mamnoon.github.io/paper-trader/) · [Actions](https://github.com/mansoor-mamnoon/paper-trader/actions) · [Incident audit](docs/AUDIT-2026-11.md)
</div>

## Allocation

The account invests available capital in individual stocks and SPY, a broad US stock fund. The desk can choose up to five stocks. SPY holds the remaining allocation, including when the desk finds no suitable stock. Stock purchases sell enough SPY to cover their cost. Sale proceeds return to SPY at the session open after stock orders execute.

This policy is enabled in the account workflow with `PAPER_TRADER_FULLY_INVESTED=1`. It takes effect at the next observed session open. A rerun does not backdate new trades. Fees, price changes, unavailable quotes and the experiment's end mean invested value can differ from the original $1,000.

Individual stocks retain the 20% position cap, 30% industry cap, volatility sizing and stop rules. The drawdown lock blocks new individual-stock purchases while SPY stays invested. SPY has no trading stop or industry cap. The account remains exposed to market declines.

## Architecture

<img src="docs/architecture.svg" alt="Market data and identity checks feed research and debate, then audited decisions and the SPY allocation enter the ledger and dashboard" width="100%">

The Rust program reads daily prices, issuer-matched news, an earnings calendar and optional SEC filings. The screen checks identity, liquidity, corporate events and price history. Research models inspect the survivors. A bull proposes a mechanism; a bear challenges it; the judge compares the candidates. Claim checks reject defined classes of unsupported assertions before orders enter SQLite.

Price spikes alone are technical signals. A risk-off stock purchase needs a cited company event. Missing earnings data remains unknown. Model conviction is an uncalibrated score. The November 10 journal contains errors that prompted the [incident repairs](docs/AUDIT-2026-11.md); saved-provider tests cover those examples.

GitHub Actions runs the desk after a completed US session. Orders fill at a later observed session open with a 0.10% fee and 0.20% adverse slippage per side. The `account` branch stores the database and exported dashboard data. GitHub Pages publishes the dashboard. No broker is connected.

## Dashboard tour

<img src="docs/dashboard.gif" alt="Recorded walkthrough of overview, model desk, strategy lab, operations, journal and system explanation" width="100%">

The recording uses saved account data. Its journal predates the new allocation policy; the live dashboard shows each run's timestamp. The desk tab contains model arguments, the journal contains decisions and fills, and the strategy lab compares the live account with shadow strategies.

## Run locally

```sh
cargo test --locked
cargo build --release --locked
PAPER_TRADER_FULLY_INVESTED=1 ./target/release/paper-trader stocks --db account.sqlite --output site
```

Set `PAPER_TRADER_LLAMA_SERVER` to the path returned by `scripts/llm.sh install` to enable the model desk. Models download on demand. Set `PAPER_TRADER_SEC_CONTACT` to a contact email to enable SEC requests. Without a model server, the quantitative ranking selects stocks.

To recompute the latest completed session on GitHub:

```sh
gh workflow run account.yml --ref main -f force=true
```

The rerun preserves executed fills and rechecks pending orders. It can return the same stock choices. It cannot replay an arbitrary earlier date against the live account.

## Limits

Daily bars cannot resolve intraday stop fills or live execution quality. News matching and claim checks can miss errors. Model scores have little outcome history, and shadow results share market sessions. The experiment lasts 60 days; its results do not establish a durable trading advantage.
