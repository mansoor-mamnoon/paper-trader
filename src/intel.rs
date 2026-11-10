//! Market intelligence for the analyst: upcoming earnings dates and recent headlines.

use crate::http;
use crate::model::{Headline, Intel};
use anyhow::Result;
use chrono::{Datelike, Duration as Days, NaiveDate, Weekday};
use serde_json::Value;
use std::collections::{BTreeMap, VecDeque};
use std::sync::Mutex;
use std::time::Duration;

const AGENT: &str = "Mozilla/5.0 (paper-trader research)";

/// Symbols reporting earnings on each weekday from `from` through `days` days ahead.
/// The value is the report date as `YYYY-MM-DD`.
pub fn fetch_earnings(from: NaiveDate, days: i64) -> BTreeMap<String, String> {
    let mut found = BTreeMap::new();
    for offset in 0..=days {
        let day = from + Days::days(offset);
        if matches!(day.weekday(), Weekday::Sat | Weekday::Sun) {
            continue;
        }
        let url = format!("https://api.nasdaq.com/api/calendar/earnings?date={day}");
        let Ok(body) = http::get(&url, AGENT, Duration::from_secs(15)) else {
            continue;
        };
        for (symbol, date) in parse_earnings(&body, day) {
            found.entry(symbol).or_insert(date);
        }
    }
    found
}

/// Parse one Nasdaq earnings-calendar response into `(symbol, date)` pairs.
pub fn parse_earnings(body: &str, day: NaiveDate) -> Vec<(String, String)> {
    let Ok(value) = serde_json::from_str::<Value>(body) else {
        return Vec::new();
    };
    value["data"]["rows"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|row| row["symbol"].as_str())
        .map(|s| (s.trim().to_string(), day.to_string()))
        .collect()
}

fn tag<'a>(block: &'a str, name: &str) -> Option<&'a str> {
    let start = block.find(&format!("<{name}>"))? + name.len() + 2;
    let end = block[start..].find(&format!("</{name}>"))? + start;
    Some(block[start..end].trim())
}

fn unwrap_text(raw: &str) -> String {
    let raw = raw.trim();
    let raw = raw
        .strip_prefix("<![CDATA[")
        .and_then(|r| r.strip_suffix("]]>"))
        .unwrap_or(raw);
    raw.replace("&amp;", "&")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
}

/// Parse an RSS feed into headlines, newest first as published.
pub fn parse_rss(xml: &str, limit: usize) -> Vec<Headline> {
    xml.split("<item>")
        .skip(1)
        .filter_map(|item| {
            let title = unwrap_text(tag(item, "title")?);
            let published = tag(item, "pubDate").map(unwrap_text).unwrap_or_default();
            (!title.is_empty()).then_some(Headline { title, published })
        })
        .take(limit)
        .collect()
}

pub fn fetch_news(symbol: &str) -> Vec<Headline> {
    let url =
        format!("https://feeds.finance.yahoo.com/rss/2.0/headline?s={symbol}&region=US&lang=en-US");
    http::get(&url, AGENT, Duration::from_secs(15))
        .map(|x| parse_rss(&x, 12))
        .unwrap_or_default()
}

/// Collect earnings dates (the last week and the next two) and headlines for `symbols`.
pub fn gather(symbols: &[String], today: NaiveDate) -> Result<Intel> {
    let earnings = fetch_earnings(today, 14);
    let reported = fetch_earnings(today - Days::days(7), 6);
    let queue = Mutex::new(symbols.iter().cloned().collect::<VecDeque<_>>());
    let news = Mutex::new(BTreeMap::new());
    std::thread::scope(|scope| {
        for _ in 0..4 {
            scope.spawn(|| {
                while let Some(symbol) = queue.lock().unwrap().pop_front() {
                    let items = fetch_news(&symbol);
                    if !items.is_empty() {
                        news.lock().unwrap().insert(symbol, items);
                    }
                }
            });
        }
    });
    Ok(Intel {
        earnings,
        reported,
        news: news.into_inner().unwrap(),
        ..Intel::default()
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rss_titles_are_unwrapped_and_limited() {
        let xml = "<rss><channel><title>Feed</title><item><title><![CDATA[Acme raises guidance & shares jump]]></title><pubDate>Mon, 09 Nov 2026 13:00:00 +0000</pubDate></item><item><title>Second &amp; third</title></item><item><title>Third</title></item></channel></rss>";
        let items = parse_rss(xml, 2);
        assert_eq!(items.len(), 2);
        assert_eq!(items[0].title, "Acme raises guidance & shares jump");
        assert_eq!(items[1].title, "Second & third");
    }

    #[test]
    fn earnings_rows_parse_and_bad_json_is_empty() {
        let day = NaiveDate::from_ymd_opt(2026, 11, 10).unwrap();
        let body = r#"{"data":{"rows":[{"symbol":"AAPL"},{"symbol":" msft "}]}}"#;
        let rows = parse_earnings(body, day);
        assert_eq!(rows[0], ("AAPL".to_string(), "2026-11-10".to_string()));
        assert_eq!(rows.len(), 2);
        assert_eq!(parse_earnings("not json", day).len(), 0);
    }
}
