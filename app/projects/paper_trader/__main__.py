"""CLI for the live service and explicitly isolated replay evaluations."""

import argparse
import json
from pathlib import Path

from . import engine, ledger, market
from .server import serve


def main():
    """Run a read-only dashboard, one live cycle, or an isolated replay."""
    parser = argparse.ArgumentParser(description="Paper-only $1,000 experiment")
    parser.add_argument("command", choices=["serve", "once", "replay", "status"])
    parser.add_argument("--db", default=str(Path.home() / ".kiss/paper-trader/account.sqlite"))
    parser.add_argument("--host", default="127.0.0.1")
    parser.add_argument("--port", type=int, default=8766)
    parser.add_argument("--no-worker", action="store_true")
    parser.add_argument("--input")
    args = parser.parse_args()
    if args.command == "serve":
        serve(args.db, args.host, args.port, args.no_worker)
    elif args.command == "once":
        ledger.claim_mode(args.db, "live")
        try:
            engine.cycle(args.db, market.observe())
        except Exception as error:
            ledger.failure(args.db, str(error))
            raise
        print(json.dumps(ledger.report(args.db)))
    elif args.command == "replay":
        if not args.input or args.db == parser.get_default("db"):
            parser.error("Replay requires --input and an explicit separate --db")
        snapshots = json.loads(Path(args.input).read_text())
        if not isinstance(snapshots, list):
            parser.error("Replay input must be a JSON array of observations")
        ledger.claim_mode(args.db, "replay")
        for snapshot in snapshots:
            try:
                engine.cycle(args.db, snapshot, mode="replay")
            except ValueError as error:
                ledger.failure(args.db, str(error))
        print(json.dumps(ledger.report(args.db)))
    else:
        print(json.dumps(ledger.report(args.db)))


if __name__ == "__main__":
    main()
