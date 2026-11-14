//! Running the desk as of an earlier moment. Every source is cut at the cutoff so the
//! models see only what was known then: bars end at the last session before it, headlines
//! must have been published before it, and Reddit counts are the ones from a day earlier.
//! Trending lists have no history, so a replay goes without them.

use crate::buzz::{self, Attention, SourceNote};
use crate::config::MIN_MARKET_CAP;
use crate::desk::Feed;
use crate::market::{self, Listing, Quote};
use crate::news::{self, Headline};
use anyhow::{Result, bail};
use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet};

/// The public sources, cut at `cutoff` (epoch seconds).
#[derive(Debug)]
pub struct AsOf {
    pub cutoff: f64,
    cache: RefCell<BTreeMap<String, Quote>>,
}

impl AsOf {
    pub fn new(cutoff: f64) -> Self {
        AsOf {
            cutoff,
            cache: RefCell::new(BTreeMap::new()),
        }
    }
}

/// Drop bars and corporate actions from after the cutoff.
pub fn truncate(mut quote: Quote, cutoff: f64) -> Option<Quote> {
    quote.bars.retain(|b| b.at < cutoff);
    quote.actions.retain(|a| a.date < cutoff);
    (quote.bars.len() >= 21).then_some(quote)
}

/// Replace a listing's latest-session numbers with the last session before the cutoff.
pub fn rewind(listing: &mut Listing, quote: &Quote) -> bool {
    let (Some(last), Some(change)) = (quote.last(), quote.change(1)) else {
        return false;
    };
    listing.price = last.close;
    listing.change = change;
    listing.volume = last.volume;
    true
}

/// Keep only headlines published before the cutoff. Undated ones are dropped because
/// nothing shows they were known then.
pub fn published_before(items: Vec<Headline>, cutoff: f64) -> Vec<Headline> {
    items
        .into_iter()
        .filter(|h| h.published > 0.0 && h.published < cutoff)
        .collect()
}

/// Shift Reddit counts back a day and drop trending lists.
pub fn shift_attention(mut attention: BTreeMap<String, Attention>) -> BTreeMap<String, Attention> {
    for seen in attention.values_mut() {
        seen.reddit = seen.reddit_before;
        seen.reddit_before = 0;
        seen.trending.clear();
    }
    attention.retain(|_, seen| seen.reddit > 0);
    attention
}

impl AsOf {
    fn fetch(&self, symbols: &BTreeSet<String>) -> BTreeMap<String, String> {
        let missing: BTreeSet<String> = symbols
            .iter()
            .filter(|s| !self.cache.borrow().contains_key(*s))
            .cloned()
            .collect();
        let (quotes, mut errors) = market::histories(&missing);
        let mut cache = self.cache.borrow_mut();
        for (symbol, quote) in quotes {
            match truncate(quote, self.cutoff) {
                Some(quote) => {
                    cache.insert(symbol, quote);
                }
                None => {
                    errors.insert(symbol, "too few sessions before the cutoff".into());
                }
            }
        }
        errors
    }
}

impl Feed for AsOf {
    fn listings(&self) -> Result<Vec<Listing>> {
        let mut rows = market::listings()?;
        // Only companies that could pass the screen need their history; the rest keep
        // today's numbers, which only ever fail them.
        let wanted: BTreeSet<String> = rows
            .iter()
            .filter(|l| market::is_company_stock(&l.name) && l.market_cap >= MIN_MARKET_CAP)
            .map(|l| l.symbol.clone())
            .collect();
        crate::progress::log(&format!(
            "replay: fetching history for {} companies",
            wanted.len()
        ));
        self.fetch(&wanted);
        let cache = self.cache.borrow();
        rows.retain_mut(|l| {
            !wanted.contains(&l.symbol) || cache.get(&l.symbol).is_some_and(|q| rewind(l, q))
        });
        if rows.len() < 1000 {
            bail!(
                "replay: only {} listings have history before the cutoff",
                rows.len()
            );
        }
        Ok(rows)
    }

    fn funds(&self) -> BTreeSet<String> {
        market::fund_symbols()
    }

    fn attention(&self) -> (BTreeMap<String, Attention>, Vec<SourceNote>) {
        let (attention, mut notes) = buzz::gather();
        notes.push(SourceNote {
            name: "Replay".into(),
            ok: true,
            detail: "Reddit counts from the day before; trending lists have no history and were left out".into(),
        });
        (shift_attention(attention), notes)
    }

    fn histories(
        &self,
        symbols: &BTreeSet<String>,
    ) -> (BTreeMap<String, Quote>, BTreeMap<String, String>) {
        let errors = self.fetch(symbols);
        let cache = self.cache.borrow();
        let quotes = symbols
            .iter()
            .filter_map(|s| cache.get(s).map(|q| (s.clone(), q.clone())))
            .collect();
        (quotes, errors)
    }

    fn ticker_news(&self, symbols: &BTreeSet<String>) -> BTreeMap<String, Vec<Headline>> {
        news::for_tickers(symbols)
            .into_iter()
            .map(|(s, items)| (s, published_before(items, self.cutoff)))
            .collect()
    }

    fn topic_news(&self, query: &str) -> (Vec<Headline>, &'static str) {
        let day = market::day_of(self.cutoff);
        let date = chrono::NaiveDate::parse_from_str(&day, "%Y-%m-%d").unwrap_or_default();
        let window = format!(
            "after:{} before:{}",
            date - chrono::Duration::days(3),
            date + chrono::Duration::days(1)
        );
        let (items, source) = news::for_topic_window(query, &window);
        (published_before(items, self.cutoff), source)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::market::{Action, Bar};

    fn quote(n: usize) -> Quote {
        Quote {
            bars: (0..n)
                .map(|i| Bar {
                    at: i as f64 * 10.0,
                    open: 1.0,
                    close: 1.0 + i as f64,
                    volume: 100.0 + i as f64,
                })
                .collect(),
            actions: vec![Action {
                kind: "splits".into(),
                date: 1000.0,
                amount: None,
                numerator: Some(2.0),
                denominator: Some(1.0),
            }],
            instrument_type: Some("EQUITY".into()),
        }
    }

    #[test]
    fn nothing_after_the_cutoff_survives() {
        let cut = truncate(quote(40), 300.0).unwrap();
        assert_eq!(cut.bars.len(), 30);
        assert!(cut.actions.is_empty());
        assert!(truncate(quote(40), 150.0).is_none());
        let mut listing = Listing::default();
        assert!(rewind(&mut listing, &cut));
        assert!((listing.price - 30.0).abs() < 1e-9);
        assert!((listing.volume - 129.0).abs() < 1e-9);
        assert!((listing.change - (30.0 / 29.0 - 1.0) * 100.0).abs() < 1e-9);
        let items = vec![
            Headline {
                title: "before".into(),
                published: 10.0,
                ..Headline::default()
            },
            Headline {
                title: "after".into(),
                published: 500.0,
                ..Headline::default()
            },
            Headline {
                title: "undated".into(),
                ..Headline::default()
            },
        ];
        let kept = published_before(items, 300.0);
        assert_eq!(kept.len(), 1);
        assert_eq!(kept[0].title, "before");
    }

    #[test]
    fn attention_moves_back_a_day() {
        let mut map = BTreeMap::new();
        map.insert(
            "A".to_string(),
            Attention {
                reddit: 50,
                reddit_before: 20,
                trending: vec!["Yahoo".into()],
            },
        );
        map.insert(
            "B".to_string(),
            Attention {
                reddit: 50,
                reddit_before: 0,
                trending: vec![],
            },
        );
        let shifted = shift_attention(map);
        assert_eq!(shifted.len(), 1);
        assert_eq!(shifted["A"].reddit, 20);
        assert!(shifted["A"].trending.is_empty());
    }
}
