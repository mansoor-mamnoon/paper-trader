"""Validate and document the one-off November 12–13 account replay."""
import argparse
import datetime as dt
import json
import sqlite3
from pathlib import Path

MODEL = "deepseek-ai/deepseek-v4.1-flash"


def validate(path, day, previous=None):
    db = sqlite3.connect(f"file:{Path(path).resolve()}?mode=ro", uri=True)
    assert db.execute("PRAGMA integrity_check").fetchone()[0] == "ok"
    state = json.loads(db.execute("SELECT body FROM state WHERE id=1").fetchone()[0])
    row = db.execute("SELECT body FROM reports WHERE day=?", (day,)).fetchone()
    assert row, f"Missing completed report for {day}"
    report = json.loads(row[0])
    assert report["day"] == day
    assert not db.execute("SELECT 1 FROM reports WHERE day>?", (day,)).fetchone(), "Future report leaked into replay"
    calls = db.execute("SELECT agent,model,ok FROM calls WHERE day=?", (day,)).fetchall()
    for role in ["scout", "strategist", "analyst", "skeptic", "manager"]:
        assert any(a == role and m == MODEL and ok for a, m, ok in calls), f"No successful DeepSeek {role} call"
    decisions = report["decisions"]
    holdings = set(state["positions"])
    reviewed = {d["symbol"] for d in decisions if d["kind"] in ["KEEP", "SELL"]}
    assert holdings <= reviewed, f"Holdings without a manager decision: {holdings - reviewed}"
    if day == "2026-11-12":
        assert any(d["kind"] == "BUY" for d in decisions), "Thursday replay produced no purchases"
        assert any(d["kind"] == "SELL" for d in decisions), "Thursday replay produced no sales"
    fills = db.execute("SELECT at,signal_at,symbol,side,quantity,price FROM fills ORDER BY id").fetchall()
    new_fills = []
    if previous:
        before = sqlite3.connect(f"file:{Path(previous).resolve()}?mode=ro", uri=True)
        old_fills = before.execute("SELECT at,signal_at,symbol,side,quantity,price FROM fills ORDER BY id").fetchall()
        assert fills[:len(old_fills)] == old_fills, "Earlier fills changed"
        new_fills = fills[len(old_fills):]
        assert new_fills, "Thursday orders did not fill on Friday"
        queued = json.loads(before.execute("SELECT body FROM state WHERE id=1").fetchone()[0])["pending"]
        for at, signal_at, symbol, side, _, _ in new_fills:
            assert dt.datetime.fromtimestamp(at, dt.timezone.utc).date().isoformat() == day
            assert symbol in queued and queued[symbol]["side"] == side, "Friday fill was not a Thursday order"
            assert abs(signal_at - queued[symbol]["at"]) < 0.001
        assert any(f[3] == "BUY" for f in new_fills), "No Friday purchases filled"
        assert any(f[3] == "SELL" for f in new_fills), "No Thursday holdings sold on Friday"
    return {
        "day": day,
        "cutoff": report["at"],
        "holdings": sorted(holdings),
        "pending": {s: o["side"] for s, o in state["pending"].items()},
        "decisions": decisions,
        "new_fills": [dict(zip(["at", "signal_at", "symbol", "side", "quantity", "price"], f)) for f in new_fills],
        "problems": report.get("problems", []),
        "calls": len(calls),
        "deepseek_calls": sum(m == MODEL and bool(ok) for _, m, ok in calls),
    }


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("database")
    parser.add_argument("day", choices=["2026-11-12", "2026-11-13"])
    parser.add_argument("--previous")
    parser.add_argument("--output", required=True)
    args = parser.parse_args()
    summary = validate(args.database, args.day, args.previous)
    Path(args.output).write_text(json.dumps(summary, indent=2) + "\n")
    print(json.dumps(summary, indent=2))
