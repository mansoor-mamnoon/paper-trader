"""Public Kraken observations. No authentication or private API endpoints."""

import json
import math
import time
import urllib.request

PAIRS = {"BTC": "XBTUSD", "ETH": "ETHUSD", "SOL": "SOLUSD", "ADA": "ADAUSD"}
KEYS = {"BTC": "XXBTZUSD", "ETH": "XETHZUSD", "SOL": "SOLUSD", "ADA": "ADAUSD"}


def public_get(endpoint):
    """Fetch one public endpoint, rejecting exchange errors."""
    request = urllib.request.Request(
        "https://api.kraken.com/0/public/" + endpoint,
        headers={"User-Agent": "PaperTradingExperiment/1.0", "Cache-Control": "no-cache"},
    )
    with urllib.request.urlopen(request, timeout=15) as response:
        payload = json.load(response)
    if payload["error"]:
        raise ValueError("Kraken: " + "; ".join(payload["error"]))
    return payload["result"]


def observe():
    """Collect fresh bid/ask, recent trade timestamps and closed hourly candles."""
    server_at = public_get("Time")["unixtime"]
    if abs(time.time() - server_at) > 90:
        raise ValueError("Exchange clock is stale or local clock is incorrect")
    quotes = {}
    for symbol, pair in PAIRS.items():
        bars = public_get("OHLC?pair=" + pair + "&interval=60")[KEYS[symbol]]
        trades = public_get("Trades?pair=" + pair + "&count=1")[KEYS[symbol]]
        # Kraken always returns the unfinished current candle last. Discard it.
        closed = bars[:-1][-60:]
        if len(closed) < 50 or any(
            int(right[0]) - int(left[0]) != 3600 for left, right in zip(closed, closed[1:])
        ):
            raise ValueError(symbol + ": missing hourly candles")
        quotes[symbol] = {
            "trade_at": float(trades[-1][2]),
            "closes": [float(row[4]) for row in closed[-60:]],
            "candle_end": int(closed[-1][0]) + 3600,
        }
    tickers = public_get("Ticker?pair=" + ",".join(PAIRS.values()))
    for symbol, quote in quotes.items():
        ticker = tickers[KEYS[symbol]]
        quote["bid"] = float(ticker["b"][0])
        quote["ask"] = float(ticker["a"][0])
    snapshot = {"at": time.time(), "quotes": quotes}
    validate(snapshot)
    return snapshot


def validate(snapshot):
    """Reject malformed, incomplete, stale or nonfinite market observations."""
    try:
        validate_values(snapshot)
    except (KeyError, TypeError, IndexError, OverflowError) as error:
        raise ValueError("Malformed market observation") from error


def validate_values(snapshot):
    """Validate the observation's numeric fields and closed-candle freshness."""
    at = snapshot["at"]
    if not math.isfinite(at) or at <= 0 or set(snapshot["quotes"]) != set(PAIRS):
        raise ValueError("Invalid observation time or incomplete universe")
    for symbol, quote in snapshot["quotes"].items():
        prices = [quote["bid"], quote["ask"], *quote["closes"]]
        if any(not math.isfinite(value) or value <= 0 for value in prices):
            raise ValueError(symbol + ": invalid price")
        if quote["bid"] > quote["ask"] or quote["ask"] / quote["bid"] > 1.01:
            raise ValueError(symbol + ": crossed or wider than 1% spread")
        age = at - quote["trade_at"]
        candle_age = at - quote["candle_end"]
        if not 0 <= age <= 180 or not 0 <= candle_age <= 3900:
            raise ValueError(symbol + ": stale or future market data")
        if len(quote["closes"]) < 50:
            raise ValueError(symbol + ": fewer than 50 closed candles")


def signal(quote):
    """Require 20-hour mean above 50-hour mean and positive 24-hour momentum."""
    closes = quote["closes"]
    fast = sum(closes[-20:]) / 20
    slow = sum(closes[-50:]) / 50
    momentum = closes[-1] / closes[-25] - 1
    return fast > slow and momentum > 0.01, {
        "sma20": fast,
        "sma50": slow,
        "momentum24h": momentum,
        "candle_end": quote["candle_end"],
    }
