# Pending-order repair — 19 November 2026

`before.sqlite` preserves the account before maintenance. Main commit `8568f83`
added `validate-orders`; applying it cancelled only the GEV pending buy because
its original 18 November research records a broken skeptic verdict.

No model was called and no order was filled. All historical fills, trades, equity,
journal, reports, calls, headlines and newsbook rows were compared and are unchanged.
Cash and positions are unchanged within floating-point serialization precision.
The cancellation is recorded in events and the exported dashboard. Sell orders and
the four other pending buys were retained. This is not a historical performance replay.
