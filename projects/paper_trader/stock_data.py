"""Free US exchange directory and unofficial, delayed Yahoo daily price observations."""

import csv
import io
import json
import math
import time
from concurrent.futures import ThreadPoolExecutor
from datetime import datetime
from urllib.parse import quote
from urllib.request import Request, urlopen
from zoneinfo import ZoneInfo

DIRECTORY = "https://www.nasdaqtrader.com/dynamic/SymDir/"
SOURCE = "Nasdaq symbol directory; Yahoo Finance unofficial daily chart endpoint"


def fetch(url):
    """Fetch a public document with a timeout and an identifiable user agent."""
    request = Request(url, headers={"User-Agent": "Mozilla/5.0 (paper-trader research)"})
    with urlopen(request, timeout=20) as response:
        return response.read().decode("utf-8")


def universe():
    """Return all non-test Nasdaq/other-listed US issues, including ETFs and ADRs."""
    result = {}
    for filename, key in (("nasdaqlisted.txt", "Symbol"), ("otherlisted.txt", "ACT Symbol")):
        rows = csv.DictReader(io.StringIO(fetch(DIRECTORY + filename)), delimiter="|")
        if not rows.fieldnames or key not in rows.fieldnames:
            raise ValueError("Invalid Nasdaq directory header")
        for row in rows:
            symbol = row.get(key, "")
            if not symbol or symbol.startswith("File Creation") or row.get("Test Issue") != "N":
                continue
            result[symbol] = {
                "symbol": symbol,
                "name": row["Security Name"],
                "exchange": row.get("Exchange", "Q"),
                "etf": row.get("ETF") == "Y",
            }
    if len(result) < 5000:
        raise ValueError("Incomplete US symbol directory; refusing a truncated universe")
    return result


def history(symbol):
    """Fetch completed daily bars, excluding today's possibly incomplete US session."""
    # Class-share dots map to Yahoo dashes. Other unsupported security spellings
    # remain visible in coverage failures instead of being silently removed.
    ticker = quote(symbol.replace(".", "-"), safe="")
    payload = json.loads(
        fetch(
            f"https://query1.finance.yahoo.com/v8/finance/chart/{ticker}"
            "?range=3mo&interval=1d&events=div%2Csplits"
        )
    )
    chart = payload["chart"]
    if chart.get("error") or not chart.get("result"):
        raise ValueError("No daily price history")
    result = chart["result"][0]
    if result["meta"].get("currency") != "USD":
        raise ValueError("Non-USD security is not eligible")
    today = datetime.now(ZoneInfo("America/New_York")).date()
    prices = result["indicators"]["quote"][0]
    bars = []
    for index, at in enumerate(result.get("timestamp", [])):
        if datetime.fromtimestamp(at, ZoneInfo("America/New_York")).date() >= today:
            continue
        opening, close, volume = (prices[key][index] for key in ("open", "close", "volume"))
        if any(value is None or not math.isfinite(value) for value in (opening, close, volume)):
            continue
        if opening <= 0 or close <= 0 or volume < 0:
            continue
        bars.append({"at": at, "open": opening, "close": close, "volume": volume})
    if len(bars) < 50:
        raise ValueError("Fewer than 50 completed daily bars")
    actions = []
    for kind, records in result.get("events", {}).items():
        for item in records.values():
            actions.append({"kind": kind, **item})
    return {"bars": sorted(bars, key=lambda bar: bar["at"]), "actions": actions}


def observe_symbol(symbol):
    """Return either one actual price observation or an explicit provider failure."""
    try:
        return symbol, history(symbol), None
    except Exception as error:
        return symbol, None, str(error)


def observe(state, batch_size=800):
    """Scan a rotating slice of the entire directory, always prioritizing account symbols."""
    listing = universe()
    symbols = sorted(listing)
    cursor = state.get("scan_cursor", 0) % len(symbols)
    count = min(batch_size, len(symbols))
    selected = {symbols[(cursor + offset) % len(symbols)] for offset in range(count)}
    selected.update(state["positions"])
    selected.update(state["pending"])
    selected.add("SPY")
    quotes, errors = {}, {}
    with ThreadPoolExecutor(max_workers=4) as pool:
        for symbol, result, error in pool.map(observe_symbol, sorted(selected)):
            if error:
                errors[symbol] = error
            else:
                quotes[symbol] = result
    return {
        "at": time.time(),
        "universe": list(listing.values()),
        "quotes": quotes,
        "errors": errors,
        "next_cursor": (cursor + count) % len(symbols),
        "coverage": {
            "universe_count": len(symbols),
            "attempted": len(selected),
            "succeeded": len(quotes),
            "failed": len(errors),
            "remaining": len(set(symbols) - selected),
            "batch_size": count,
            "scan_start_index": cursor,
            "rotation_cycles": math.ceil(len(symbols) / count),
            "scope": "US exchange-listed issues; not worldwide or OTC",
            "method": "Rotating alphabetical scan; positions, pending orders and SPY every run",
        },
    }
