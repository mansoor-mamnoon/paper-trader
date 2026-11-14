<div align="center">
<img src="docs/logo.svg" alt="Paper Trader" width="72">

# Paper Trader

A $1,000 simulated account run by a desk of free, local language models. They trade news buzz sector by sector, decide every holding every day, and publish every decision.

[Dashboard](https://mansoor-mamnoon.github.io/paper-trader/) · [Actions](https://github.com/mansoor-mamnoon/paper-trader/actions)
</div>

## What happens each evening

<img src="docs/architecture.svg" alt="Rust gathers data, six model agents decide, Rust keeps the books" width="100%">

After the US close, a GitHub Actions run does one trading day:

1. **Settle.** Rust fills yesterday's orders at today's open, with a 0.1% fee and 0.2% adverse slippage on each side, and marks the book.
2. **Buzz screen.** It reads the entire US market from the Nasdaq screener in one request. It keeps companies worth at least $1B, priced at least $3 and trading at least $25M a day, and drops every ETF, index fund, closed-end fund, ETN, preferred, warrant and unit. It scores each stock by attention: Reddit mentions and their growth, Yahoo and StockTwits trending lists, the size of the day's move, dollar volume, and volume against its 20-day average. The screen decides what the models read. It never decides a trade.
3. **Scouts** (one per sector, 12 sectors) each write the day's newsbook entry from two days of sector news, the sector's tape, its most-buzzed stocks and their own earlier entries.
4. **Strategist** reads the newsbook and picks one to three sectors to trade. The candidates are the eight most-buzzed stocks in those sectors, from the whole eligible market, plus the four most-buzzed stocks anywhere else.
5. **Classifier** labels every headline of every holding and candidate (product launch, big deal, results beat, upgrade, approval, policy, takeover, retail frenzy, demand boom, bad news, noise, unrelated) with a buzz score from 0 to 3. CEO changes, filings, dividends and conference appearances are noise by definition.
6. **Analyst** makes the case for each holding and each candidate. **Skeptic** attacks that case with the same file: is it fresh, is it priced in, is the buzz fading.
7. **Manager** must answer KEEP or SELL for every holding. Every sale must be replaced: it picks the replacements and their weights from the argued candidates. It reads its own notes, decisions and closed trades from earlier days.
8. **Rust queues the orders** for the next open, writes everything to memory and publishes the dashboard.

## Rules no model can change

- Always fully invested in at most five individual US stocks. Sales fill before purchases, and purchases share all the cash by weight. If the manager sells and buys nothing new, the freed cash tops up the stocks it kept.
- No stop-losses, take-profit rules or momentum fallback. A model decides every holding and every purchase. If no model runs, the books are settled and nothing is traded.
- Earnings dates are not a filter. Nothing stops the desk from holding through a report.
- Each answer must match a JSON schema enforced by the model server. A holding can only get KEEP or SELL, a purchase can only be a stock the analyst argued that day, and a focus sector can only be one the scouts wrote about.

## Memory

Everything lives in one SQLite file on the `account` branch:

| Table | What the agents get from it |
|---|---|
| `newsbook` | Each scout reads its sector's last five entries, so a bill moving through Congress or a spending wave is followed across days |
| `headlines` | Headlines are stored with the day the desk first saw them and the classifier's label, so old news is shown as old |
| `journal` | The manager reads its last five notes, the last week of KEEP, SELL and BUY decisions, and every past decision on a holding |
| `trades` | The manager reads recent closed trades and the account's average return by catalyst |
| `calls` | Every prompt and answer, kept ten days and published per day on the dashboard |

## Models

All agents run on **Qwen3.5 9B** (Apache-2.0, 4-bit, 5.7 GB) with llama.cpp on the free GitHub runner (4 CPU cores, 16 GB memory, no GPU). Rust downloads llama.cpp and the model itself, pins each file to a Hugging Face commit and checks its SHA-256.

The previous desk used nine 2–4B models, one per role, chosen to come from nine different companies. The new desk drops that rule. One stronger model plays every role, so there are no model swaps during a run and the time goes into more and longer answers. Larger open models were considered:

| Model | Size at 4 bits | Fits the runner | Notes |
|---|---:|---|---|
| Qwen3.5 9B | 5.7 GB | yes | Chosen. Published reasoning scores (GPQA) above gpt-oss-20b, and it leaves room for a 12k context |
| Gemma 4 12B (QAT) | 7.0 GB | yes | Registered as an alternative to compare |
| gpt-oss-20b | 11.6 GB | barely | Mixture of experts with about 3.6B active parameters, so it is fast on CPU, but 11.6 GB leaves little room for the context in 16 GB |
| Gemma 4 26B-A4B, Qwen3.6 35B-A3B | 13–17 GB | no | Too large for 16 GB |
| Qwen3 4B Instruct (old judge) | 2.5 GB | yes | Kept as the baseline |

`paper-trader bench` puts the same known-answer desk questions to each model: labelling eight headlines, deciding three holdings, and picking two purchases. It reports accuracy and speed. Set `PAPER_TRADER_MODEL=<id>` to run the whole desk on another registered model.

First results, on an Apple M2 Pro (Metal), 13 November 2026:

| Model | Headlines labelled right | Holdings decided right | Purchases right | Generation speed |
|---|---:|---:|---:|---:|
| Qwen3.5 9B | 8 / 8 | 3 / 3 | 2 / 2 | 26 tokens/s |
| Qwen3 4B Instruct | 8 / 8 | 3 / 3 | 2 / 2 | 48 tokens/s |

Both models answered every question correctly, so these questions are too easy to separate them. The 9B model is kept for the harder, open-ended work: newsbook entries, cases and rebuttals. The runner has no GPU, so it is several times slower there. The questions should get harder as the journal collects real cases the desk got wrong.

## Why the desk is built this way

- **Debate, but one round.** [TradingAgents](https://arxiv.org/abs/2412.20138) showed an analyst, debate and manager structure for trading. [Talk Isn't Always Cheap](https://arxiv.org/abs/2509.05396) found that longer debates can move correct answers to wrong ones. So the skeptic gets one rebuttal and the manager decides, with no back-and-forth.
- **Structure matters more than the model.** The live [Agent Market Arena](https://arxiv.org/abs/2510.11695) found that changing the agent structure changed results far more than changing the model. That is why the roles have separate evidence jobs (sector news, headlines, the case, the attack, the book) and not just different personas.
- **News is the signal.** [Lopez-Lira and Tang](https://arxiv.org/abs/2304.07619) found that LLM readings of headlines predict next-day returns, more strongly for smaller stocks. [Beyond Sentiment](https://www.alphaxiv.org/abs/2607.28496) found that structured LLM features add information that sentiment scores miss. That is why the classifier labels the kind of catalyst instead of scoring positive or negative.
- **Why not a separate classifier model.** FinBERT-style models score sentiment, not catalyst type, and running one would add a Python or ONNX runtime to an otherwise all-Rust project. The LLM classifier is one more agent on the model already loaded.
- **Why not fine-tune a smaller model from a larger one.** The MIT work in this area ([DisCIPL](https://news.mit.edu/2025/enabling-small-language-models-solve-complex-reasoning-tasks-1212)) has a large model plan and small models execute; distillation more generally needs a teacher and many labelled examples. The desk has no free teacher that can run on the runner and, so far, only a handful of closed trades to learn from. Revisit once the `trades` and `headlines` tables hold a few hundred labelled outcomes: they are exactly the training set a distilled classifier would need.
- **Why the screen is buzz and not momentum.** The previous screen ranked 20- and 50-day trend winners and then the models explained them. Trend alone produced the CEO-change and pool-supply rationales in the old journal. The new screen surfaces what people are talking about today, and the models read the news to decide whether the talk is worth money.

## Run locally

```sh
cargo test --locked
cargo build --release --locked
./target/release/paper-trader run --db account.sqlite --output site --force
./target/release/paper-trader bench --models qwen3.5-9b,qwen3-4b-instruct
```

The first run downloads llama.cpp and the model (5.7 GB) to `~/.cache/paper-trader-llm`. No API key or account is needed for any data source or model. Open `site/index.html` after copying `web/*` into `site/`.

To run the desk on GitHub now instead of waiting for the evening schedule:

```sh
gh workflow run account.yml --ref main -f force=true
```

## Switchover

The model desk took the account over on 13 November 2026. The ledger was rewound to the 12 November close, and the new desk ran for that evening with `--as-of`, which cuts every source at that moment: bars end at the 12 November session, headlines must have been published before it, and Reddit counts are the previous day's. Trending lists have no history, so that run went without them. Its orders filled at the real 13 November open. The previous desk's 13 November trades are kept in `archive/` on the `account` branch.

## Limits

This is a paper account: no broker is connected. Daily bars cannot show intraday fills. Headlines come from free feeds and can be late or missing. Five concentrated buzz positions are volatile by design, and a few weeks of results cannot show whether the desk has an edge. Nothing here is investment advice.
