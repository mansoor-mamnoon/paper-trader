"""Run one persistent US-stock paper cycle and export a static dashboard data snapshot."""

import argparse
import csv
import json
import os
import time
from pathlib import Path

from . import ledger, stock_account, stock_data


def export(path, output):
    """Export compatible state JSON, full CSV logs and universe/coverage metadata."""
    root = Path(output)
    (root / "api").mkdir(parents=True, exist_ok=True)
    (root / "export").mkdir(exist_ok=True)
    report = ledger.report(path)
    db = ledger.connect(path)
    try:
        state = ledger.load(db)
        report.update(
            {
                "coverage": state.get("coverage", {}),
                "data_errors": state.get("data_errors", {}),
                "source": stock_data.SOURCE,
                "benchmark_label": "SPY price-only (dividends excluded)",
                "simulation": (
                    "Prior completed daily bars; latest completed session open after signal; "
                    "missed runs skip sessions; not real-time execution"
                ),
            }
        )
        report["rules"].update(
            position_cap=0.20,
            fee=stock_account.FEE,
            slippage=stock_account.SLIP,
            cycle_seconds=86400,
        )
        report["health"]["age_seconds"] = max(0, time.time() - state["last_at"])
        body = json.dumps(report, allow_nan=False, indent=2)
        for name in ("state", "state.json"):
            target = root / "api" / name
            temporary = target.with_suffix(".tmp")
            temporary.write_text(body)
            os.replace(temporary, target)
        (root / "api/universe.json").write_text(json.dumps(state.get("universe", [])))
        for table in ("fills", "decisions", "equity", "events"):
            cursor = db.execute("SELECT * FROM " + table + " ORDER BY id")
            with (root / "export" / (table + ".csv")).open("w", newline="") as stream:
                writer = csv.writer(stream)
                writer.writerow([column[0] for column in cursor.description])
                writer.writerows(cursor)
    finally:
        db.close()
    return report


def main():
    """Execute one daily observation, or an explicitly isolated replay, and publish JSON."""
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--db", required=True, help="Persistent SQLite STOCK account; never a crypto DB"
    )
    parser.add_argument("--output", required=True, help="Static output directory")
    parser.add_argument(
        "--batch-size",
        type=int,
        default=800,
        help="Rotating universe slice; use 20000 for entire directory (many requests)",
    )
    parser.add_argument(
        "--replay", type=Path, help="Explicit observation JSON; separate replay account"
    )
    args = parser.parse_args()
    if args.batch_size < 1:
        parser.error("--batch-size must be positive")
    mode = "stocks-replay" if args.replay else "stocks-daily"
    stock_account.reserve(args.db, mode)
    try:
        if args.replay:
            observation = json.loads(args.replay.read_text())
        else:
            db = ledger.connect(args.db)
            try:
                state = ledger.load(db)
            finally:
                db.close()
            observation = stock_data.observe(state, args.batch_size)
        stock_account.cycle(args.db, observation, mode)
    except Exception as error:
        ledger.failure(args.db, str(error))
        export(args.db, args.output)
        raise
    report = export(args.db, args.output)
    print(
        json.dumps(
            {
                "equity": report["account"]["equity"],
                "coverage": report["coverage"],
                "health": report["health"],
                "output": str(Path(args.output) / "api/state"),
            }
        )
    )


if __name__ == "__main__":
    main()
