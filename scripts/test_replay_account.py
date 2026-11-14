"""Checks that replay promotion requires an intact chronological trade chain."""
import json
import sqlite3
import tempfile
import unittest
from pathlib import Path
from replay_account import MODEL, validate


class ReplayValidationTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.thursday = Path(self.temp.name) / "thursday.sqlite"
        self.friday = Path(self.temp.name) / "friday.sqlite"
        self.signal = 1794521263.379
        self.opening = 1794580200.0
        self.create(self.thursday, "2026-11-12", {"OLD": {}, "KEEP": {}}, {
            "OLD": {"side": "SELL", "at": self.signal},
            "NEW": {"side": "BUY", "at": self.signal},
        }, [{"symbol": "OLD", "kind": "SELL"}, {"symbol": "KEEP", "kind": "KEEP"}, {"symbol": "NEW", "kind": "BUY"}])
        self.create(self.friday, "2026-11-13", {"NEW": {}, "KEEP": {}}, {}, [
            {"symbol": "NEW", "kind": "KEEP"}, {"symbol": "KEEP", "kind": "KEEP"},
        ])
        with sqlite3.connect(self.friday) as db:
            db.executemany("INSERT INTO fills VALUES(?,?,?,?,?,?,?)", [
                (1, self.opening, self.signal, "OLD", "SELL", 1, 50),
                (2, self.opening, self.signal, "NEW", "BUY", 1, 49),
            ])

    def create(self, path, day, positions, pending, decisions):
        with sqlite3.connect(path) as db:
            db.executescript("""
                CREATE TABLE state(id INTEGER, body TEXT);
                CREATE TABLE reports(day TEXT, body TEXT);
                CREATE TABLE calls(day TEXT, agent TEXT, model TEXT, ok INTEGER);
                CREATE TABLE fills(id INTEGER, at REAL, signal_at REAL, symbol TEXT, side TEXT, quantity REAL, price REAL);
            """)
            db.execute("INSERT INTO state VALUES(1,?)", (json.dumps({"positions": positions, "pending": pending}),))
            db.execute("INSERT INTO reports VALUES(?,?)", (day, json.dumps({"day": day, "at": self.signal, "decisions": decisions})))
            for role in ["scout", "strategist", "analyst", "skeptic", "manager"]:
                db.execute("INSERT INTO calls VALUES(?,?,?,1)", (day, role, MODEL))

    def test_friday_can_keep_everything_after_filling_thursday_orders(self):
        validate(self.thursday, "2026-11-12")
        result = validate(self.friday, "2026-11-13", self.thursday)
        self.assertEqual(len(result["new_fills"]), 2)

    def test_a_fill_not_ordered_on_thursday_blocks_promotion(self):
        with sqlite3.connect(self.friday) as db:
            db.execute("UPDATE fills SET symbol='UNORDERED' WHERE side='BUY'")
        with self.assertRaisesRegex(AssertionError, "not a Thursday order"):
            validate(self.friday, "2026-11-13", self.thursday)

    def test_a_future_report_blocks_a_thursday_replay(self):
        with sqlite3.connect(self.thursday) as db:
            db.execute("INSERT INTO reports VALUES('2026-11-13','{}')")
        with self.assertRaisesRegex(AssertionError, "Future report"):
            validate(self.thursday, "2026-11-12")

    def test_a_missing_deepseek_manager_blocks_promotion(self):
        with sqlite3.connect(self.friday) as db:
            db.execute("DELETE FROM calls WHERE agent='manager'")
        with self.assertRaisesRegex(AssertionError, "DeepSeek manager"):
            validate(self.friday, "2026-11-13", self.thursday)


if __name__ == "__main__":
    unittest.main()
