"""Transactional SQLite account and append-only trading evidence."""

import json
import sqlite3
import time
from pathlib import Path


def connect(path):
    """Open a durable ledger with bounded lock waits and explicit transactions."""
    Path(path).parent.mkdir(parents=True, exist_ok=True)
    db = sqlite3.connect(path, timeout=30)
    db.row_factory = sqlite3.Row
    db.execute("PRAGMA journal_mode=WAL")
    db.execute("PRAGMA synchronous=FULL")
    db.executescript("""
        CREATE TABLE IF NOT EXISTS state (id INTEGER PRIMARY KEY CHECK(id=1), body TEXT NOT NULL);
        CREATE TABLE IF NOT EXISTS fills (
            id INTEGER PRIMARY KEY, at REAL, signal_at REAL, symbol TEXT, side TEXT,
            quantity REAL, price REAL, notional REAL, fee REAL, realized_pnl REAL, reason TEXT);
        CREATE TABLE IF NOT EXISTS decisions (
            id INTEGER PRIMARY KEY, at REAL, symbol TEXT, action TEXT, reason TEXT, evidence TEXT);
        CREATE TABLE IF NOT EXISTS equity (
            id INTEGER PRIMARY KEY, at REAL UNIQUE, equity REAL, cash REAL,
            benchmark REAL, drawdown REAL, planned_risk REAL);
        CREATE TABLE IF NOT EXISTS events (
            id INTEGER PRIMARY KEY, at REAL, kind TEXT, message TEXT);
    """)
    initial = {
        "mode": None,
        "cash": 1000.0,
        "positions": {},
        "pending": {},
        "cooldown": {},
        "started_at": None,
        "last_at": 0,
        "peak": 1000.0,
        "max_drawdown": 0,
        "halted": False,
        "halt_reason": "",
        "benchmark_quantity": 0,
        "realized_pnl": 0.0,
        "fees": 0.0,
        "health": {"status": "starting"},
    }
    db.execute("INSERT OR IGNORE INTO state VALUES (1, ?)", (json.dumps(initial),))
    db.commit()
    return db


def load(db):
    """Read the account state inside the caller's transaction."""
    return json.loads(db.execute("SELECT body FROM state WHERE id=1").fetchone()[0])


def save(db, state):
    """Persist the account atomically with the caller's ledger entries."""
    db.execute("UPDATE state SET body=? WHERE id=1", (json.dumps(state, allow_nan=False),))


def event(db, at, kind, message):
    """Append an operational event without altering past entries."""
    db.execute("INSERT INTO events(at,kind,message) VALUES(?,?,?)", (at, kind, message))


def claim_mode(path, mode):
    """Reserve a database for one data source before recording any failures."""
    if mode not in ("live", "replay"):
        raise ValueError("Unknown account mode")
    db = connect(path)
    try:
        db.execute("BEGIN IMMEDIATE")
        state = load(db)
        if state["mode"] not in (None, mode):
            raise ValueError("Live and replay data must use separate databases")
        if state["mode"] is None:
            state["mode"] = mode
            save(db, state)
        db.commit()
    finally:
        db.close()


def failure(path, message):
    """Record a failed cycle while preserving cash, positions and valuation."""
    with connect(path) as db:
        db.execute("BEGIN IMMEDIATE")
        state = load(db)
        state["health"] = {"status": "error", "at": time.time(), "message": message}
        event(db, time.time(), "error", message)
        save(db, state)
    db.close()


def report(path):
    """Return a consistent read-only dashboard view and lifetime performance."""
    db = connect(path)
    try:
        db.execute("BEGIN")
        state = load(db)
        history = [dict(row) for row in db.execute("SELECT * FROM equity ORDER BY id")]
        latest = (
            history[-1]
            if history
            else {
                "equity": 1000,
                "cash": 1000,
                "benchmark": 1000,
                "drawdown": 0,
                "planned_risk": 0,
            }
        )
        account = {
            key: state[key]
            for key in [
                "cash",
                "started_at",
                "last_at",
                "peak",
                "max_drawdown",
                "halted",
                "halt_reason",
                "realized_pnl",
                "fees",
                "mode",
            ]
        }
        account.update(latest)
        account["return_pct"] = (latest["equity"] / 1000 - 1) * 100
        account["benchmark_return_pct"] = (latest["benchmark"] / 1000 - 1) * 100
        account["unrealized_pnl"] = latest["equity"] - 1000 - state["realized_pnl"]
        account["end_at"] = (state["started_at"] or time.time()) + 60 * 86400
        health = dict(state["health"])
        health["age_seconds"] = max(0, time.time() - state["last_at"])
        if health["status"] == "ok" and health["age_seconds"] > 900:
            health["status"] = "stale"
        step = max(1, len(history) // 600)
        sampled = history[::step]
        if history and sampled[-1] != history[-1]:
            sampled.append(history[-1])
        return {
            "account": account,
            "positions": list(state["positions"].values()),
            "pending": state["pending"],
            "health": health,
            "history": sampled,
            "fills": [
                dict(r) for r in db.execute("SELECT * FROM fills ORDER BY id DESC LIMIT 200")
            ],
            "decisions": [
                dict(r) for r in db.execute("SELECT * FROM decisions ORDER BY id DESC LIMIT 120")
            ],
            "events": [
                dict(r) for r in db.execute("SELECT * FROM events ORDER BY id DESC LIMIT 30")
            ],
            "rules": {
                "starting_cash": 1000,
                "position_cap": 0.25,
                "trade_risk": 0.01,
                "aggregate_risk": 0.03,
                "drawdown_halt": 0.10,
                "fee": 0.004,
                "slippage": 0.001,
                "stop": 0.04,
                "take_profit": 0.08,
                "cycle_seconds": 300,
                "duration_days": 60,
            },
        }
    finally:
        db.close()
