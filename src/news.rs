//! Headlines: per-stock feeds from Yahoo Finance and per-sector searches from Google News,
//! with Bing News as the second source. No account or key is needed for any of them.

use crate::http;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::sync::Mutex;
use std::time::Duration;

/// One news item as a feed reported it.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Headline {
    pub title: String,
    /// Publisher, when the feed names one.
    pub source: String,
    /// Publication time in epoch seconds, 0 when the feed gave none.
    pub published: f64,
    pub link: String,
}

/// The text of the first `<name>` element, with or without attributes.
fn tag<'a>(block: &'a str, name: &str) -> Option<&'a str> {
    let open = format!("<{name}");
    let mut from = 0;
    let start = loop {
        let at = block[from..].find(&open)? + from;
        let after = at + open.len();
        match block[after..].chars().next()? {
            '>' => break after + 1,
            c if c.is_whitespace() => break block[after..].find('>')? + after + 1,
            _ => from = after,
        }
    };
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
        .replace("&apos;", "'")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

/// Parse an RSS feed. Aggregator feeds put the publisher after the last " - " in the
/// title; it is moved into `source`.
pub fn parse_rss(xml: &str, limit: usize) -> Vec<Headline> {
    xml.split("<item>")
        .skip(1)
        .filter_map(|item| {
            let mut title = unwrap_text(tag(item, "title")?);
            let field = |name: &str| tag(item, name).map(unwrap_text).unwrap_or_default();
            let mut source = field("source");
            if let Some(at) = title.rfind(" - ") {
                let tail = title[at + 3..].trim().to_string();
                if !tail.is_empty() && tail.len() <= 40 && (source.is_empty() || source == tail) {
                    source = tail;
                    title.truncate(at);
                }
            }
            let published = DateTime::parse_from_rfc2822(&field("pubDate"))
                .map_or(0.0, |d| d.with_timezone(&Utc).timestamp() as f64);
            (!title.trim().is_empty()).then(|| Headline {
                title: title.trim().to_string(),
                source,
                published,
                link: field("link"),
            })
        })
        .take(limit)
        .collect()
}

const SPAM: [&str; 20] = [
    "shareholder alert",
    "investor alert",
    "class action",
    "law firm",
    "law offices",
    "lead plaintiff",
    "securities fraud",
    "investors to contact",
    "encourages investors",
    "reminds investors",
    "to present at",
    "to participate in",
    "to host conference call",
    "conference call transcript",
    "earnings call transcript",
    "stocks to buy",
    "stocks to watch",
    "should you buy",
    "zacks rank",
    "horoscope",
];

/// Law-firm solicitations, conference notices and listicles: never worth a model's time.
pub fn is_spam(title: &str) -> bool {
    let lower = title.to_lowercase();
    SPAM.iter().any(|s| lower.contains(s))
}

fn key(title: &str) -> String {
    title
        .to_lowercase()
        .chars()
        .filter(|c| c.is_alphanumeric())
        .take(60)
        .collect()
}

/// Drop spam, items older than `max_age_days` (undated items are kept) and repeats, and
/// return the rest newest first.
pub fn clean(mut items: Vec<Headline>, now: f64, max_age_days: i64, limit: usize) -> Vec<Headline> {
    let oldest = now - max_age_days as f64 * 86400.0;
    let mut seen = BTreeSet::new();
    items.retain(|h| {
        !is_spam(&h.title)
            && (h.published == 0.0 || h.published >= oldest)
            && h.published <= now + 86400.0
            && seen.insert(key(&h.title))
    });
    items.sort_by(|a, b| b.published.total_cmp(&a.published));
    items.truncate(limit);
    items
}

/// Yahoo Finance's feed for one ticker.
pub fn for_ticker(symbol: &str) -> Vec<Headline> {
    let url = format!(
        "https://feeds.finance.yahoo.com/rss/2.0/headline?s={}&region=US&lang=en-US",
        http::encode(&symbol.replace('.', "-"))
    );
    http::get_retry(&url, Duration::from_secs(15), 2)
        .map(|xml| parse_rss(&xml, 20))
        .unwrap_or_default()
}

/// A news search for a sector over the last two days. Returns the items and the source used.
pub fn for_topic(query: &str) -> (Vec<Headline>, &'static str) {
    let google = format!(
        "https://news.google.com/rss/search?q={}&hl=en-US&gl=US&ceid=US:en",
        http::encode(&format!("{query} when:2d"))
    );
    let items = http::get_retry(&google, Duration::from_secs(20), 2)
        .map(|xml| parse_rss(&xml, 40))
        .unwrap_or_default();
    if !items.is_empty() {
        return (items, "Google News");
    }
    let bing = format!(
        "https://www.bing.com/news/search?q={}&format=rss&qft=interval%3d%227%22",
        http::encode(query)
    );
    let items = http::get_retry(&bing, Duration::from_secs(20), 2)
        .map(|xml| parse_rss(&xml, 40))
        .unwrap_or_default();
    (items, "Bing News")
}

/// Per-ticker headlines for many symbols, four requests at a time.
pub fn for_tickers(symbols: &BTreeSet<String>) -> BTreeMap<String, Vec<Headline>> {
    let queue = Mutex::new(symbols.iter().cloned().collect::<VecDeque<_>>());
    let found = Mutex::new(BTreeMap::new());
    std::thread::scope(|scope| {
        for _ in 0..4 {
            scope.spawn(|| {
                loop {
                    let Some(symbol) = queue.lock().unwrap().pop_front() else {
                        break;
                    };
                    let items = for_ticker(&symbol);
                    found.lock().unwrap().insert(symbol, items);
                }
            });
        }
    });
    found.into_inner().unwrap()
}

#[cfg(test)]
mod tests {
    use super::*;

    const FEED: &str = "<rss><channel><title>Feed</title>\
        <item><title><![CDATA[Acme wins $2B Pentagon AI contract &amp; shares jump - Reuters]]></title>\
        <pubDate>Mon, 09 Nov 2026 13:00:00 +0000</pubDate><link>https://x.test/a</link>\
        <source url=\"https://reuters.com\">Reuters</source></item>\
        <item><title>SHAREHOLDER ALERT: Law firm reminds investors of deadline</title>\
        <pubDate>Mon, 09 Nov 2026 14:00:00 +0000</pubDate></item>\
        <item><title>Acme wins $2B Pentagon AI contract &amp; shares jump</title>\
        <pubDate>Mon, 09 Nov 2026 12:00:00 +0000</pubDate></item>\
        <item><title>Old story about Acme</title><pubDate>Mon, 02 Nov 2026 12:00:00 +0000</pubDate></item>\
        <item><title>Undated note</title></item></channel></rss>";

    #[test]
    fn rss_items_are_parsed_with_publisher_and_time() {
        let items = parse_rss(FEED, 10);
        assert_eq!(items.len(), 5);
        assert_eq!(
            items[0].title,
            "Acme wins $2B Pentagon AI contract & shares jump"
        );
        assert_eq!(items[0].source, "Reuters");
        assert_eq!(items[0].link, "https://x.test/a");
        assert!((items[0].published - 1_794_229_200.0).abs() < 1.0);
        assert_eq!(items[4].published, 0.0);
        assert_eq!(parse_rss(FEED, 2).len(), 2);
    }

    #[test]
    fn cleaning_drops_spam_old_items_and_repeats() {
        let now = 1_794_229_200.0 + 3600.0 * 5.0;
        let kept = clean(parse_rss(FEED, 10), now, 4, 10);
        let titles: Vec<&str> = kept.iter().map(|h| h.title.as_str()).collect();
        assert_eq!(
            titles,
            [
                "Acme wins $2B Pentagon AI contract & shares jump",
                "Undated note"
            ]
        );
    }

    #[test]
    fn spam_is_recognised() {
        assert!(is_spam(
            "ROSEN, A LEADING LAW FIRM, Encourages Investors to Act"
        ));
        assert!(is_spam("Acme to Present at the Global Tech Conference"));
        assert!(!is_spam("Acme lands record order from hyperscaler"));
    }
}
