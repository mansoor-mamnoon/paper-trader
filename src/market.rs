//! Free market data: the Nasdaq stock screener for the whole US market in one request, the
//! Nasdaq symbol directory for the list of funds, and Yahoo's daily bars for the stocks the
//! desk looks at closely.

use crate::config::{MIN_DOLLAR_VOLUME, MIN_MARKET_CAP, MIN_PRICE};
use crate::http;
use anyhow::{Result, anyhow, bail};
use chrono::{DateTime, Utc};
use chrono_tz::America::New_York;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::sync::Mutex;
use std::time::Duration;

const SCREENER: &str =
    "https://api.nasdaq.com/api/screener/stocks?tableonly=true&limit=25000&download=true";
const DIRECTORY: &str = "https://www.nasdaqtrader.com/dynamic/SymDir/";

/// One listed stock with its latest session, from the screener.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Listing {
    pub symbol: String,
    pub name: String,
    pub price: f64,
    /// The latest session's change in percent.
    pub change: f64,
    pub volume: f64,
    pub market_cap: f64,
    pub sector: String,
    pub industry: String,
}

impl Listing {
    pub fn dollar_volume(&self) -> f64 {
        self.price * self.volume
    }

    /// The company name without its share-class description.
    pub fn company(&self) -> String {
        company_name(&self.name)
    }
}

fn number(text: &str) -> Option<f64> {
    let cleaned: String = text
        .chars()
        .filter(|c| !matches!(c, '$' | '%' | ',' | ' '))
        .collect();
    cleaned.parse::<f64>().ok().filter(|v| v.is_finite())
}

/// Parse the screener response. Class shares use a dot (`BRK.B`).
pub fn parse_screener(body: &str) -> Vec<Listing> {
    let Ok(value) = serde_json::from_str::<Value>(body) else {
        return Vec::new();
    };
    value["data"]["rows"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|row| {
            let text = |key: &str| row[key].as_str().unwrap_or("").trim().to_string();
            let symbol = text("symbol").replace('/', ".");
            if symbol.is_empty() || symbol.contains('^') {
                return None;
            }
            Some(Listing {
                symbol,
                name: text("name"),
                price: number(&text("lastsale"))?,
                change: number(&text("pctchange")).unwrap_or(0.0),
                volume: number(&text("volume")).unwrap_or(0.0),
                market_cap: number(&text("marketCap")).unwrap_or(0.0),
                sector: text("sector"),
                industry: text("industry"),
            })
        })
        .collect()
}

/// Every US-listed stock with its latest session.
pub fn listings() -> Result<Vec<Listing>> {
    let rows = parse_screener(&http::get_retry(SCREENER, Duration::from_secs(40), 3)?);
    if rows.len() < 3000 {
        bail!(
            "The stock screener returned only {} rows; refusing a truncated market",
            rows.len()
        );
    }
    Ok(rows)
}

/// Symbols the exchange directory flags as exchange-traded funds.
pub fn parse_directory_funds(text: &str, key: &str, into: &mut BTreeSet<String>) {
    let mut lines = text.lines();
    let header: Vec<&str> = lines.next().unwrap_or("").split('|').collect();
    let column = |name: &str| header.iter().position(|h| h.trim() == name);
    let (Some(symbol_at), Some(etf_at)) = (column(key), column("ETF")) else {
        return;
    };
    for line in lines {
        let cells: Vec<&str> = line.split('|').collect();
        if cells.get(etf_at).map(|c| c.trim()) == Some("Y")
            && let Some(symbol) = cells.get(symbol_at)
        {
            into.insert(symbol.trim().to_string());
        }
    }
}

/// Every symbol the exchanges flag as a fund. Empty when the directory cannot be read, in
/// which case the name check and the price provider's instrument type still apply.
pub fn fund_symbols() -> BTreeSet<String> {
    let mut funds = BTreeSet::new();
    for (file, key) in [
        ("nasdaqlisted.txt", "Symbol"),
        ("otherlisted.txt", "ACT Symbol"),
    ] {
        if let Ok(text) = http::get_retry(&format!("{DIRECTORY}{file}"), Duration::from_secs(30), 2)
        {
            parse_directory_funds(&text, key, &mut funds);
        }
    }
    funds
}

const EQUITY_WORDS: [&str; 10] = [
    "common stock",
    "common shares",
    "common share",
    "ordinary shares",
    "ordinary share",
    "capital stock",
    "class a",
    "american depositary",
    "american depository",
    "subordinate voting",
];

const NOT_A_STOCK: [&str; 17] = [
    "etf",
    "etn",
    "fund",
    "warrant",
    "warrants",
    "right",
    "rights",
    "unit",
    "units",
    "preferred",
    "preference",
    "notes",
    "debentures",
    "index",
    "portfolio",
    "2x",
    "3x",
];

/// Whether a registered security name describes a company's own shares. Funds of every kind
/// (index funds, ETFs, ETNs, closed-end funds), preferreds, warrants, units and notes fail.
pub fn is_company_stock(name: &str) -> bool {
    let lower = name.to_lowercase();
    if lower.contains('%') || (lower.contains("depositary shares") && !lower.contains("american")) {
        return false;
    }
    let words: Vec<&str> = lower
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| !w.is_empty())
        .collect();
    if NOT_A_STOCK.iter().any(|bad| words.contains(bad)) {
        return false;
    }
    EQUITY_WORDS.iter().any(|w| lower.contains(w))
        || words.contains(&"ads")
        || words.contains(&"adr")
}

/// The company name: the registered name up to its share-class description.
pub fn company_name(name: &str) -> String {
    let lower = name.to_lowercase();
    let mut cut = name.len();
    for marker in EQUITY_WORDS.iter().copied().chain([
        " - ",
        "class b",
        "class c",
        "(reit)",
        "each representing",
        " ads",
        " adr",
        "sponsored",
    ]) {
        if let Some(at) = lower.find(marker) {
            cut = cut.min(at);
        }
    }
    let trimmed = name[..cut].trim().trim_end_matches([',', '-', '.']).trim();
    if trimmed.is_empty() {
        name.trim().to_string()
    } else {
        trimmed.to_string()
    }
}

/// Why a listing may not be bought, or `None` when it may.
pub fn ineligible(listing: &Listing, funds: &BTreeSet<String>) -> Option<&'static str> {
    if funds.contains(&listing.symbol) {
        return Some("exchange-traded fund");
    }
    if !is_company_stock(&listing.name) {
        return Some("not a company's own shares");
    }
    if listing.market_cap < MIN_MARKET_CAP {
        return Some("market value below the floor");
    }
    if listing.price < MIN_PRICE {
        return Some("price below the floor");
    }
    if listing.dollar_volume() < MIN_DOLLAR_VOLUME {
        return Some("too thinly traded");
    }
    None
}

/// One completed daily bar.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Bar {
    pub at: f64,
    pub open: f64,
    pub close: f64,
    pub volume: f64,
}

/// A split or a dividend reported with the bars.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Action {
    /// `splits` or `dividends`.
    pub kind: String,
    pub date: f64,
    pub amount: Option<f64>,
    pub numerator: Option<f64>,
    pub denominator: Option<f64>,
}

/// Daily price history for one symbol.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Quote {
    pub bars: Vec<Bar>,
    pub actions: Vec<Action>,
    /// The price provider's instrument type, `EQUITY` for a stock.
    pub instrument_type: Option<String>,
}

impl Quote {
    pub fn last(&self) -> Option<&Bar> {
        self.bars.last()
    }

    /// Percent change of the close over the last `sessions` sessions.
    pub fn change(&self, sessions: usize) -> Option<f64> {
        let n = self.bars.len();
        (n > sessions)
            .then(|| (self.bars[n - 1].close / self.bars[n - 1 - sessions].close - 1.0) * 100.0)
    }

    /// The latest session's volume against the average of the 20 sessions before it.
    pub fn relative_volume(&self) -> Option<f64> {
        let n = self.bars.len();
        if n < 21 {
            return None;
        }
        let average = self.bars[n - 21..n - 1]
            .iter()
            .map(|b| b.volume)
            .sum::<f64>()
            / 20.0;
        (average > 0.0).then(|| self.bars[n - 1].volume / average)
    }

    /// How far the close sits below the highest close of the last 60 sessions, in percent.
    pub fn below_high(&self) -> Option<f64> {
        let last = self.last()?.close;
        let high = self
            .bars
            .iter()
            .rev()
            .take(60)
            .map(|b| b.close)
            .fold(0.0, f64::max);
        (high > 0.0).then(|| (1.0 - last / high) * 100.0)
    }

    pub fn is_stock(&self) -> bool {
        self.instrument_type.as_deref() == Some("EQUITY")
    }
}

/// New York wall-clock time after which the day's session counts as completed.
pub const SESSION_SETTLED: (u32, u32) = (16, 30);

/// The first New York date whose session is not yet completed at `now`.
pub fn first_open_day(now: DateTime<Utc>) -> chrono::NaiveDate {
    use chrono::Timelike;
    let local = now.with_timezone(&New_York);
    let today = local.date_naive();
    if (local.hour(), local.minute()) >= SESSION_SETTLED {
        today.succ_opt().unwrap_or(today)
    } else {
        today
    }
}

/// When the most recent weekday session settled (16:30 New York) at or before `now`.
pub fn last_settled(now: DateTime<Utc>) -> DateTime<Utc> {
    use chrono::{Datelike, TimeZone, Weekday};
    let mut day = now.with_timezone(&New_York).date_naive();
    loop {
        let weekday = !matches!(day.weekday(), Weekday::Sat | Weekday::Sun);
        let settled = day
            .and_hms_opt(SESSION_SETTLED.0, SESSION_SETTLED.1, 0)
            .and_then(|t| New_York.from_local_datetime(&t).earliest())
            .map(|t| t.with_timezone(&Utc));
        if let Some(settled) = settled.filter(|t| weekday && *t <= now) {
            return settled;
        }
        day = day.pred_opt().unwrap_or(day);
    }
}

/// The New York calendar date of a timestamp, `YYYY-MM-DD`.
pub fn day_of(at: f64) -> String {
    DateTime::<Utc>::from_timestamp(at as i64, 0)
        .unwrap_or_default()
        .with_timezone(&New_York)
        .date_naive()
        .to_string()
}

/// Turn a Yahoo chart payload into completed daily bars and corporate actions. Bars dated
/// on or after `open_day` are still trading and are dropped.
pub fn parse_history(payload: &Value, open_day: chrono::NaiveDate) -> Result<Quote> {
    let chart = &payload["chart"];
    let result = chart["result"]
        .get(0)
        .filter(|_| chart["error"].is_null())
        .ok_or_else(|| anyhow!("No daily price history"))?;
    if result["meta"]["currency"] != "USD" {
        bail!("Not priced in US dollars");
    }
    let prices = &result["indicators"]["quote"][0];
    let mut bars = Vec::new();
    for (i, stamp) in result["timestamp"]
        .as_array()
        .into_iter()
        .flatten()
        .enumerate()
    {
        let Some(at) = stamp.as_f64() else { continue };
        let day = DateTime::<Utc>::from_timestamp(at as i64, 0)
            .ok_or_else(|| anyhow!("Bad timestamp"))?
            .with_timezone(&New_York)
            .date_naive();
        if day >= open_day {
            continue;
        }
        let pick = |k: &str| prices[k].get(i).and_then(Value::as_f64);
        let (Some(open), Some(close), Some(volume)) = (pick("open"), pick("close"), pick("volume"))
        else {
            continue;
        };
        if !(open.is_finite() && close.is_finite() && volume.is_finite())
            || open <= 0.0
            || close <= 0.0
            || volume < 0.0
        {
            continue;
        }
        bars.push(Bar {
            at,
            open,
            close,
            volume,
        });
    }
    if bars.len() < 25 {
        bail!("Fewer than 25 completed daily bars");
    }
    bars.sort_by(|a, b| a.at.total_cmp(&b.at));
    let mut actions = Vec::new();
    if let Some(events) = result["events"].as_object() {
        for (kind, records) in events {
            for item in records.as_object().into_iter().flat_map(|m| m.values()) {
                let field = |key: &str| item.get(key).and_then(Value::as_f64);
                let Some(date) = field("date") else { continue };
                actions.push(Action {
                    kind: kind.clone(),
                    date,
                    amount: field("amount"),
                    numerator: field("numerator"),
                    denominator: field("denominator"),
                });
            }
        }
    }
    actions.sort_by(|a, b| a.date.total_cmp(&b.date));
    Ok(Quote {
        bars,
        actions,
        instrument_type: result["meta"]["instrumentType"]
            .as_str()
            .map(str::to_string),
    })
}

/// Three months of daily bars for one symbol.
pub fn history(symbol: &str) -> Result<Quote> {
    let ticker = http::encode(&symbol.replace('.', "-"));
    let url = format!(
        "https://query1.finance.yahoo.com/v8/finance/chart/{ticker}?range=3mo&interval=1d&events=div%2Csplits"
    );
    let payload: Value = serde_json::from_str(&http::get_retry(&url, Duration::from_secs(20), 2)?)?;
    parse_history(&payload, first_open_day(Utc::now()))
}

/// Histories for many symbols, four requests at a time. Failures are returned by symbol.
pub fn histories(
    symbols: &BTreeSet<String>,
) -> (BTreeMap<String, Quote>, BTreeMap<String, String>) {
    let queue = Mutex::new(symbols.iter().cloned().collect::<VecDeque<_>>());
    let found = Mutex::new((BTreeMap::new(), BTreeMap::new()));
    std::thread::scope(|scope| {
        for _ in 0..4 {
            scope.spawn(|| {
                loop {
                    let Some(symbol) = queue.lock().unwrap().pop_front() else {
                        break;
                    };
                    let outcome = history(&symbol);
                    let mut guard = found.lock().unwrap();
                    match outcome {
                        Ok(quote) => {
                            guard.0.insert(symbol, quote);
                        }
                        Err(error) => {
                            guard.1.insert(symbol, error.to_string());
                        }
                    }
                }
            });
        }
    });
    found.into_inner().unwrap()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn utc(s: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(s).unwrap().with_timezone(&Utc)
    }

    #[test]
    fn the_screener_row_becomes_a_listing() {
        let body = r#"{"data":{"rows":[
            {"symbol":"NVDA","name":"NVIDIA Corporation Common Stock","lastsale":"$201.50","pctchange":"3.25%","volume":"150000000","marketCap":"4900000000000.00","sector":"Technology","industry":"Semiconductors"},
            {"symbol":"BRK/B","name":"Berkshire Hathaway Inc.","lastsale":"$500.00","pctchange":"","volume":"10","marketCap":"","sector":"","industry":""},
            {"symbol":"BAD","name":"No price","lastsale":"","pctchange":"1%","volume":"1","marketCap":"1","sector":"","industry":""}]}}"#;
        let rows = parse_screener(body);
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].symbol, "NVDA");
        assert!((rows[0].change - 3.25).abs() < 1e-9);
        assert!((rows[0].dollar_volume() - 201.5 * 150_000_000.0).abs() < 1.0);
        assert_eq!(rows[1].symbol, "BRK.B");
        assert_eq!(rows[0].company(), "NVIDIA Corporation");
        assert!(parse_screener("not json").is_empty());
    }

    #[test]
    fn funds_and_other_paper_are_not_company_stock() {
        for name in [
            "NVIDIA Corporation Common Stock",
            "Alphabet Inc. Class A Common Stock",
            "Taiwan Semiconductor Manufacturing Company Ltd. American Depositary Shares",
            "Arm Holdings plc ADS",
            "Accenture plc Class A Ordinary Shares (Ireland)",
        ] {
            assert!(is_company_stock(name), "{name}");
        }
        for name in [
            "SPDR S&P 500 ETF Trust",
            "T-REX 2X Inverse DRAM Daily Target ETF",
            "Invesco QQQ Trust, Series 1",
            "Vanguard Total Stock Market Index Fund ETF Shares",
            "BlackRock Science and Technology Trust Common Shares of Beneficial Interest Fund",
            "Acme Acquisition Corp. Warrant",
            "Acme Acquisition Corp. Units",
            "Bank of America Corporation Depositary Shares Series QQ Preferred",
            "Some Bank 6.50% Senior Notes due 2030",
            "Adams Diversified Equity Fund Inc. Common Stock",
            "Mystery Holdings",
        ] {
            assert!(!is_company_stock(name), "{name}");
        }
    }

    #[test]
    fn eligibility_names_the_first_failed_rule() {
        let good = Listing {
            symbol: "NVDA".into(),
            name: "NVIDIA Corporation Common Stock".into(),
            price: 200.0,
            change: 1.0,
            volume: 1.0e8,
            market_cap: 4.0e12,
            sector: "Technology".into(),
            industry: "Semiconductors".into(),
        };
        let funds: BTreeSet<String> = ["SPY".to_string()].into();
        assert_eq!(ineligible(&good, &funds), None);
        let fund = Listing {
            symbol: "SPY".into(),
            ..good.clone()
        };
        assert_eq!(ineligible(&fund, &funds), Some("exchange-traded fund"));
        let small = Listing {
            market_cap: 5.0e8,
            ..good.clone()
        };
        assert_eq!(
            ineligible(&small, &funds),
            Some("market value below the floor")
        );
        let thin = Listing {
            volume: 1000.0,
            ..good
        };
        assert_eq!(ineligible(&thin, &funds), Some("too thinly traded"));
    }

    #[test]
    fn the_directory_lists_funds() {
        let text = "Symbol|Security Name|Test Issue|ETF\nAAA|Alpha Inc|N|N\nSPY|SPDR|N|Y\nFile Creation Time: 1|||\n";
        let mut funds = BTreeSet::new();
        parse_directory_funds(text, "Symbol", &mut funds);
        assert_eq!(funds.into_iter().collect::<Vec<_>>(), ["SPY"]);
    }

    fn payload(n: usize, last_open: f64) -> Value {
        let stamps: Vec<f64> = (0..n)
            .map(|i| last_open - 86400.0 * (n - 1 - i) as f64)
            .collect();
        let closes: Vec<f64> = (0..n).map(|i| 10.0 + i as f64).collect();
        json!({"chart": {"error": null, "result": [{
            "meta": {"currency": "USD", "instrumentType": "EQUITY"},
            "timestamp": stamps,
            "indicators": {"quote": [{"open": vec![10.0; n], "close": closes, "volume": vec![5.0; n]}]},
            "events": {"splits": {"1": {"date": last_open - 86400.0, "numerator": 2.0, "denominator": 1.0}}}
        }]}})
    }

    #[test]
    fn history_keeps_completed_sessions_only() {
        let monday_open = utc("2026-11-09T14:30:00Z").timestamp() as f64;
        let evening = parse_history(
            &payload(60, monday_open),
            first_open_day(utc("2026-11-09T22:37:00Z")),
        )
        .unwrap();
        assert_eq!(evening.last().unwrap().at, monday_open);
        assert!(evening.is_stock());
        assert_eq!(evening.actions[0].kind, "splits");
        let midday = parse_history(
            &payload(60, monday_open),
            first_open_day(utc("2026-11-09T17:00:00Z")),
        )
        .unwrap();
        assert!(midday.last().unwrap().at < monday_open);
    }

    #[test]
    fn quote_statistics() {
        let monday_open = utc("2026-11-09T14:30:00Z").timestamp() as f64;
        let mut quote = parse_history(
            &payload(60, monday_open),
            first_open_day(utc("2026-11-09T22:37:00Z")),
        )
        .unwrap();
        let change = quote.change(1).unwrap();
        assert!((change - (69.0 / 68.0 - 1.0) * 100.0).abs() < 1e-9);
        assert!((quote.relative_volume().unwrap() - 1.0).abs() < 1e-9);
        assert!(quote.below_high().unwrap().abs() < 1e-9);
        quote.bars.last_mut().unwrap().volume = 15.0;
        assert!((quote.relative_volume().unwrap() - 3.0).abs() < 1e-9);
    }

    #[test]
    fn non_usd_and_short_histories_are_rejected() {
        let day = chrono::NaiveDate::from_ymd_opt(2030, 1, 1).unwrap();
        let eur = json!({"chart": {"error": null, "result": [{"meta": {"currency": "EUR"}}]}});
        assert!(parse_history(&eur, day).is_err());
        let monday_open = utc("2026-11-09T14:30:00Z").timestamp() as f64;
        assert!(parse_history(&payload(10, monday_open), day).is_err());
    }

    #[test]
    fn sessions_settle_at_half_past_four_new_york() {
        let day = |y, m, d| chrono::NaiveDate::from_ymd_opt(y, m, d).unwrap();
        assert_eq!(
            first_open_day(utc("2026-11-09T21:29:00Z")),
            day(2026, 11, 9)
        );
        assert_eq!(
            first_open_day(utc("2026-11-09T21:30:00Z")),
            day(2026, 11, 10)
        );
        assert_eq!(first_open_day(utc("2026-07-01T20:30:00Z")), day(2026, 7, 2));
        assert_eq!(
            last_settled(utc("2026-11-10T08:00:00Z")),
            utc("2026-11-09T21:30:00Z")
        );
        assert_eq!(
            last_settled(utc("2026-11-08T18:00:00Z")),
            utc("2026-11-06T21:30:00Z")
        );
        assert_eq!(
            day_of(utc("2026-11-10T02:00:00Z").timestamp() as f64),
            "2026-11-09"
        );
    }
}
