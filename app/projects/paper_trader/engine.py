"""Long-only paper execution with deferred entries and explicit loss budgets."""

import time

from . import ledger, market

FEE = 0.004
SLIP = 0.001


def sale_price(quote):
    """Model an adverse sell fill at bid less ten basis points."""
    return quote["bid"] * (1 - SLIP)


def value(state, quotes):
    """Mark positions at conservative net liquidation proceeds."""
    return state["cash"] + sum(
        pos["quantity"] * sale_price(quotes[symbol]) * (1 - FEE)
        for symbol, pos in state["positions"].items()
    )


def planned_risk(state):
    """Sum original entry-to-stop losses, including simulated execution costs."""
    return sum(pos["planned_risk"] for pos in state["positions"].values())


def decision(db, at, symbol, action, reason, evidence=None):
    """Append an immutable decision, including the information then available."""
    import json

    db.execute(
        "INSERT INTO decisions(at,symbol,action,reason,evidence) VALUES(?,?,?,?,?)",
        (at, symbol, action, reason, json.dumps(evidence or {})),
    )


def sell(db, state, at, symbol, quote, reason, signal_at):
    """Liquidate an existing simulated holding at the current adverse price."""
    pos = state["positions"].pop(symbol)
    price = sale_price(quote)
    notional = pos["quantity"] * price
    fee = notional * FEE
    pnl = notional - fee - pos["cost"]
    state["cash"] += notional - fee
    state["fees"] += fee
    state["realized_pnl"] += pnl
    state["cooldown"][symbol] = at + 86400
    state["pending"].pop(symbol, None)
    db.execute(
        "INSERT INTO fills(at,signal_at,symbol,side,quantity,price,notional,fee,"
        "realized_pnl,reason) VALUES(?,?,?,?,?,?,?,?,?,?)",
        (at, signal_at, symbol, "SELL", pos["quantity"], price, notional, fee, pnl, reason),
    )
    decision(db, at, symbol, "SELL_FILLED", reason, {"bid": quote["bid"], "price": price})


def buy(db, state, at, symbol, quote, pending, equity):
    """Size and execute a delayed entry only within all current risk budgets."""
    price = quote["ask"] * (1 + SLIP)
    stop = price * 0.96
    unit_cost = price * (1 + FEE)
    unit_risk = unit_cost - stop * (1 - SLIP) * (1 - FEE)
    # Five basis points of equity buffer avoids exhausting the aggregate limit
    # merely through the execution fees and conservative liquidation mark.
    budget = equity * 0.0095
    if planned_risk(state) + budget > equity * 0.03:
        decision(db, at, symbol, "REJECT", "Aggregate planned-risk limit")
        return
    quantity = min(equity * 0.25 / unit_cost, budget / unit_risk, state["cash"] / unit_cost)
    if quantity * unit_cost < 10:
        decision(db, at, symbol, "REJECT", "Less than $10 available under risk limits")
        return
    notional = quantity * price
    fee = notional * FEE
    cost = notional + fee
    state["cash"] -= cost
    state["fees"] += fee
    state["positions"][symbol] = {
        "symbol": symbol,
        "quantity": quantity,
        "entry": price,
        "cost": cost,
        "stop": stop,
        "target": price * 1.08,
        "opened_at": at,
        "planned_risk": quantity * unit_risk,
    }
    db.execute(
        "INSERT INTO fills(at,signal_at,symbol,side,quantity,price,notional,fee,"
        "realized_pnl,reason) VALUES(?,?,?,?,?,?,?,?,?,?)",
        (
            at,
            pending["at"],
            symbol,
            "BUY",
            quantity,
            price,
            notional,
            fee,
            0,
            "Prior closed-candle signal; later fresh observation",
        ),
    )
    decision(
        db,
        at,
        symbol,
        "BUY_FILLED",
        "Deferred entry passed risk checks",
        {"ask": quote["ask"], "trade_at": quote["trade_at"], "planned_risk": budget},
    )


def cycle(path, snapshot, mode="live"):
    """Apply one validated observation atomically; duplicate timestamps are inert."""
    if mode not in ("live", "replay"):
        raise ValueError("Unknown account mode")
    market.validate(snapshot)
    at, quotes = snapshot["at"], snapshot["quotes"]
    db = ledger.connect(path)
    try:
        db.execute("BEGIN IMMEDIATE")
        state = ledger.load(db)
        if state["mode"] not in (None, mode):
            raise ValueError("Live and replay data must use separate databases")
        if mode == "live" and not 0 <= time.time() - at <= 180:
            raise ValueError("Live observation is stale or ahead of the local clock")
        if at <= state["last_at"]:
            return
        state["mode"] = mode
        if state["started_at"] is None:
            state["started_at"] = at
            state["benchmark_quantity"] = 1000 / (quotes["BTC"]["ask"] * (1 + SLIP) * (1 + FEE))
            ledger.event(db, at, "start", "60-day paper experiment started with $1,000")
        equity = value(state, quotes)
        state["peak"] = max(state["peak"], equity)
        drawdown = 1 - equity / state["peak"]
        state["max_drawdown"] = max(state["max_drawdown"], drawdown)
        if not state["halted"] and (drawdown >= 0.10 or at >= state["started_at"] + 60 * 86400):
            state["halted"] = True
            state["halt_reason"] = (
                "10% drawdown halt" if drawdown >= 0.10 else "60-day experiment ended"
            )
            ledger.event(db, at, "halt", state["halt_reason"])
        exited = False
        for symbol, pos in list(state["positions"].items()):
            quote = quotes[symbol]
            reason = ""
            if state["halted"]:
                reason = state["halt_reason"]
            elif quote["bid"] <= pos["stop"]:
                reason = "4% stop crossed; filled at observed bid, not historical stop"
            elif quote["bid"] >= pos["target"]:
                reason = "8% take-profit threshold"
            elif at - pos["opened_at"] >= 7 * 86400:
                reason = "Seven-day maximum holding period"
            elif pos["quantity"] * quote["bid"] > equity * 0.25:
                reason = "25% position exposure rebalance"
            elif planned_risk(state) > equity * 0.03:
                reason = "3% aggregate planned-risk rebalance"
            if reason:
                sell(db, state, at, symbol, quote, reason, pos["opened_at"])
                exited = True
                equity = value(state, quotes)
        for symbol, pending in list(state["pending"].items()):
            quote = quotes[symbol]
            if state["halted"] or at - pending["at"] > 3600:
                state["pending"].pop(symbol)
                decision(db, at, symbol, "CANCEL", "Halted or signal expired after one hour")
                continue
            if at - pending["at"] < 60 or quote["trade_at"] <= pending["at"]:
                continue
            state["pending"].pop(symbol)
            if pending["side"] == "BUY" and exited:
                decision(db, at, symbol, "CANCEL", "Portfolio exit invalidated queued entry")
            elif pending["side"] == "SELL":
                sell(db, state, at, symbol, quote, "Deferred trend exit", pending["at"])
            elif market.signal(quote)[0]:
                buy(db, state, at, symbol, quote, pending, value(state, quotes))
            else:
                decision(db, at, symbol, "CANCEL", "Trend no longer qualifies")
        for symbol, quote in quotes.items():
            pos = state["positions"].get(symbol)
            qualifies, evidence = market.signal(quote)
            evidence.update(
                {"bid": quote["bid"], "ask": quote["ask"], "trade_at": quote["trade_at"]}
            )
            if state["halted"]:
                continue
            if symbol in state["pending"]:
                decision(db, at, symbol, "WAIT", "Waiting for a later fresh observation", evidence)
            elif pos and not qualifies:
                state["pending"][symbol] = {"at": at, "side": "SELL"}
                decision(
                    db, at, symbol, "SELL", "Trend weakened; queue next-observation exit", evidence
                )
            elif pos:
                decision(
                    db,
                    at,
                    symbol,
                    "HOLD",
                    "Trend intact; standing stop and target active",
                    evidence,
                )
            elif at < state["cooldown"].get(symbol, 0):
                decision(db, at, symbol, "WAIT", "24-hour cooldown after exit", evidence)
            elif qualifies:
                state["pending"][symbol] = {"at": at, "side": "BUY"}
                decision(
                    db,
                    at,
                    symbol,
                    "BUY",
                    "20h mean > 50h mean and 24h return > 1%; queue only",
                    evidence,
                )
            else:
                decision(
                    db,
                    at,
                    symbol,
                    "WAIT",
                    "Trend filter not satisfied; cash is a position",
                    evidence,
                )
        equity = value(state, quotes)
        state["peak"] = max(state["peak"], equity)
        drawdown = max(0, 1 - equity / state["peak"])
        state["max_drawdown"] = max(state["max_drawdown"], drawdown)
        state["last_at"] = at
        state["health"] = {
            "status": "ok" if mode == "live" else "replay",
            "at": at,
            "message": "Fresh public Kraken observations"
            if mode == "live"
            else "Historical replay scenario; not live trading performance",
        }
        for symbol, pos in state["positions"].items():
            pos["mark"] = quotes[symbol]["bid"]
            pos["market_value"] = pos["quantity"] * sale_price(quotes[symbol]) * (1 - FEE)
            pos["unrealized_pnl"] = pos["market_value"] - pos["cost"]
        benchmark = state["benchmark_quantity"] * sale_price(quotes["BTC"]) * (1 - FEE)
        db.execute(
            "INSERT INTO equity(at,equity,cash,benchmark,drawdown,planned_risk) "
            "VALUES(?,?,?,?,?,?)",
            (at, equity, state["cash"], benchmark, drawdown, planned_risk(state)),
        )
        ledger.save(db, state)
        db.commit()
    finally:
        db.close()
