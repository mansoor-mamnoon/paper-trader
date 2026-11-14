//! The buzz screen. It ranks stocks by how much attention they are getting, not by trend:
//! Reddit mentions, trending lists, the size of the latest move and unusual volume. It
//! decides what the models read. It never decides a trade.

use crate::config::PRESCREEN;
use crate::http;
use crate::market::{Listing, Quote};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;
use std::time::Duration;

/// Attention one stock is getting outside the tape.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Attention {
    /// Reddit mentions over the last 24 hours.
    pub reddit: u32,
    /// Reddit mentions over the 24 hours before that.
    pub reddit_before: u32,
    /// Lists the stock is trending on.
    pub trending: Vec<String>,
}

/// Whether a public source answered, for the dashboard.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct SourceNote {
    pub name: String,
    pub ok: bool,
    pub detail: String,
}

/// Parse one page of `ApeWisdom`'s Reddit mention counts.
pub fn parse_reddit(body: &str, into: &mut BTreeMap<String, Attention>) -> usize {
    let Ok(value) = serde_json::from_str::<Value>(body) else {
        return 0;
    };
    let mut count = 0;
    for row in value["results"].as_array().into_iter().flatten() {
        let Some(symbol) = row["ticker"].as_str() else {
            continue;
        };
        let number = |key: &str| {
            row[key]
                .as_u64()
                .or_else(|| row[key].as_str().and_then(|s| s.parse().ok()))
                .unwrap_or(0) as u32
        };
        let entry = into.entry(symbol.to_string()).or_default();
        entry.reddit = number("mentions");
        entry.reddit_before = number("mentions_24h_ago");
        count += 1;
    }
    count
}

/// Parse Yahoo Finance's trending tickers.
pub fn parse_yahoo_trending(body: &str) -> Vec<String> {
    serde_json::from_str::<Value>(body)
        .ok()
        .and_then(|v| v["finance"]["result"][0]["quotes"].as_array().cloned())
        .into_iter()
        .flatten()
        .filter_map(|q| q["symbol"].as_str().map(|s| s.replace('-', ".")))
        .collect()
}

/// Parse `StockTwits`' trending symbols.
pub fn parse_stocktwits(body: &str) -> Vec<String> {
    serde_json::from_str::<Value>(body)
        .ok()
        .and_then(|v| v["symbols"].as_array().cloned())
        .into_iter()
        .flatten()
        .filter_map(|q| q["symbol"].as_str().map(str::to_string))
        .collect()
}

/// Ask every attention source. A source that fails is noted and skipped.
pub fn gather() -> (BTreeMap<String, Attention>, Vec<SourceNote>) {
    let mut attention = BTreeMap::new();
    let mut notes = Vec::new();
    let timeout = Duration::from_secs(20);
    let mut reddit = 0;
    let mut reddit_error = String::new();
    for page in 1..=3 {
        let url = format!("https://apewisdom.io/api/v1.0/filter/all-stocks/page/{page}");
        match http::get_retry(&url, timeout, 2) {
            Ok(body) => reddit += parse_reddit(&body, &mut attention),
            Err(error) => reddit_error = error.to_string(),
        }
    }
    notes.push(SourceNote {
        name: "Reddit mentions (ApeWisdom)".into(),
        ok: reddit > 0,
        detail: if reddit > 0 {
            format!("{reddit} tickers")
        } else {
            reddit_error
        },
    });
    for (name, url, parse) in [
        (
            "Yahoo Finance trending",
            "https://query1.finance.yahoo.com/v1/finance/trending/US?count=50",
            parse_yahoo_trending as fn(&str) -> Vec<String>,
        ),
        (
            "StockTwits trending",
            "https://api.stocktwits.com/api/2/trending/symbols.json",
            parse_stocktwits,
        ),
    ] {
        let label = name.split(' ').next().unwrap_or(name);
        match http::get_retry(url, timeout, 2).map(|body| parse(&body)) {
            Ok(symbols) if !symbols.is_empty() => {
                for symbol in &symbols {
                    attention
                        .entry(symbol.clone())
                        .or_default()
                        .trending
                        .push(label.to_string());
                }
                notes.push(SourceNote {
                    name: name.into(),
                    ok: true,
                    detail: format!("{} tickers", symbols.len()),
                });
            }
            Ok(_) => notes.push(SourceNote {
                name: name.into(),
                ok: false,
                detail: "empty answer".into(),
            }),
            Err(error) => notes.push(SourceNote {
                name: name.into(),
                ok: false,
                detail: error.to_string(),
            }),
        }
    }
    (attention, notes)
}

/// One stock with its buzz score.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Buzz {
    pub listing: Listing,
    pub attention: Attention,
    /// The total. Higher means more attention.
    pub score: f64,
    /// Latest volume against the 20-session average, once bars are known.
    pub relative_volume: Option<f64>,
}

/// Points for the latest move. A rise counts in full and a fall counts half: bad news is
/// attention too, but the account only buys.
fn move_points(change: f64) -> f64 {
    let size = change.abs().min(15.0) / 15.0;
    if change >= 0.0 {
        3.0 * size
    } else {
        1.5 * size
    }
}

/// Points for Reddit chatter: up to 3 for volume, 1 more when mentions grew by half.
fn reddit_points(attention: &Attention) -> f64 {
    let volume = (f64::from(attention.reddit).ln_1p() / 500.0_f64.ln_1p()).min(1.0) * 3.0;
    let growing = attention.reddit >= 10
        && f64::from(attention.reddit) > 1.5 * f64::from(attention.reddit_before);
    volume + if growing { 1.0 } else { 0.0 }
}

/// Score every eligible stock on the move, dollar volume, Reddit and trending lists, best
/// first.
pub fn score_all(eligible: &[Listing], attention: &BTreeMap<String, Attention>) -> Vec<Buzz> {
    let mut volumes: Vec<f64> = eligible.iter().map(Listing::dollar_volume).collect();
    volumes.sort_by(f64::total_cmp);
    let mut scored: Vec<Buzz> = eligible
        .iter()
        .map(|listing| {
            let seen = attention.get(&listing.symbol).cloned().unwrap_or_default();
            let below = volumes.partition_point(|v| *v < listing.dollar_volume());
            let volume_rank = below as f64 / volumes.len().max(1) as f64;
            let score = move_points(listing.change)
                + volume_rank
                + reddit_points(&seen)
                + 1.5 * seen.trending.len() as f64;
            Buzz {
                listing: listing.clone(),
                attention: seen,
                score,
                relative_volume: None,
            }
        })
        .collect();
    rank(&mut scored);
    scored
}

/// The top [`PRESCREEN`] of [`score_all`].
pub fn prescreen(eligible: &[Listing], attention: &BTreeMap<String, Attention>) -> Vec<Buzz> {
    let mut scored = score_all(eligible, attention);
    scored.truncate(PRESCREEN);
    scored
}

/// Add up to 3 points for unusual volume once the bars are known, and rank again.
pub fn refine(scored: &mut [Buzz], quotes: &BTreeMap<String, Quote>) {
    for item in scored.iter_mut() {
        if item.relative_volume.is_some() {
            continue;
        }
        if let Some(ratio) = quotes
            .get(&item.listing.symbol)
            .and_then(Quote::relative_volume)
        {
            item.relative_volume = Some(ratio);
            if ratio > 1.0 {
                item.score += (ratio - 1.0).min(4.0) / 4.0 * 3.0;
            }
        }
    }
    rank(scored);
}

fn rank(scored: &mut [Buzz]) {
    scored.sort_by(|a, b| {
        b.score
            .total_cmp(&a.score)
            .then_with(|| a.listing.symbol.cmp(&b.listing.symbol))
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::market::Bar;

    fn listing(symbol: &str, change: f64, dollars: f64) -> Listing {
        Listing {
            symbol: symbol.into(),
            name: format!("{symbol} Inc. Common Stock"),
            price: 10.0,
            change,
            volume: dollars / 10.0,
            market_cap: 5.0e9,
            ..Listing::default()
        }
    }

    #[test]
    fn sources_are_parsed() {
        let mut attention = BTreeMap::new();
        let body = r#"{"results":[{"ticker":"NVDA","mentions":1240,"mentions_24h_ago":"2812"},{"ticker":"NBIS","mentions":192,"mentions_24h_ago":48}]}"#;
        assert_eq!(parse_reddit(body, &mut attention), 2);
        assert_eq!(attention["NVDA"].reddit, 1240);
        assert_eq!(attention["NVDA"].reddit_before, 2812);
        assert_eq!(parse_reddit("nope", &mut attention), 0);
        let yahoo = r#"{"finance":{"result":[{"quotes":[{"symbol":"UAL"},{"symbol":"BRK-B"}]}]}}"#;
        assert_eq!(parse_yahoo_trending(yahoo), ["UAL", "BRK.B"]);
        let twits = r#"{"symbols":[{"symbol":"TRUMP.X"},{"symbol":"HOOD"}]}"#;
        assert_eq!(parse_stocktwits(twits), ["TRUMP.X", "HOOD"]);
    }

    #[test]
    fn attention_outranks_a_quiet_tape() {
        let eligible = vec![
            listing("QUIET", 0.2, 9.0e8),
            listing("MOVER", 9.0, 1.0e8),
            listing("CHAT", 0.5, 5.0e7),
            listing("DROP", -9.0, 1.0e8),
        ];
        let mut attention = BTreeMap::new();
        attention.insert(
            "CHAT".to_string(),
            Attention {
                reddit: 300,
                reddit_before: 100,
                trending: vec!["Yahoo".into()],
            },
        );
        let ranked = prescreen(&eligible, &attention);
        let order: Vec<&str> = ranked.iter().map(|b| b.listing.symbol.as_str()).collect();
        assert_eq!(order, ["CHAT", "MOVER", "DROP", "QUIET"]);
        // A fall earns half the points of an equal rise.
        assert!((move_points(9.0) - 2.0 * move_points(-9.0)).abs() < 1e-9);
    }

    #[test]
    fn unusual_volume_lifts_a_stock() {
        let eligible = vec![listing("AAA", 2.0, 1.0e8), listing("BBB", 2.0, 1.0e8)];
        let mut ranked = prescreen(&eligible, &BTreeMap::new());
        assert_eq!(ranked[0].listing.symbol, "AAA");
        let bars = |last: f64| Quote {
            bars: (0..21)
                .map(|i| Bar {
                    at: f64::from(i),
                    open: 1.0,
                    close: 1.0,
                    volume: if i == 20 { last } else { 100.0 },
                })
                .collect(),
            ..Quote::default()
        };
        let quotes: BTreeMap<String, Quote> = [
            ("AAA".to_string(), bars(100.0)),
            ("BBB".to_string(), bars(400.0)),
        ]
        .into();
        refine(&mut ranked, &quotes);
        assert_eq!(ranked[0].listing.symbol, "BBB");
        assert_eq!(ranked[0].relative_volume, Some(4.0));
        assert!((ranked[0].score - ranked[1].score - 2.25).abs() < 1e-9);
    }
}
