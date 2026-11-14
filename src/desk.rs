//! One trading day, start to finish.
//!
//! 1. Settle: fill yesterday's orders at today's open and mark the book.
//! 2. Screen: rank every eligible stock by buzz.
//! 3. Scouts write each sector's newsbook entry.
//! 4. The strategist picks the sectors to trade.
//! 5. The classifier labels the headlines of every holding and candidate.
//! 6. The analyst makes a case for each stock and the skeptic attacks it.
//! 7. The manager keeps or sells every holding and picks every purchase.
//! 8. Orders are queued for the next open and everything is written to memory.

use crate::agents::{self, Book, Card, Facts, Focus, Holding, Memory};
use crate::broker;
use crate::buzz::{self, Attention, Buzz, SourceNote};
use crate::config::{
    CLASSIFY_BATCH, FOCUS_CANDIDATES, HEADLINE_DAYS, HEADLINES_PER_STOCK, MANAGER_MEMORY,
    MAX_FOCUS, MIN_HOLD_DAYS, NEWSBOOK_MEMORY, PRESCREEN, SCOUT_HEADLINES, SCOUT_NAMES,
    TRADE_MEMORY, WILDCARDS,
};
use crate::llm::Brain;
use crate::market::{self, Listing, Quote};
use crate::models::Agent;
use crate::news::{self, Headline};
use crate::progress;
use crate::sectors::{self, SECTORS};
use crate::store::{self, ENGINE, Entry, Order, SectorNote, State, Store};
use anyhow::Result;
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::collections::{BTreeMap, BTreeSet};
use std::time::Instant;

/// Where the day's data comes from. The live feed is the internet; tests use a fixture.
pub trait Feed {
    fn listings(&self) -> Result<Vec<Listing>>;
    fn funds(&self) -> BTreeSet<String>;
    fn attention(&self) -> (BTreeMap<String, Attention>, Vec<SourceNote>);
    fn histories(
        &self,
        symbols: &BTreeSet<String>,
    ) -> (BTreeMap<String, Quote>, BTreeMap<String, String>);
    fn ticker_news(&self, symbols: &BTreeSet<String>) -> BTreeMap<String, Vec<Headline>>;
    fn topic_news(&self, query: &str) -> (Vec<Headline>, &'static str);
}

/// The public, free data sources.
#[derive(Clone, Copy, Debug)]
pub struct Live;

impl Feed for Live {
    fn listings(&self) -> Result<Vec<Listing>> {
        market::listings()
    }
    fn funds(&self) -> BTreeSet<String> {
        market::fund_symbols()
    }
    fn attention(&self) -> (BTreeMap<String, Attention>, Vec<SourceNote>) {
        buzz::gather()
    }
    fn histories(
        &self,
        symbols: &BTreeSet<String>,
    ) -> (BTreeMap<String, Quote>, BTreeMap<String, String>) {
        market::histories(symbols)
    }
    fn ticker_news(&self, symbols: &BTreeSet<String>) -> BTreeMap<String, Vec<Headline>> {
        news::for_tickers(symbols)
    }
    fn topic_news(&self, query: &str) -> (Vec<Headline>, &'static str) {
        news::for_topic(query)
    }
}

/// How the day narrowed the market down.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Funnel {
    pub listed: usize,
    pub eligible: usize,
    pub screened: usize,
    pub candidates: usize,
    pub headlines: usize,
    pub calls: usize,
    pub failed_calls: usize,
    pub minutes: f64,
}

/// Everything the desk did in one day, for the dashboard and the archive.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Report {
    pub day: String,
    pub at: f64,
    /// Agent name to the model that played it.
    pub models: BTreeMap<String, String>,
    pub funnel: Funnel,
    pub sources: Vec<SourceNote>,
    /// Sector id to how its stocks traded.
    pub tapes: BTreeMap<String, String>,
    pub newsbook: Vec<SectorNote>,
    pub focus: Focus,
    /// Holdings first, then candidates.
    pub cards: Vec<Card>,
    /// KEEP, SELL, BUY and `TOP_UP` lines.
    pub decisions: Vec<Entry>,
    /// The manager's note to itself.
    pub note: String,
    /// The most-buzzed stocks of the day, whatever their sector.
    pub buzz: Vec<Buzz>,
    pub problems: Vec<String>,
}

fn median(values: &mut [f64]) -> f64 {
    if values.is_empty() {
        return 0.0;
    }
    values.sort_by(f64::total_cmp);
    let mid = values.len() / 2;
    if values.len() % 2 == 1 {
        values[mid]
    } else {
        f64::midpoint(values[mid - 1], values[mid])
    }
}

/// How a sector's stocks traded, in one line.
pub fn tape(members: &[&Listing]) -> String {
    if members.is_empty() {
        return "no eligible stocks".into();
    }
    let mut changes: Vec<f64> = members.iter().map(|l| l.change).collect();
    let up = changes.iter().filter(|c| **c > 0.0).count();
    let mut leaders: Vec<&&Listing> = members.iter().collect();
    leaders.sort_by(|a, b| b.change.total_cmp(&a.change));
    let top: Vec<String> = leaders
        .iter()
        .take(3)
        .map(|l| format!("{} {:+.1}%", l.symbol, l.change))
        .collect();
    format!(
        "{} stocks, median move {:+.1}%, {}% rose. Biggest gainers: {}",
        members.len(),
        median(&mut changes),
        up * 100 / members.len(),
        top.join(", ")
    )
}

fn buzz_line(item: &Buzz) -> String {
    let mut line = format!(
        "{} ({}): {:+.1}% today",
        item.listing.symbol,
        item.listing.company(),
        item.listing.change
    );
    if let Some(ratio) = item.relative_volume {
        line.push_str(&format!(", volume {ratio:.1}x normal"));
    }
    if item.attention.reddit > 0 {
        line.push_str(&format!(
            ", Reddit mentions {} (day before {})",
            item.attention.reddit, item.attention.reddit_before
        ));
    }
    if !item.attention.trending.is_empty() {
        line.push_str(&format!(
            ", trending on {}",
            item.attention.trending.join(" and ")
        ));
    }
    line
}

fn facts(
    listing: Option<&Listing>,
    quote: Option<&Quote>,
    attention: Option<&Attention>,
    score: f64,
) -> Facts {
    let seen = attention.cloned().unwrap_or_default();
    Facts {
        price: quote
            .and_then(Quote::last)
            .map(|b| b.close)
            .or(listing.map(|l| l.price))
            .unwrap_or(0.0),
        market_cap: listing.map_or(0.0, |l| l.market_cap),
        change_1d: quote
            .and_then(|q| q.change(1))
            .or(listing.map(|l| l.change))
            .unwrap_or(0.0),
        change_5d: quote.and_then(|q| q.change(5)),
        change_20d: quote.and_then(|q| q.change(20)),
        relative_volume: quote.and_then(Quote::relative_volume),
        below_high: quote.and_then(Quote::below_high),
        reddit: seen.reddit,
        reddit_before: seen.reddit_before,
        trending: seen.trending,
        buzz: score,
    }
}

fn memory(store: &Store, day: &str) -> Result<Memory> {
    let decisions = store
        .journal_since(&store::days_before(day, 7))?
        .into_iter()
        .filter(|e| e.day.as_str() < day && matches!(e.kind.as_str(), "KEEP" | "SELL" | "BUY"))
        .take(15)
        .collect();
    let trades = store
        .trades(TRADE_MEMORY)?
        .iter()
        .map(|t| {
            format!(
                "{} ({}): {:+.1}% over {} days. Sold because: {}",
                t.symbol,
                if t.catalyst.is_empty() {
                    "no catalyst recorded"
                } else {
                    &t.catalyst
                },
                t.return_pct,
                ((t.closed_at - t.opened_at) / 86400.0).round().max(0.0),
                t.exit_reason
            )
        })
        .collect();
    let record = store
        .catalyst_record()?
        .iter()
        .map(|(label, count, average)| {
            format!("{label}: {count} closed trades, average {average:+.1}%")
        })
        .collect();
    Ok(Memory {
        notes: store.journal_kind("NOTE", day, MANAGER_MEMORY)?,
        decisions,
        trades,
        record,
    })
}

/// Take the account over from the desk this one replaced: its queued orders came from rules
/// that no longer exist, so they are cancelled and the manager decides afresh.
fn take_over(store: &Store, state: &mut State, at: f64) -> Result<()> {
    if state.engine == ENGINE {
        return Ok(());
    }
    if !state.pending.is_empty() {
        let symbols: Vec<&str> = state.pending.keys().map(String::as_str).collect();
        store.event(
            at,
            "takeover",
            &format!(
                "The model desk took the account over and cancelled the previous desk's queued orders: {}",
                symbols.join(", ")
            ),
        )?;
        state.pending.clear();
    }
    store.drop_legacy_tables()?;
    state.engine = ENGINE.into();
    Ok(())
}

/// Run one day. With no `brain` the books are still settled, but nothing is decided: every
/// trading decision belongs to a model.
pub fn run(
    store: &Store,
    brain: Option<&dyn Brain>,
    feed: &dyn Feed,
    at: f64,
    minutes: f64,
) -> Result<Report> {
    let started = Instant::now();
    let session = chrono::DateTime::from_timestamp(at as i64, 0).unwrap_or_default();
    let day = market::day_of(market::last_settled(session).timestamp() as f64);
    let mut report = Report {
        day: day.clone(),
        at,
        ..Report::default()
    };
    let mut state = store.load()?;

    // 1. The market, the attention and the books.
    let stage = progress::begin("market data and buzz screen");
    let listings = feed.listings()?;
    let funds = feed.funds();
    let by_symbol: BTreeMap<&str, &Listing> =
        listings.iter().map(|l| (l.symbol.as_str(), l)).collect();
    let eligible: Vec<Listing> = listings
        .iter()
        .filter(|l| market::ineligible(l, &funds).is_none())
        .cloned()
        .collect();
    let (attention, sources) = feed.attention();
    report.sources = sources;
    let all_scored = buzz::score_all(&eligible, &attention);
    let mut ranked: Vec<Buzz> = all_scored.iter().take(PRESCREEN).cloned().collect();
    let mut wanted: BTreeSet<String> = ranked.iter().map(|b| b.listing.symbol.clone()).collect();
    wanted.extend(state.positions.keys().cloned());
    wanted.extend(state.pending.keys().cloned());
    wanted.insert("SPY".into());
    let (mut quotes, quote_errors) = feed.histories(&wanted);
    for symbol in state.positions.keys() {
        if let Some(error) = quote_errors.get(symbol) {
            report
                .problems
                .push(format!("No prices for holding {symbol}: {error}"));
        }
    }
    store.begin()?;
    take_over(store, &mut state, at)?;
    broker::settle(store, &mut state, &quotes, at)?;
    store.save(&state)?;
    store.commit()?;
    // A stock must also be an equity according to the price provider.
    ranked.retain(|b| quotes.get(&b.listing.symbol).is_some_and(Quote::is_stock));
    buzz::refine(&mut ranked, &quotes);
    report.funnel.listed = listings.len();
    report.funnel.eligible = eligible.len();
    report.funnel.screened = ranked.len();
    report.buzz = ranked.iter().take(25).cloned().collect();
    let mut members: BTreeMap<&str, Vec<&Listing>> = BTreeMap::new();
    for listing in &eligible {
        members
            .entry(sectors::sector_of(listing).id)
            .or_default()
            .push(listing);
    }
    for sector in &SECTORS {
        report.tapes.insert(
            sector.id.into(),
            tape(members.get(sector.id).map_or(&[][..], Vec::as_slice)),
        );
    }
    drop(stage);

    let Some(brain) = brain else {
        report
            .problems
            .push("No model is available, so no trading decision was made today.".into());
        return finish(store, &mut state, report, None, started);
    };
    for agent in Agent::ALL {
        report
            .models
            .insert(agent.name().into(), brain.model_name(agent));
    }

    // 2. Scouts: one newsbook entry per sector.
    let stage = progress::begin("scouts write the sector newsbook");
    for sector in &SECTORS {
        let (items, _) = feed.topic_news(sector.query);
        let items = news::clean(items, at, 2, SCOUT_HEADLINES);
        let scope = format!("sector:{}", sector.id);
        store.remember_headlines(&day, &scope, &items)?;
        let brief = agents::ScoutBrief {
            day: day.clone(),
            sector: sector.id.into(),
            tape: report.tapes[sector.id].clone(),
            names: ranked
                .iter()
                .filter(|b| sectors::sector_of(&b.listing).id == sector.id)
                .take(SCOUT_NAMES)
                .map(buzz_line)
                .collect(),
            headlines: items
                .iter()
                .map(|h| {
                    if h.source.is_empty() {
                        h.title.clone()
                    } else {
                        format!("{} ({})", h.title, h.source)
                    }
                })
                .collect(),
            history: store.sector_history(sector.id, &day, NEWSBOOK_MEMORY)?,
        };
        match agents::scout(brain, &brief) {
            Ok(note) => {
                store.write_note(&note)?;
                report.newsbook.push(note);
            }
            Err(error) => report
                .problems
                .push(format!("Scout for {} failed: {error}", sector.name)),
        }
    }
    report
        .newsbook
        .sort_by(|a, b| b.heat.cmp(&a.heat).then_with(|| a.sector.cmp(&b.sector)));
    drop(stage);

    // 3. Strategist: the sectors to trade.
    let mut held_by_sector: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for (symbol, position) in &state.positions {
        let sector = by_symbol
            .get(symbol.as_str())
            .map_or(position.sector.clone(), |l| {
                sectors::sector_of(l).id.to_string()
            });
        if !sector.is_empty() {
            held_by_sector
                .entry(sector)
                .or_default()
                .push(symbol.clone());
        }
    }
    let earlier_focus = store.journal_kind("FOCUS", &day, MANAGER_MEMORY)?;
    report.focus = match agents::strategist(
        brain,
        &day,
        &report.newsbook,
        &report.tapes,
        &held_by_sector,
        &earlier_focus,
        MAX_FOCUS,
    ) {
        Ok(focus) => focus,
        Err(error) => {
            report.problems.push(format!("Strategist failed: {error}"));
            Focus {
                sectors: report
                    .newsbook
                    .iter()
                    .filter(|n| n.outlook != "bearish")
                    .take(2)
                    .map(|n| n.sector.clone())
                    .collect(),
                thesis: "The strategist did not answer, so the desk took the sectors the scouts scored hottest.".into(),
            }
        }
    };

    // 4. Candidates: the most-buzzed stocks of the focus sectors from the whole market, then
    // the day's most-buzzed stocks from anywhere else, none of them already held.
    let in_focus = |b: &Buzz| {
        report
            .focus
            .sectors
            .iter()
            .any(|s| s == sectors::sector_of(&b.listing).id)
    };
    let mut focus_pool: Vec<Buzz> = all_scored
        .iter()
        .filter(|b| !state.positions.contains_key(&b.listing.symbol) && in_focus(b))
        .take(FOCUS_CANDIDATES * 2)
        .cloned()
        .collect();
    let missing: BTreeSet<String> = focus_pool
        .iter()
        .map(|b| b.listing.symbol.clone())
        .filter(|s| !quotes.contains_key(s))
        .collect();
    if !missing.is_empty() {
        quotes.extend(feed.histories(&missing).0);
    }
    focus_pool.retain(|b| quotes.get(&b.listing.symbol).is_some_and(Quote::is_stock));
    buzz::refine(&mut focus_pool, &quotes);
    let mut candidates: Vec<Buzz> = focus_pool.into_iter().take(FOCUS_CANDIDATES).collect();
    let wildcards: Vec<Buzz> = ranked
        .iter()
        .filter(|b| {
            !state.positions.contains_key(&b.listing.symbol)
                && !candidates
                    .iter()
                    .any(|c| c.listing.symbol == b.listing.symbol)
        })
        .take(WILDCARDS + FOCUS_CANDIDATES.saturating_sub(candidates.len()))
        .cloned()
        .collect();
    candidates.extend(wildcards);
    report.funnel.candidates = candidates.len();

    // 5. Headlines for holdings and candidates, labelled by the classifier.
    let stage = progress::begin("classifier labels the headlines");
    let mut researched: Vec<String> = state.positions.keys().cloned().collect();
    researched.extend(candidates.iter().map(|b| b.listing.symbol.clone()));
    let since = store::days_before(&day, HEADLINE_DAYS);
    let fetched = feed.ticker_news(&researched.iter().cloned().collect());
    for symbol in &researched {
        let company = by_symbol
            .get(symbol.as_str())
            .map_or(symbol.clone(), |l| l.company());
        let items = news::clean(
            fetched.get(symbol).cloned().unwrap_or_default(),
            at,
            HEADLINE_DAYS,
            HEADLINES_PER_STOCK,
        );
        store.remember_headlines(&day, symbol, &items)?;
        let unlabelled: Vec<_> = store
            .headlines(symbol, &since, HEADLINES_PER_STOCK)?
            .into_iter()
            .filter(|h| h.label.is_empty())
            .collect();
        for batch in unlabelled.chunks(CLASSIFY_BATCH) {
            match agents::classify(brain, symbol, &company, batch) {
                Ok(labels) => {
                    for (id, label, score) in labels {
                        store.label_headline(id, &label, score)?;
                    }
                }
                Err(error) => report
                    .problems
                    .push(format!("Classifier failed on {symbol}: {error}")),
            }
        }
    }
    drop(stage);

    // 6. A card per stock, then the analyst and the skeptic.
    let stage = progress::begin("analyst and skeptic argue each stock");
    let scores: BTreeMap<&str, f64> = ranked
        .iter()
        .map(|b| (b.listing.symbol.as_str(), b.score))
        .collect();
    for symbol in &researched {
        let listing = by_symbol.get(symbol.as_str()).copied();
        let position = state.positions.get(symbol);
        let headlines = store.headlines(symbol, &since, HEADLINES_PER_STOCK)?;
        report.funnel.headlines += headlines.len();
        report.cards.push(Card {
            symbol: symbol.clone(),
            name: listing.map_or(symbol.clone(), Listing::company),
            sector: listing.map_or_else(
                || position.map(|p| p.sector.clone()).unwrap_or_default(),
                |l| sectors::sector_of(l).id.to_string(),
            ),
            facts: facts(
                listing,
                quotes.get(symbol),
                attention.get(symbol),
                scores.get(symbol.as_str()).copied().unwrap_or(0.0),
            ),
            headlines,
            holding: match position {
                Some(p) => Some(Holding {
                    cost: p.cost,
                    market_value: p.market_value,
                    pnl_pct: if p.cost > 0.0 {
                        (p.market_value / p.cost - 1.0) * 100.0
                    } else {
                        0.0
                    },
                    days_held: ((at - p.opened_at) / 86400.0).floor().max(0.0) as i64,
                    thesis: p.thesis.clone(),
                    history: store.journal_for(symbol, &day, 4)?,
                }),
                None => None,
            },
            case: None,
            rebuttal: None,
        });
    }
    let notes: BTreeMap<String, SectorNote> = report
        .newsbook
        .iter()
        .map(|n| (n.sector.clone(), n.clone()))
        .collect();
    for card in &mut report.cards {
        // Holdings are always argued. New candidates are skipped once most of the time is
        // spent, so the manager is never left without time to decide.
        if card.holding.is_none() && started.elapsed().as_secs_f64() > minutes * 60.0 * 0.75 {
            report.problems.push(format!(
                "Ran short of time before {} could be argued",
                card.symbol
            ));
            continue;
        }
        let note = notes.get(&card.sector);
        match agents::analyst(brain, card, note, &day) {
            Ok(case) => {
                match agents::skeptic(brain, card, &case, note, &day) {
                    Ok(rebuttal) => card.rebuttal = Some(rebuttal),
                    Err(error) => report
                        .problems
                        .push(format!("Skeptic failed on {}: {error}", card.symbol)),
                }
                card.case = Some(case);
            }
            Err(error) => report
                .problems
                .push(format!("Analyst failed on {}: {error}", card.symbol)),
        }
    }
    drop(stage);

    // 7. The manager decides.
    let stage = progress::begin("manager decides the book");
    let remembered = memory(store, &day)?;
    let book = Book {
        day: &day,
        focus: &report.focus,
        memory: &remembered,
        holdings: report
            .cards
            .iter()
            .filter(|c| c.holding.is_some())
            .collect(),
        candidates: report
            .cards
            .iter()
            .filter(|c| {
                c.holding.is_none() && c.case.as_ref().is_some_and(|k| k.catalyst != "none")
            })
            .collect(),
    };
    let mut problems = Vec::new();
    let mut sells: BTreeMap<String, String> = BTreeMap::new();
    let mut keeps: BTreeMap<String, String> = BTreeMap::new();
    match agents::review(brain, &book) {
        Ok(verdicts) => {
            for (symbol, (sell, reason)) in verdicts {
                if sell {
                    sells.insert(symbol, reason);
                } else {
                    keeps.insert(symbol, reason);
                }
            }
        }
        Err(error) => problems.push(format!(
            "The manager's review failed, so no holding was sold today: {error}"
        )),
    }
    // A young holding is sold only on bad news: buzz alone rises and falls day to day.
    for card in &book.holdings {
        let Some(holding) = &card.holding else {
            continue;
        };
        let bad_news = card
            .headlines
            .iter()
            .any(|h| h.label == "bad_news" && h.buzz >= 1);
        if holding.days_held < MIN_HOLD_DAYS
            && !bad_news
            && let Some(reason) = sells.remove(&card.symbol)
        {
            keeps.insert(
                card.symbol.clone(),
                format!(
                    "Held under the minimum-hold rule: owned {} days with no bad news, so fading buzz is not enough to sell yet. The manager had said: {reason}",
                    holding.days_held
                ),
            );
        }
    }
    let allowed: Vec<String> = book.candidates.iter().map(|c| c.symbol.clone()).collect();
    if allowed.is_empty() && keeps.is_empty() && !sells.is_empty() {
        problems.push("No candidate had a case today and nothing was kept, so the sales were not placed: the money would have sat in cash.".into());
        keeps.append(&mut sells);
    }
    let surplus = broker::sales_required(&state).saturating_sub(sells.len());
    if surplus > 0 && !keeps.is_empty() {
        let kept: Vec<String> = keeps.keys().cloned().collect();
        match agents::trim(brain, &book, &kept, surplus.min(kept.len())) {
            Ok(extra) => {
                for (symbol, reason) in extra {
                    keeps.remove(&symbol);
                    sells.insert(symbol, reason);
                }
            }
            Err(error) => problems.push(format!("The manager could not trim the book: {error}")),
        }
    }
    let needed = broker::purchases_needed(&state, sells.len()).min(allowed.len());
    let mut purchases: Vec<agents::Purchase> = Vec::new();
    for _ in 0..3 {
        if purchases.len() >= needed {
            break;
        }
        let chosen: Vec<String> = purchases.iter().map(|p| p.symbol.clone()).collect();
        let open: Vec<String> = allowed
            .iter()
            .filter(|s| !chosen.contains(s))
            .cloned()
            .collect();
        match agents::pick(brain, &book, &open, needed - purchases.len(), &chosen) {
            Ok(more) => purchases.extend(more),
            Err(error) => {
                problems.push(format!("The manager's purchase call failed: {error}"));
                break;
            }
        }
    }
    if purchases.len() < needed {
        problems.push(format!(
            "The manager named {} of the {needed} purchases needed",
            purchases.len()
        ));
    }
    let mut buys: BTreeMap<String, Order> = BTreeMap::new();
    for purchase in &purchases {
        let card = report.cards.iter().find(|c| c.symbol == purchase.symbol);
        let case = card.and_then(|c| c.case.as_ref());
        buys.insert(
            purchase.symbol.clone(),
            Order {
                side: "BUY".into(),
                at,
                reason: purchase.reason.clone(),
                weight: purchase.weight as f64,
                sector: card.map(|c| c.sector.clone()).unwrap_or_default(),
                thesis: case.map(|c| c.thesis.clone()).unwrap_or_default(),
                catalyst: case.map(|c| c.catalyst.clone()).unwrap_or_default(),
            },
        );
    }
    let topped = broker::queue(&mut state, &sells, &buys, at);
    let line = |symbol: &str, kind: &str, text: &str| Entry {
        day: day.clone(),
        at,
        symbol: symbol.into(),
        kind: kind.into(),
        text: text.into(),
    };
    for (symbol, reason) in &keeps {
        report.decisions.push(line(symbol, "KEEP", reason));
    }
    for (symbol, reason) in &sells {
        report.decisions.push(line(symbol, "SELL", reason));
    }
    for purchase in &purchases {
        report.decisions.push(line(
            &purchase.symbol,
            "BUY",
            &format!("{} (weight {})", purchase.reason, purchase.weight),
        ));
    }
    for symbol in &topped {
        report.decisions.push(line(
            symbol,
            "TOP_UP",
            "The book is full, so freed cash is added to this kept stock",
        ));
    }
    match agents::note(brain, &book, &report.decisions) {
        Ok(text) => report.note = text,
        Err(error) => problems.push(format!("The manager wrote no note: {error}")),
    }
    report.problems.extend(problems);
    drop(stage);
    finish(store, &mut state, report, Some(brain), started)
}

/// Write the day to memory: journal, transcripts, report and state, in one transaction.
fn finish(
    store: &Store,
    state: &mut State,
    mut report: Report,
    brain: Option<&dyn Brain>,
    started: Instant,
) -> Result<Report> {
    let calls = brain.map(Brain::take_calls).unwrap_or_default();
    report.funnel.calls = calls.len();
    report.funnel.failed_calls = calls.iter().filter(|c| !c.ok).count();
    report.funnel.minutes = started.elapsed().as_secs_f64() / 60.0;
    state.health = json!({
        "status": if report.problems.is_empty() { "ok" } else { "degraded" },
        "at": report.at,
        "problems": report.problems.len(),
    });
    store.begin()?;
    store.clear_journal(&report.day)?;
    if !report.focus.sectors.is_empty() {
        let names: Vec<&str> = report
            .focus
            .sectors
            .iter()
            .map(|s| sectors::name_of(s))
            .collect();
        store.journal(&Entry {
            day: report.day.clone(),
            at: report.at,
            symbol: String::new(),
            kind: "FOCUS".into(),
            text: format!("{}. {}", names.join("; "), report.focus.thesis),
        })?;
    }
    for entry in &report.decisions {
        store.journal(entry)?;
    }
    if !report.note.is_empty() {
        store.journal(&Entry {
            day: report.day.clone(),
            at: report.at,
            symbol: String::new(),
            kind: "NOTE".into(),
            text: report.note.clone(),
        })?;
    }
    store.record_calls(&report.day, &calls)?;
    store.put_report(&report.day, report.at, &serde_json::to_string(&report)?)?;
    store.save(state)?;
    store.commit()?;
    store.prune(&report.day)?;
    Ok(report)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agents::testing::Scripted;
    use crate::config::{FEE, MAX_POSITIONS};
    use crate::market::Bar;
    use crate::store::Position;
    use serde_json::Value;

    const AT: f64 = 1_794_605_856.0; // Friday 13 November 2026, 21:37 UTC
    const DAY: &str = "2026-11-13";

    struct Fixture {
        listings: Vec<Listing>,
    }

    fn stock(symbol: &str, sector: &str, industry: &str, change: f64) -> Listing {
        Listing {
            symbol: symbol.into(),
            name: format!("{symbol} Corp Common Stock"),
            price: 50.0,
            change,
            volume: 4.0e6,
            market_cap: 2.0e10,
            sector: sector.into(),
            industry: industry.into(),
        }
    }

    fn bars(last_close: f64, last_volume: f64) -> Quote {
        Quote {
            bars: (0..30)
                .map(|i| Bar {
                    at: AT - 86400.0 * f64::from(30 - i),
                    open: 50.0,
                    close: if i == 29 { last_close } else { 50.0 },
                    volume: if i == 29 { last_volume } else { 1.0e6 },
                })
                .collect(),
            actions: Vec::new(),
            instrument_type: Some("EQUITY".into()),
        }
    }

    impl Feed for Fixture {
        fn listings(&self) -> Result<Vec<Listing>> {
            Ok(self.listings.clone())
        }
        fn funds(&self) -> BTreeSet<String> {
            ["FUND".to_string()].into()
        }
        fn attention(&self) -> (BTreeMap<String, Attention>, Vec<SourceNote>) {
            let mut map = BTreeMap::new();
            map.insert(
                "CHIPA".to_string(),
                Attention {
                    reddit: 400,
                    reddit_before: 100,
                    trending: vec!["Yahoo".into()],
                },
            );
            (
                map,
                vec![SourceNote {
                    name: "fixture".into(),
                    ok: true,
                    detail: String::new(),
                }],
            )
        }
        fn histories(
            &self,
            symbols: &BTreeSet<String>,
        ) -> (BTreeMap<String, Quote>, BTreeMap<String, String>) {
            let mut quotes = BTreeMap::new();
            for symbol in symbols {
                let mut quote = bars(52.0, 3.0e6);
                if symbol == "FUND" {
                    quote.instrument_type = Some("ETF".into());
                }
                quotes.insert(symbol.clone(), quote);
            }
            (quotes, BTreeMap::new())
        }
        fn ticker_news(&self, symbols: &BTreeSet<String>) -> BTreeMap<String, Vec<Headline>> {
            symbols
                .iter()
                .map(|s| {
                    (
                        s.clone(),
                        vec![Headline {
                            title: format!("{s} lands record AI order"),
                            source: "Reuters".into(),
                            published: AT - 3600.0,
                            link: String::new(),
                        }],
                    )
                })
                .collect()
        }
        fn topic_news(&self, query: &str) -> (Vec<Headline>, &'static str) {
            (
                vec![Headline {
                    title: format!("Sector news for {query}"),
                    source: "AP".into(),
                    published: AT - 7200.0,
                    link: String::new(),
                }],
                "fixture",
            )
        }
    }

    fn fixture() -> Fixture {
        let mut listings = vec![
            stock("CHIPA", "Technology", "Semiconductors", 8.0),
            stock("CHIPB", "Technology", "Semiconductors", 6.0),
            stock("CHIPC", "Technology", "Semiconductors", 5.0),
            stock("CHIPD", "Technology", "Semiconductors", 4.0),
            stock("CHIPE", "Technology", "Semiconductors", 3.0),
            stock("CHIPF", "Technology", "Semiconductors", 2.0),
            stock("BANKA", "Finance", "Major Banks", 9.0),
            stock(
                "OLDA",
                "Industrials",
                "Industrial Machinery/Components",
                0.1,
            ),
            stock(
                "OLDB",
                "Industrials",
                "Industrial Machinery/Components",
                -0.2,
            ),
            stock("TINY", "Technology", "Semiconductors", 30.0),
        ];
        listings[9].market_cap = 1.0e8;
        listings.push(Listing {
            symbol: "FUND".into(),
            name: "Some Chip ETF".into(),
            ..stock("FUND", "", "", 12.0)
        });
        Fixture { listings }
    }

    /// A desk that likes chips, sells whatever has "OLD" in its name and buys in list order.
    fn brain() -> Scripted {
        Scripted::new(|agent, subject, _, schema| {
            Ok(match (agent, subject) {
                (Agent::Scout, sector) => json!({
                    "entry": format!("Entry for {sector}."),
                    "driver": "driver",
                    "heat": if sector == "chips" { 9 } else { 3 },
                    "outlook": "bullish",
                }),
                (Agent::Strategist, _) => json!({"focus": ["chips"], "thesis": "Chips are hot."}),
                (Agent::Classifier, _) => {
                    let n = schema["properties"]["labels"]["minItems"].as_u64().unwrap();
                    let labels: Vec<Value> = (1..=n)
                        .map(|i| json!({"n": i, "label": "big_deal", "buzz": 3}))
                        .collect();
                    json!({"labels": labels})
                }
                (Agent::Analyst, symbol) => json!({
                    "catalyst": "big_deal",
                    "thesis": format!("{symbol} has a live order."),
                    "upside": 7,
                    "kill": "Order cancelled",
                }),
                (Agent::Skeptic, _) => {
                    json!({"objection": "Late.", "priced_in": false, "verdict": "stands"})
                }
                (Agent::Manager, "holdings") => {
                    let mut out = serde_json::Map::new();
                    for symbol in schema["required"].as_array().unwrap() {
                        let symbol = symbol.as_str().unwrap();
                        let action = if symbol.starts_with("OLD") {
                            "SELL"
                        } else {
                            "KEEP"
                        };
                        out.insert(
                            symbol.into(),
                            json!({"reason": "because", "action": action}),
                        );
                    }
                    Value::Object(out)
                }
                (Agent::Manager, "purchases") => {
                    let items = &schema["properties"]["buys"];
                    let n = items["minItems"].as_u64().unwrap() as usize;
                    let allowed = items["items"]["properties"]["symbol"]["enum"]
                        .as_array()
                        .unwrap();
                    let buys: Vec<Value> = allowed
                        .iter()
                        .take(n)
                        .map(|s| json!({"symbol": s, "reason": "best case", "weight": 2}))
                        .collect();
                    json!({"buys": buys})
                }
                (Agent::Manager, "trim") => {
                    let items = &schema["properties"]["sell"];
                    let n = items["minItems"].as_u64().unwrap() as usize;
                    let kept = items["items"]["properties"]["symbol"]["enum"]
                        .as_array()
                        .unwrap();
                    let sell: Vec<Value> = kept
                        .iter()
                        .take(n)
                        .map(|s| json!({"symbol": s, "reason": "weakest"}))
                        .collect();
                    json!({"sell": sell})
                }
                (Agent::Manager, _) => json!({"note": "Betting on chips."}),
            })
        })
    }

    fn held(symbol: &str) -> Position {
        Position {
            symbol: symbol.into(),
            quantity: 10.0,
            entry: 50.0,
            cost: 500.0 * (1.0 + FEE),
            opened_at: AT - 5.0 * 86400.0,
            ..Position::default()
        }
    }

    #[test]
    fn a_fresh_account_is_fully_invested_by_the_models() {
        let store = Store::open(":memory:").unwrap();
        let brain = brain();
        let report = run(&store, Some(&brain), &fixture(), AT, 240.0).unwrap();
        assert_eq!(report.day, DAY);
        assert_eq!(report.funnel.listed, 11);
        assert_eq!(
            report.funnel.eligible, 9,
            "the fund and the small company are out"
        );
        assert_eq!(report.newsbook.len(), SECTORS.len());
        assert_eq!(report.newsbook[0].sector, "chips");
        assert_eq!(report.focus.sectors, ["chips"]);
        // The six chip stocks come first, then the day's most-buzzed others; never the fund.
        assert_eq!(report.cards.len(), 9);
        assert!(report.cards[..6].iter().all(|c| c.sector == "chips"));
        assert!(
            report
                .cards
                .iter()
                .all(|c| c.symbol != "FUND" && c.case.is_some() && c.rebuttal.is_some())
        );
        assert_eq!(
            report.cards[0].symbol, "CHIPA",
            "the most talked-about stock comes first"
        );
        assert_eq!(report.cards[0].headlines[0].label, "big_deal");
        let buys: Vec<&str> = report
            .decisions
            .iter()
            .filter(|d| d.kind == "BUY")
            .map(|d| d.symbol.as_str())
            .collect();
        assert_eq!(buys.len(), MAX_POSITIONS);
        let state = store.load().unwrap();
        assert_eq!(state.engine, ENGINE);
        assert_eq!(state.pending.len(), MAX_POSITIONS);
        assert!(
            state
                .pending
                .values()
                .all(|o| o.side == "BUY" && o.catalyst == "big_deal")
        );
        assert_eq!(report.note, "Betting on chips.");
        assert!(report.problems.is_empty(), "{:?}", report.problems);
        assert_eq!(store.reports(1).unwrap()[0].0, DAY);
        assert_eq!(store.journal_since(DAY).unwrap().len(), MAX_POSITIONS + 2);
    }

    #[test]
    fn every_holding_is_decided_and_each_sale_is_replaced() {
        let store = Store::open(":memory:").unwrap();
        let mut state = State {
            cash: 0.0,
            engine: ENGINE.into(),
            ..State::default()
        };
        for symbol in ["OLDA", "OLDB", "CHIPA"] {
            state.positions.insert(symbol.into(), held(symbol));
        }
        store.save(&state).unwrap();
        let brain = brain();
        let report = run(&store, Some(&brain), &fixture(), AT, 240.0).unwrap();
        let kinds = |kind: &str| -> Vec<String> {
            report
                .decisions
                .iter()
                .filter(|d| d.kind == kind)
                .map(|d| d.symbol.clone())
                .collect()
        };
        assert_eq!(kinds("KEEP"), ["CHIPA"]);
        assert_eq!(kinds("SELL"), ["OLDA", "OLDB"]);
        assert_eq!(
            kinds("BUY").len(),
            6,
            "two sales free money; every argued candidate is bought on the way to eight"
        );
        assert!(!kinds("BUY").contains(&"CHIPA".to_string()));
        let state = store.load().unwrap();
        assert_eq!(
            state.pending.values().filter(|o| o.side == "SELL").count(),
            2
        );
        assert_eq!(
            state.pending.values().filter(|o| o.side == "BUY").count(),
            6
        );
        // The holdings were argued too.
        assert_eq!(brain.count(Agent::Analyst), 3 + 5 + 1);
        // The next day's open fills everything and leaves no cash.
        let next = AT + 86400.0;
        let mut quotes = BTreeMap::new();
        for symbol in state.pending.keys().chain(state.positions.keys()) {
            let mut quote = bars(52.0, 1.0e6);
            quote.bars.push(Bar {
                at: next - 3600.0,
                open: 53.0,
                close: 54.0,
                volume: 1.0e6,
            });
            quotes.insert(symbol.clone(), quote);
        }
        let mut state = state;
        broker::settle(&store, &mut state, &quotes, next).unwrap();
        assert!(state.cash.abs() < 1e-6);
        assert_eq!(state.positions.len(), 7);
        assert!(state.pending.is_empty());
        let equity = state.equity();
        assert!(
            state
                .positions
                .values()
                .filter(|p| p.symbol != "CHIPA")
                .all(|p| p.cost <= 0.25 * equity + 1e-6)
        );
    }

    #[test]
    fn the_takeover_cancels_old_orders_and_trims_an_oversized_book() {
        let store = Store::open(":memory:").unwrap();
        let mut state = State {
            cash: 0.0,
            ..State::default()
        };
        for i in 1..=MAX_POSITIONS + 1 {
            let symbol = format!("HELD{i}");
            state.positions.insert(symbol.clone(), held(&symbol));
        }
        state.pending.insert(
            "HELD1".into(),
            Order {
                side: "SELL".into(),
                at: AT - 86400.0 * 3.0,
                ..Order::default()
            },
        );
        store.save(&state).unwrap();
        let brain = brain();
        let report = run(&store, Some(&brain), &fixture(), AT, 240.0).unwrap();
        let state = store.load().unwrap();
        assert!(
            state.positions.contains_key("HELD1"),
            "the old desk's sale was cancelled, not filled"
        );
        assert!(store.events(5).unwrap().iter().any(|e| e.1 == "takeover"));
        let sells = report.decisions.iter().filter(|d| d.kind == "SELL").count();
        assert_eq!(sells, 1, "nine holdings are cut to eight");
        assert_eq!(brain.count(Agent::Manager), 3, "review, trim and note");
        // Nothing new is bought, so the freed cash goes to the eight that were kept.
        assert_eq!(
            report
                .decisions
                .iter()
                .filter(|d| d.kind == "TOP_UP")
                .count(),
            MAX_POSITIONS
        );
        assert_eq!(state.pending.len(), MAX_POSITIONS + 1);
    }

    #[test]
    fn a_young_holding_is_kept_unless_bad_news_broke_it() {
        let store = Store::open(":memory:").unwrap();
        let mut state = State {
            cash: 0.0,
            engine: ENGINE.into(),
            ..State::default()
        };
        let mut young = held("OLDA");
        young.opened_at = AT - 86400.0;
        state.positions.insert("OLDA".into(), young);
        state.positions.insert("OLDB".into(), held("OLDB"));
        store.save(&state).unwrap();
        let brain = brain();
        let report = run(&store, Some(&brain), &fixture(), AT, 240.0).unwrap();
        let keep = report
            .decisions
            .iter()
            .find(|d| d.symbol == "OLDA")
            .unwrap();
        assert_eq!(keep.kind, "KEEP");
        assert!(keep.text.contains("minimum-hold"));
        let sell = report
            .decisions
            .iter()
            .find(|d| d.symbol == "OLDB")
            .unwrap();
        assert_eq!(
            sell.kind, "SELL",
            "a holding past the minimum is sold as decided"
        );
    }

    #[test]
    fn only_stocks_the_analyst_made_a_case_for_can_be_bought() {
        let store = Store::open(":memory:").unwrap();
        let inner = brain();
        let brain = Scripted::new(move |agent, subject, user, schema| {
            if agent == Agent::Analyst && subject != "CHIPA" {
                return Ok(
                    json!({"catalyst": "none", "thesis": "Nothing new.", "upside": 2, "kill": "n/a"}),
                );
            }
            inner.ask(agent, subject, "", user, schema)
        });
        let report = run(&store, Some(&brain), &fixture(), AT, 240.0).unwrap();
        let buys: Vec<&str> = report
            .decisions
            .iter()
            .filter(|d| d.kind == "BUY")
            .map(|d| d.symbol.as_str())
            .collect();
        assert_eq!(buys, ["CHIPA"]);
        assert_eq!(store.load().unwrap().pending.len(), 1);
    }

    #[test]
    fn without_a_model_nothing_is_decided() {
        let store = Store::open(":memory:").unwrap();
        let report = run(&store, None, &fixture(), AT, 240.0).unwrap();
        assert!(report.decisions.is_empty());
        assert_eq!(report.problems.len(), 1);
        let state = store.load().unwrap();
        assert!(state.pending.is_empty());
        assert_eq!(state.health["status"], "degraded");
        assert_eq!(store.equity_curve().unwrap().len(), 1);
    }

    #[test]
    fn a_failing_strategist_falls_back_to_the_scouts_heat() {
        let store = Store::open(":memory:").unwrap();
        let inner = brain();
        let brain = Scripted::new(move |agent, subject, user, schema| {
            if agent == Agent::Strategist {
                anyhow::bail!("timeout");
            }
            inner.ask(agent, subject, "", user, schema)
        });
        let report = run(&store, Some(&brain), &fixture(), AT, 240.0).unwrap();
        assert_eq!(report.focus.sectors[0], "chips");
        assert!(
            report
                .problems
                .iter()
                .any(|p| p.contains("Strategist failed"))
        );
        assert_eq!(store.load().unwrap().health["status"], "degraded");
    }

    #[test]
    fn the_tape_summarises_a_sector() {
        let a = stock("A", "", "", 4.0);
        let b = stock("B", "", "", -1.0);
        let c = stock("C", "", "", 1.0);
        assert_eq!(
            tape(&[&a, &b, &c]),
            "3 stocks, median move +1.0%, 66% rose. Biggest gainers: A +4.0%, C +1.0%, B -1.0%"
        );
        assert_eq!(tape(&[]), "no eligible stocks");
    }
}
