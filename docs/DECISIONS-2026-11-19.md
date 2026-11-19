# Decision audit: 19 November 2026

Evidence: `origin/account` at `74cea5b`, `account.sqlite` reports, journal, trades,
and the published `site/api/state.json`. Reviewed sessions: 16–18 November.
These are claims recorded by the desk, not independently verified company news.

The account through 18 November is $909.50 (−9.05%) versus $993.08 (−0.69%) for
SPY. There are 21 closed trades. The history spans different desks and models;
it cannot establish the current model's standalone profitability.

| Recorded mistake | Cause | Repair |
|---|---|---|
| NVDA bought on 17 November despite `broken`: skeptic disputes the alleged earnings beat and notes contradictory sector timing. | Purchase list checked only `catalyst != none`. | Require a completed skeptic with `stands` or `weakened`; `broken` and failed research cannot fund purchases. |
| GEV queued on 18 November: manager explicitly says “case broken, minimal weight.” | Skeptic verdict was advisory even when evidence contradicted the case. | Same eligibility check; a small weight cannot bypass a broken case. |
| Newly collected older stories described as today's news. NXT's backlog story was first seen 17 November but published 14 November UTC. | SQL filtered and ordered by first-seen day; dossier called that date the news date. | Filter/order by publication timestamp and display publication date. Keep first-seen metadata for memory. Exclude undated items from fresh-news windows. |
| Weak or unclassified headlines could carry a case; any sector note could rescue a failed citation. | A negative-label blacklist and `note.is_some()` were sufficient. | Require recognized bullish labels and buzz >= 2. Sector-only support requires explicit evidence 0, today's same-sector bullish note and a nonempty driver. |

The skeptic's claim on 18 November that NXT's story was nine days old is also
incorrect against the stored publication timestamp: it was four calendar days old.
The fix uses timestamps rather than accepting either model's age claim.

The repeated portfolio replacements exposed an additional policy defect: the manager
could not decline a weak replacement, sales could be cancelled to retain four names,
and cash could automatically top up holdings with no new buy decision. The purchase
schema now permits zero selections. Sales remain valid with fewer than four names;
both automatic top-up paths have been removed. This deliberately changes the prior
fully-invested policy. A new regression settles a sale after all replacements are
declined and verifies the account holds cash with no unintended purchases.

Validation uses deterministic fixtures and regression tests; it does not rerun a
language model on historical dates or claim a profitable counterfactual. Historical
fills and model transcripts are retained as evidence.

## Pending orders and rollout

The settlement path now rechecks queued buys against their signal-time report,
including buys written before this repair. A missing original report, missing skeptic,
or broken case cancels the buy and records an event; sell orders are preserved.
The `validate-orders` command permits this maintenance without settling the book or
rerunning historical model decisions. It is idempotent. Tests cover valid buys,
broken cases, orphan buys, preserved exits and repeated validation.

Local validation: Rust tests, strict Clippy, formatting, dashboard JavaScript syntax,
and the Python replay/tuning tests. These checks establish the intended execution
behavior, not trading quality or future profitability.

Applied to the durable account on 19 November: only GEV's pending buy was cancelled.
Account commit `8548f1d` includes the pre-change database in
`archive/decision-audit-2026-11-19/before.sqlite`. All historical table rows were
compared unchanged; cash and positions were checked within serialization precision.
The other pending buys and all exits remain. No new desk session was forced.

[CI passed on the implementation](https://github.com/mansoor-mamnoon/paper-trader/actions/runs/44488086744):
81 Rust tests, strict Clippy, formatting and nine Python tests.
