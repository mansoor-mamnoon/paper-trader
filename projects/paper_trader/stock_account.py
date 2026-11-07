"""Isolated stock paper ledger: whole shares, delayed daily fills and explicit evidence."""

import json
import math

from . import ledger

FEE = 0.001
SLIP = 0.002
DAY = 86400


def reserve(path, mode):
    """Reserve an isolated stock account; never reuse a crypto or replay account."""
    db = ledger.connect(path)
    try:
        db.execute("BEGIN IMMEDIATE")
        state = ledger.load(db)
        if state["mode"] not in (None, mode):
            raise ValueError("Stock live/replay and crypto accounts require separate databases")
        state["mode"] = mode
        ledger.save(db, state)
        db.commit()
    finally:
        db.close()


def decision(db, at, symbol, action, reason, evidence=None):
    """Append the exact decision and information available at observation time."""
    db.execute(
        "INSERT INTO decisions(at,symbol,action,reason,evidence) VALUES(?,?,?,?,?)",
        (at, symbol, action, reason, json.dumps(evidence or {}, allow_nan=False)),
    )


def equity(state):
    """Value cash plus last available conservative position marks."""
    return state["cash"] + sum(p["market_value"] for p in state["positions"].values())


def corporate_actions(db, state, symbol, quote, at):
    """Adjust held quantities for splits and credit reported cash distributions once."""
    position = state["positions"].get(symbol)
    if not position:
        return
    processed = state.setdefault("corporate_actions", [])
    for action in sorted(quote.get("actions", []), key=lambda row: row["date"]):
        key = f"{symbol}:{action['kind']}:{action['date']}"
        if key in processed or not position["opened_at"] < action["date"] <= at:
            continue
        if action["kind"] == "splits":
            ratio = action["numerator"] / action["denominator"]
            if not math.isfinite(ratio) or ratio <= 0:
                raise ValueError("Invalid split ratio")
            position["quantity"] *= ratio
            for field in ("entry", "stop", "target"):
                position[field] /= ratio
            reason = f"Reported split applied, ratio {ratio}"
        elif action["kind"] == "dividends":
            amount = position["quantity"] * action["amount"]
            if not math.isfinite(amount) or amount < 0:
                raise ValueError("Invalid cash distribution")
            state["cash"] += amount
            state["realized_pnl"] += amount
            reason = f"Reported distribution credited on ex-date, not payment date: ${amount:.4f}"
        else:
            continue
        processed.append(key)
        decision(db, at, symbol, "CORPORATE_ACTION", reason, action)


def fill(db, state, symbol, order, bar, at):
    """Execute a prior decision at a later completed bar open, with adverse costs."""
    side = order["side"]
    price = bar["open"] * (1 + SLIP if side == "BUY" else 1 - SLIP)
    if side == "BUY":
        capital = equity(state)
        quantity = math.floor(min(state["cash"], capital * 0.20) / (price * (1 + FEE)))
        if state["halted"] or len(state["positions"]) >= 3 or quantity < 1:
            decision(db, at, symbol, "REJECT", "Halted, three-position limit, or whole share unaffordable")
            return
        cost = quantity * price * (1 + FEE)
        state["cash"] -= cost
        state["positions"][symbol] = {
            "symbol": symbol, "quantity": quantity, "entry": price, "cost": cost,
            "stop": price * 0.96, "target": price * 1.08, "opened_at": bar["at"],
            "planned_risk": cost - quantity * price * 0.96 * (1 - SLIP) * (1 - FEE),
            "market_value": quantity * price * (1 - SLIP) * (1 - FEE),
            "unrealized_pnl": 0,
        }
        pnl = 0
    else:
        position = state["positions"].pop(symbol)
        quantity = position["quantity"]
        proceeds = quantity * price * (1 - FEE)
        pnl = proceeds - position["cost"]
        state["cash"] += proceeds
        state["realized_pnl"] += pnl
        state["cooldown"][symbol] = at + 5 * DAY
    fee = quantity * price * FEE
    state["fees"] += fee
    db.execute(
        "INSERT INTO fills(at,signal_at,symbol,side,quantity,price,notional,fee,"
        "realized_pnl,reason) VALUES(?,?,?,?,?,?,?,?,?,?)",
        (bar["at"], order["at"], symbol, side, quantity, price, quantity * price,
         fee, pnl, order["reason"]),
    )
    decision(db, at, symbol, side + "_FILLED",
             "Daily simulation: latest completed session open after signal, plus adverse costs; "
             "missed runs skip sessions",
             {"bar_at": bar["at"], "price": price, "signal_at": order["at"]})


def cycle(path, observation, mode="stocks-daily"):
    """Apply one recorded observation atomically; retries cannot duplicate orders or fills."""
    reserve(path, mode)
    at = observation["at"]
    if not math.isfinite(at) or at <= 0:
        raise ValueError("Invalid observation timestamp")
    db = ledger.connect(path)
    try:
        db.execute("BEGIN IMMEDIATE")
        state = ledger.load(db)
        if at <= state["last_at"]:
            db.rollback()
            return
        quotes = observation["quotes"]
        usable = {}
        for symbol, quote in quotes.items():
            bars = quote["bars"]
            if len(bars) < 50 or any(
                not all(math.isfinite(bar[k]) for k in ("at", "open", "close", "volume"))
                or min(bar["open"], bar["close"]) <= 0 or bar["volume"] < 0
                or bar["at"] >= at for bar in bars
            ) or any(a["at"] >= b["at"] for a, b in zip(bars, bars[1:])):
                raise ValueError("Invalid daily bars")
            if at - bars[-1]["at"] > 5 * DAY:
                decision(db, at, symbol, "SKIP", "Daily bar older than five days")
                continue
            usable[symbol] = quote
        if state["started_at"] is None:
            state["started_at"] = at
        if at - state["started_at"] >= 60 * DAY:
            state["halted"], state["halt_reason"] = True, "60-day experiment ended"
        for symbol, quote in usable.items():
            corporate_actions(db, state, symbol, quote, quote["bars"][-1]["at"])
        # Missing held marks freeze entries; absent/delisted stocks are not valued at zero.
        missing = set(state["positions"]) - set(usable)
        if missing:
            decision(db, at, "ACCOUNT", "FREEZE", "Held symbols unavailable; retained last marks",
                     {"symbols": sorted(missing)})
        for symbol, order in list(state["pending"].items()):
            if symbol not in usable:
                continue
            eligible = [bar for bar in usable[symbol]["bars"] if bar["at"] > order["at"]]
            if not eligible:
                continue
            del state["pending"][symbol]
            if at - order["at"] > 7 * DAY or (order["side"] == "BUY" and missing):
                decision(db, at, symbol, "EXPIRED", "Order stale or held prices unavailable")
                continue
            fill(db, state, symbol, order, eligible[-1], at)
        for symbol, position in state["positions"].items():
            if symbol not in usable:
                continue
            close = usable[symbol]["bars"][-1]["close"]
            position["market_value"] = position["quantity"] * close * (1 - SLIP) * (1 - FEE)
            position["unrealized_pnl"] = position["market_value"] - position["cost"]
            position["price_at"] = usable[symbol]["bars"][-1]["at"]
        total = equity(state)
        state["peak"] = max(state["peak"], total)
        drawdown = 1 - total / state["peak"]
        state["max_drawdown"] = max(state["max_drawdown"], drawdown)
        if drawdown >= 0.10:
            state["halted"], state["halt_reason"] = True, "10% drawdown halt (gaps can exceed limits)"
        for symbol, position in state["positions"].items():
            if symbol in usable and symbol not in state["pending"]:
                close = usable[symbol]["bars"][-1]["close"]
                if state["halted"] or close <= position["stop"] or close >= position["target"]:
                    state["pending"][symbol] = {"side": "SELL", "at": at,
                        "reason": "Close crossed stop/target or account halted; next-session open"}
                    decision(db, at, symbol, "SELL_SIGNAL", state["pending"][symbol]["reason"])
        candidates = []
        for symbol, quote in usable.items():
            bars = quote["bars"]
            close = bars[-1]["close"]
            sma20 = sum(bar["close"] for bar in bars[-20:]) / 20
            sma50 = sum(bar["close"] for bar in bars[-50:]) / 50
            momentum = close / bars[-21]["close"] - 1
            dollar_volume = sum(bar["close"] * bar["volume"] for bar in bars[-20:]) / 20
            evidence = {"sma20": sma20, "sma50": sma50, "momentum20d": momentum,
                        "average_dollar_volume": dollar_volume, "price_at": bars[-1]["at"]}
            eligible = close >= 5 and dollar_volume >= 1000000 and close > sma20 > sma50 and momentum > .02
            decision(db, at, symbol, "CANDIDATE" if eligible else "PASS",
                     "20/50-day trend, 20-day momentum, price >= $5, liquidity >= $1M/day", evidence)
            if eligible:
                candidates.append((momentum, symbol))
        slots = 3 - len(state["positions"]) - sum(o["side"] == "BUY" for o in state["pending"].values())
        if not state["halted"] and not missing:
            for _, symbol in sorted(candidates, reverse=True):
                if slots <= 0:
                    break
                if symbol in state["positions"] or symbol in state["pending"] or state["cooldown"].get(symbol, 0) > at:
                    continue
                state["pending"][symbol] = {"side": "BUY", "at": at,
                    "reason": "Positive liquid daily trend; whole shares capped at 20% of equity"}
                decision(db, at, symbol, "BUY_SIGNAL", state["pending"][symbol]["reason"])
                slots -= 1
        benchmark = 1000.0
        if "SPY" in usable:
            spy = usable["SPY"]["bars"][-1]["close"]
            if not state["benchmark_quantity"]:
                state["benchmark_quantity"] = 1000 / spy
            benchmark = state["benchmark_quantity"] * spy
        elif state.get("benchmark_value"):
            benchmark = state["benchmark_value"]
        state["benchmark_value"] = benchmark
        risk = sum(p["planned_risk"] for p in state["positions"].values())
        db.execute("INSERT INTO equity(at,equity,cash,benchmark,drawdown,planned_risk) VALUES(?,?,?,?,?,?)",
                   (at, total, state["cash"], benchmark, drawdown, risk))
        state["last_at"] = at
        state["scan_cursor"] = observation.get("next_cursor", 0)
        state["coverage"] = observation["coverage"]
        state["coverage"]["usable"] = len(usable)
        state["universe"] = observation["universe"]
        state["data_errors"] = observation["errors"]
        state["health"] = {
            "status": "delayed", "at": at,
            "message": "Daily-price simulation, not live quotes. Prior completed sessions only. "
                       + ("Some holdings lack current marks. " if missing else "")
                       + f"{len(usable)} usable prices; {len(observation['errors'])} provider errors.",
        }
        ledger.event(db, at, "stock_scan", json.dumps(state["coverage"]))
        ledger.save(db, state)
        db.commit()
    finally:
        db.close()
