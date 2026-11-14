//! The six agents: what each one is told, what it is shown and what it must answer.
//!
//! Every answer is JSON held to a schema by the model server, so a choice is always one of
//! the allowed values: a real sector, a stock on the list, KEEP or SELL.

use crate::llm::Brain;
use crate::models::Agent;
use crate::sectors::{self, SECTORS};
use crate::store::{Entry, SectorNote, Tagged};
use anyhow::{Result, anyhow, bail};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;

/// The kinds of catalyst a headline can be, with what each means.
pub const CATALYSTS: [(&str, &str); 12] = [
    (
        "product_launch",
        "a new product, model, drug or service people are excited about",
    ),
    (
        "big_deal",
        "a major contract, partnership, investment or customer win",
    ),
    (
        "earnings_beat",
        "results well above expectations or guidance raised",
    ),
    (
        "upgrade",
        "an analyst upgrade or a sharply higher price target",
    ),
    (
        "approval",
        "a regulatory or FDA approval, or a decisive court win",
    ),
    (
        "policy",
        "legislation, government spending, tariffs or rules that help the business",
    ),
    ("takeover", "a merger, acquisition or buyout approach"),
    (
        "retail_frenzy",
        "viral retail attention, a short squeeze or meme momentum",
    ),
    (
        "demand_boom",
        "surging demand, sales, prices or an industry-wide boom",
    ),
    (
        "bad_news",
        "a miss, guidance cut, downgrade, probe, recall, dilution or serious lawsuit",
    ),
    (
        "noise",
        "executive changes, routine filings, dividends, conference appearances, market wraps, opinion pieces, lists",
    ),
    ("unrelated", "not about this company"),
];

/// Catalyst labels that are reasons to buy.
pub fn bullish_labels() -> Vec<&'static str> {
    CATALYSTS
        .iter()
        .map(|(label, _)| *label)
        .filter(|l| !matches!(*l, "bad_news" | "noise" | "unrelated"))
        .collect()
}

/// What the tape and the crowd say about one stock.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Facts {
    pub price: f64,
    pub market_cap: f64,
    pub change_1d: f64,
    pub change_5d: Option<f64>,
    pub change_20d: Option<f64>,
    pub relative_volume: Option<f64>,
    pub below_high: Option<f64>,
    pub reddit: u32,
    pub reddit_before: u32,
    pub trending: Vec<String>,
    pub buzz: f64,
}

/// The analyst's case for owning a stock.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Case {
    pub catalyst: String,
    pub thesis: String,
    /// 1 to 10.
    pub upside: i64,
    /// What would prove the case wrong.
    pub kill: String,
}

/// The skeptic's answer to the case.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Rebuttal {
    pub objection: String,
    pub priced_in: bool,
    /// `stands`, `weakened` or `broken`.
    pub verdict: String,
}

/// What the account already has riding on a stock.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Holding {
    pub cost: f64,
    pub market_value: f64,
    pub pnl_pct: f64,
    pub days_held: i64,
    /// The case made when it was bought.
    pub thesis: String,
    /// Earlier decisions about it, newest first.
    pub history: Vec<Entry>,
}

/// Everything the desk knows about one stock today.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Card {
    pub symbol: String,
    pub name: String,
    /// Newsbook sector id.
    pub sector: String,
    pub facts: Facts,
    pub headlines: Vec<Tagged>,
    pub holding: Option<Holding>,
    pub case: Option<Case>,
    pub rebuttal: Option<Rebuttal>,
}

fn text(value: &Value, key: &str) -> String {
    value[key]
        .as_str()
        .unwrap_or("")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

fn string(max: u32) -> Value {
    json!({"type": "string", "minLength": 1, "maxLength": max})
}

fn object(properties: &Value, required: &[&str]) -> Value {
    json!({"type": "object", "properties": properties, "required": required, "additionalProperties": false})
}

fn signed(value: f64) -> String {
    format!("{value:+.1}%")
}

fn money(value: f64) -> String {
    if value >= 1.0e12 {
        format!("${:.1} trillion", value / 1.0e12)
    } else if value >= 1.0e9 {
        format!("${:.1} billion", value / 1.0e9)
    } else {
        format!("${:.0} million", value / 1.0e6)
    }
}

/// The numbers about a stock in one or two lines.
pub fn facts_line(facts: &Facts) -> String {
    let mut line = format!(
        "Price ${:.2}, company worth {}. Latest session {}",
        facts.price,
        money(facts.market_cap),
        signed(facts.change_1d)
    );
    if let Some(v) = facts.change_5d {
        let _ = write!(line, ", 5 sessions {}", signed(v));
    }
    if let Some(v) = facts.change_20d {
        let _ = write!(line, ", 20 sessions {}", signed(v));
    }
    if let Some(v) = facts.relative_volume {
        let _ = write!(line, ". Volume {v:.1}x its 20-session average");
    }
    if let Some(v) = facts.below_high {
        let _ = write!(line, ". {v:.0}% below its 3-month high");
    }
    line.push_str(". ");
    if facts.reddit > 0 || facts.reddit_before > 0 {
        let _ = write!(
            line,
            "Reddit mentions in the last 24 hours: {} (the day before: {}). ",
            facts.reddit, facts.reddit_before
        );
    } else {
        line.push_str("No notable Reddit chatter. ");
    }
    if !facts.trending.is_empty() {
        let _ = write!(line, "Trending on {}.", facts.trending.join(" and "));
    }
    line.trim().to_string()
}

fn headline_lines(headlines: &[Tagged], today: &str) -> String {
    if headlines.is_empty() {
        return "  (no headlines found)\n".into();
    }
    let mut out = String::new();
    for h in headlines {
        let when = if h.day == today {
            "today"
        } else {
            h.day.as_str()
        };
        let label = if h.label.is_empty() {
            String::new()
        } else {
            format!(" [{}, buzz {}/3]", h.label, h.buzz)
        };
        let source = if h.source.is_empty() {
            String::new()
        } else {
            format!(" ({})", h.source)
        };
        let _ = writeln!(out, "  - {when}{label}: {}{source}", h.title);
    }
    out
}

/// The full file on a stock, as the analyst and the skeptic read it.
pub fn dossier(card: &Card, sector_note: Option<&SectorNote>, today: &str) -> String {
    let mut out = format!(
        "STOCK: {} ({})\nSECTOR: {}\n",
        card.symbol,
        card.name,
        sectors::name_of(&card.sector)
    );
    if let Some(note) = sector_note {
        let _ = writeln!(
            out,
            "SECTOR NEWSBOOK TODAY (heat {}/10, {}): {}",
            note.heat, note.outlook, note.entry
        );
    }
    let _ = writeln!(out, "NUMBERS: {}", facts_line(&card.facts));
    let _ = write!(
        out,
        "HEADLINES (first seen on the day shown; the label and buzz score are the classifier's):\n{}",
        headline_lines(&card.headlines, today)
    );
    if let Some(holding) = &card.holding {
        let _ = writeln!(
            out,
            "THE ACCOUNT OWNS THIS STOCK: held {} days, ${:.2} paid, now worth ${:.2} ({}).",
            holding.days_held,
            holding.cost,
            holding.market_value,
            signed(holding.pnl_pct)
        );
        if !holding.thesis.is_empty() {
            let _ = writeln!(out, "Why it was bought: {}", holding.thesis);
        }
        for entry in &holding.history {
            let _ = writeln!(
                out,
                "  {} the desk said {}: {}",
                entry.day, entry.kind, entry.text
            );
        }
    }
    out
}

/// A stock in a few lines, as the manager reads it.
pub fn summary(card: &Card) -> String {
    let mut out = format!(
        "{} ({}), {}. {}\n",
        card.symbol,
        card.name,
        sectors::name_of(&card.sector),
        facts_line(&card.facts)
    );
    if let Some(holding) = &card.holding {
        let _ = writeln!(
            out,
            "  Owned {} days, {} so far. Bought because: {}",
            holding.days_held,
            signed(holding.pnl_pct),
            if holding.thesis.is_empty() {
                "(bought by the previous desk on a momentum ranking, with no news case)"
            } else {
                &holding.thesis
            }
        );
        for entry in holding.history.iter().take(3) {
            let _ = writeln!(
                out,
                "  {} you said {}: {}",
                entry.day, entry.kind, entry.text
            );
        }
    }
    let fresh: Vec<&Tagged> = card
        .headlines
        .iter()
        .filter(|h| h.buzz >= 2 && !matches!(h.label.as_str(), "noise" | "unrelated"))
        .take(3)
        .collect();
    for h in fresh {
        let _ = writeln!(out, "  News ({}, {}): {}", h.day, h.label, h.title);
    }
    match &card.case {
        Some(case) => {
            let _ = writeln!(
                out,
                "  Analyst (upside {}/10, catalyst {}): {} Wrong if: {}",
                case.upside, case.catalyst, case.thesis, case.kill
            );
        }
        None => out.push_str("  Analyst: no case was made.\n"),
    }
    if let Some(rebuttal) = &card.rebuttal {
        let _ = writeln!(
            out,
            "  Skeptic (case {}{}): {}",
            rebuttal.verdict,
            if rebuttal.priced_in {
                ", move already priced in"
            } else {
                ""
            },
            rebuttal.objection
        );
    }
    out
}

const SCOUT: &str = "You keep one sector's page in a trading desk's newsbook. Each day you write what happened in the sector that could move its stocks over the next few days: laws passed or moving through, government money, demand, prices, big deals, product launches, approvals, and waves of investor excitement. Name companies and tickers when the headlines do. Compare with your earlier entries and say what is new, what is still going, and what has faded. Ignore executive changes, routine filings, dividends and generic market commentary. Use only what you are shown. Heat is how much is happening for investors: 0 is nothing, 5 is a normal busy day, 10 is the story everyone is talking about.";

/// What a scout is shown.
#[derive(Clone, Debug, Default)]
pub struct ScoutBrief {
    pub day: String,
    pub sector: String,
    /// How the sector's stocks traded, in one line.
    pub tape: String,
    /// The sector's most-buzzed stocks, one per line.
    pub names: Vec<String>,
    pub headlines: Vec<String>,
    /// Earlier entries, newest first.
    pub history: Vec<SectorNote>,
}

/// Write one sector's newsbook entry.
pub fn scout(brain: &dyn Brain, brief: &ScoutBrief) -> Result<SectorNote> {
    let mut user = format!(
        "SECTOR: {}\nDATE: {}\nTAPE: {}\n\nMOST TALKED-ABOUT STOCKS IN THE SECTOR TODAY:\n",
        sectors::name_of(&brief.sector),
        brief.day,
        brief.tape
    );
    if brief.names.is_empty() {
        user.push_str("  (none stood out)\n");
    }
    for name in &brief.names {
        let _ = writeln!(user, "  - {name}");
    }
    user.push_str("\nHEADLINES FROM THE LAST TWO DAYS:\n");
    if brief.headlines.is_empty() {
        user.push_str("  (no headlines found)\n");
    }
    for headline in &brief.headlines {
        let _ = writeln!(user, "  - {headline}");
    }
    user.push_str("\nYOUR EARLIER ENTRIES:\n");
    if brief.history.is_empty() {
        user.push_str("  (none: this is the first entry)\n");
    }
    for note in &brief.history {
        let _ = writeln!(
            user,
            "  {} (heat {}, {}): {}",
            note.day, note.heat, note.outlook, note.entry
        );
    }
    user.push_str("\nWrite today's entry.");
    let schema = object(
        &json!({
            "entry": string(480),
            "driver": string(120),
            "heat": {"type": "integer", "minimum": 0, "maximum": 10},
            "outlook": {"type": "string", "enum": ["bullish", "neutral", "bearish"]},
        }),
        &["entry", "driver", "heat", "outlook"],
    );
    let answer = brain.ask(Agent::Scout, &brief.sector, SCOUT, &user, &schema)?;
    let entry = text(&answer, "entry");
    if entry.is_empty() {
        bail!("the scout wrote nothing");
    }
    Ok(SectorNote {
        day: brief.day.clone(),
        sector: brief.sector.clone(),
        heat: answer["heat"].as_i64().unwrap_or(0).clamp(0, 10),
        outlook: text(&answer, "outlook"),
        entry,
        driver: text(&answer, "driver"),
    })
}

const STRATEGIST: &str = "You set sector strategy for a small stock account that trades news buzz and is always fully invested. Read the sector newsbook and choose the one to three sectors where excitement is strongest and most likely to keep pushing prices up over the next few days. Prefer sectors with a concrete driver, such as a law, a spending wave, a product cycle or a wave of deals, over sectors that are merely busy. Concentrate: fewer sectors is better. Your earlier choices are shown so you can stay with a story that is working or leave one that has faded.";

/// The strategist's choice.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Focus {
    pub sectors: Vec<String>,
    pub thesis: String,
}

/// Choose the sectors to trade from today's newsbook.
pub fn strategist(
    brain: &dyn Brain,
    day: &str,
    notes: &[SectorNote],
    tapes: &BTreeMap<String, String>,
    held: &BTreeMap<String, Vec<String>>,
    earlier: &[Entry],
    max: usize,
) -> Result<Focus> {
    let mut user = format!("DATE: {day}\n\nTODAY'S NEWSBOOK:\n");
    for note in notes {
        let _ = writeln!(
            user,
            "- {} [id {}]: heat {}/10, {}. Driver: {}. {}\n  Tape: {}",
            sectors::name_of(&note.sector),
            note.sector,
            note.heat,
            note.outlook,
            note.driver,
            note.entry,
            tapes.get(&note.sector).map_or("unknown", String::as_str)
        );
    }
    user.push_str("\nWHAT THE ACCOUNT HOLDS NOW:\n");
    if held.is_empty() {
        user.push_str("  (nothing)\n");
    }
    for (sector, symbols) in held {
        let _ = writeln!(
            user,
            "  {}: {}",
            sectors::name_of(sector),
            symbols.join(", ")
        );
    }
    user.push_str("\nYOUR EARLIER CHOICES:\n");
    if earlier.is_empty() {
        user.push_str("  (none yet)\n");
    }
    for entry in earlier {
        let _ = writeln!(user, "  {}: {}", entry.day, entry.text);
    }
    let _ = write!(
        user,
        "\nChoose 1 to {max} sector ids and explain the choice in two or three sentences."
    );
    let ids: Vec<&str> = notes.iter().map(|n| n.sector.as_str()).collect();
    if ids.is_empty() {
        bail!("no newsbook entries to choose from");
    }
    let schema = object(
        &json!({
            "thesis": string(500),
            "focus": {"type": "array", "minItems": 1, "maxItems": max, "items": {"type": "string", "enum": ids}},
        }),
        &["thesis", "focus"],
    );
    let answer = brain.ask(Agent::Strategist, "sectors", STRATEGIST, &user, &schema)?;
    let mut chosen = Vec::new();
    for id in answer["focus"].as_array().into_iter().flatten() {
        if let Some(id) = id.as_str()
            && ids.contains(&id)
            && !chosen.contains(&id.to_string())
            && chosen.len() < max
        {
            chosen.push(id.to_string());
        }
    }
    if chosen.is_empty() {
        bail!("the strategist chose no sector");
    }
    Ok(Focus {
        sectors: chosen,
        thesis: text(&answer, "thesis"),
    })
}

const CLASSIFIER: &str = "You label news headlines for a trading desk that buys stocks on fresh buzz. For each numbered headline about the named company, choose the one label that fits and score its buzz from 0 to 3: 0 is nothing an investor would act on, 1 is minor, 2 is a real catalyst, 3 is a major catalyst that people will be talking about. Judge only what the headline says. A headline about another company, or about the market in general, is unrelated.";

/// Label a batch of one stock's headlines. Returns `(headline id, label, buzz)`.
pub fn classify(
    brain: &dyn Brain,
    symbol: &str,
    company: &str,
    batch: &[Tagged],
) -> Result<Vec<(i64, String, i64)>> {
    let mut user = format!("COMPANY: {company} (ticker {symbol})\n\nLABELS:\n");
    for (label, meaning) in CATALYSTS {
        let _ = writeln!(user, "  {label}: {meaning}");
    }
    user.push_str("\nHEADLINES:\n");
    for (i, h) in batch.iter().enumerate() {
        let _ = writeln!(user, "  {}. {}", i + 1, h.title);
    }
    let _ = write!(user, "\nLabel all {} headlines in order.", batch.len());
    let labels: Vec<&str> = CATALYSTS.iter().map(|(l, _)| *l).collect();
    let schema = object(
        &json!({"labels": {
            "type": "array", "minItems": batch.len(), "maxItems": batch.len(),
            "items": object(
                &json!({
                    "n": {"type": "integer", "minimum": 1, "maximum": batch.len()},
                    "label": {"type": "string", "enum": labels},
                    "buzz": {"type": "integer", "minimum": 0, "maximum": 3},
                }),
                &["n", "label", "buzz"],
            ),
        }}),
        &["labels"],
    );
    let answer = brain.ask(Agent::Classifier, symbol, CLASSIFIER, &user, &schema)?;
    let mut out = Vec::new();
    let mut seen = BTreeSet::new();
    for (position, item) in answer["labels"]
        .as_array()
        .into_iter()
        .flatten()
        .enumerate()
    {
        let n = item["n"].as_u64().map_or(position + 1, |n| n as usize);
        let Some(headline) = n.checked_sub(1).and_then(|i| batch.get(i)) else {
            continue;
        };
        let label = text(item, "label");
        if !labels.contains(&label.as_str()) || !seen.insert(headline.id) {
            continue;
        }
        let buzz = if matches!(label.as_str(), "noise" | "unrelated") {
            0
        } else {
            item["buzz"].as_i64().unwrap_or(0).clamp(0, 3)
        };
        out.push((headline.id, label, buzz));
    }
    if out.is_empty() {
        bail!("the classifier labelled nothing");
    }
    Ok(out)
}

const ANALYST: &str = "You are the analyst on a trading desk that buys stocks on news buzz. Make the strongest honest case for owning this stock over the next few days. What counts: a fresh catalyst people are excited about, such as a product launch, a big deal, a policy tailwind, a results beat, an approval, a takeover approach, a demand boom or a surge of retail attention, and signs the crowd is arriving, such as rising Reddit mentions, trending lists and unusual volume. What does not count: executive changes, routine filings, dividends, conference appearances, or the mere fact that the price has been rising. An upcoming earnings date is neither a reason to buy nor a reason to avoid. Use only the facts in the file and never invent news. If the file holds no real catalyst, say so plainly, choose catalyst none and score the upside low. Upside is 1 to 10: how likely and how far the stock rises over the next few days.";

fn case_schema() -> Value {
    let mut labels = bullish_labels();
    labels.push("none");
    object(
        &json!({
            "catalyst": {"type": "string", "enum": labels},
            "thesis": string(420),
            "upside": {"type": "integer", "minimum": 1, "maximum": 10},
            "kill": string(180),
        }),
        &["catalyst", "thesis", "upside", "kill"],
    )
}

/// Make the case for one stock.
pub fn analyst(
    brain: &dyn Brain,
    card: &Card,
    note: Option<&SectorNote>,
    today: &str,
) -> Result<Case> {
    let user = format!(
        "{}\nState the main catalyst, the case in two or three sentences, the upside score, and in `kill` the one development that would prove the case wrong.",
        dossier(card, note, today)
    );
    let answer = brain.ask(Agent::Analyst, &card.symbol, ANALYST, &user, &case_schema())?;
    let thesis = text(&answer, "thesis");
    if thesis.is_empty() {
        bail!("the analyst wrote no case");
    }
    Ok(Case {
        catalyst: text(&answer, "catalyst"),
        thesis,
        upside: answer["upside"].as_i64().unwrap_or(1).clamp(1, 10),
        kill: text(&answer, "kill"),
    })
}

const SKEPTIC: &str = "You are the skeptic on a trading desk that buys stocks on news buzz. The analyst has made a case for a stock. Attack it using the same file. Is the catalyst real, about this company, and fresh, or is it old news? Has the price already made the move, so a buyer today is late? Is the buzz fading compared with yesterday? Did the analyst lean on something the file does not say? Give the single strongest objection. Then say whether the case stands, is weakened, or is broken. Do not object for the sake of it: if the case is good, say it stands.";

/// Attack the analyst's case for one stock.
pub fn skeptic(
    brain: &dyn Brain,
    card: &Card,
    case: &Case,
    note: Option<&SectorNote>,
    today: &str,
) -> Result<Rebuttal> {
    let user = format!(
        "{}\nTHE ANALYST'S CASE (catalyst {}, upside {}/10): {}\nThe analyst says the case is wrong if: {}\n\nGive your strongest objection, say whether the move is already priced in, and give your verdict.",
        dossier(card, note, today),
        case.catalyst,
        case.upside,
        case.thesis,
        case.kill
    );
    let schema = object(
        &json!({
            "objection": string(360),
            "priced_in": {"type": "boolean"},
            "verdict": {"type": "string", "enum": ["stands", "weakened", "broken"]},
        }),
        &["objection", "priced_in", "verdict"],
    );
    let answer = brain.ask(Agent::Skeptic, &card.symbol, SKEPTIC, &user, &schema)?;
    Ok(Rebuttal {
        objection: text(&answer, "objection"),
        priced_in: answer["priced_in"].as_bool().unwrap_or(false),
        verdict: text(&answer, "verdict"),
    })
}

const MANAGER: &str = "You manage a $1,000 stock account that trades news buzz. Rules you cannot change: the account is always fully invested, it holds at most five individual stocks, and every holding is decided again every day. You make every decision and nobody overrides you. Act. Keep a stock only if its catalyst is still alive and you would buy it again today at this price. Otherwise sell it, and the money goes straight into a stronger candidate. A stock with no fresh news and no buzz is a stock to sell, however it was bought. Do not sell because of an executive change or an upcoming earnings date. Each swap costs about 0.6% in fees and slippage, so do not swap one story for another that is only slightly better. Weigh the analyst against the skeptic and decide.";

/// What the manager remembers from earlier days.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Memory {
    /// The manager's notes to itself, newest first.
    pub notes: Vec<Entry>,
    /// Recent keep, sell and buy decisions, newest first.
    pub decisions: Vec<Entry>,
    /// Closed trades in one line each, newest first.
    pub trades: Vec<String>,
    /// The account's record by catalyst in one line each.
    pub record: Vec<String>,
}

fn memory_text(memory: &Memory) -> String {
    let mut out = String::from("YOUR NOTES FROM EARLIER DAYS:\n");
    if memory.notes.is_empty() {
        out.push_str("  (none yet)\n");
    }
    for note in &memory.notes {
        let _ = writeln!(out, "  {}: {}", note.day, note.text);
    }
    out.push_str("YOUR RECENT DECISIONS:\n");
    if memory.decisions.is_empty() {
        out.push_str("  (none yet)\n");
    }
    for entry in &memory.decisions {
        let _ = writeln!(
            out,
            "  {} {} {}: {}",
            entry.day, entry.kind, entry.symbol, entry.text
        );
    }
    if !memory.trades.is_empty() {
        out.push_str("CLOSED TRADES:\n");
        for trade in &memory.trades {
            let _ = writeln!(out, "  {trade}");
        }
    }
    if !memory.record.is_empty() {
        out.push_str("THE ACCOUNT'S RECORD BY CATALYST:\n");
        for line in &memory.record {
            let _ = writeln!(out, "  {line}");
        }
    }
    out
}

fn cards_text(title: &str, cards: &[&Card]) -> String {
    let mut out = format!("{title}:\n");
    if cards.is_empty() {
        out.push_str("  (none)\n");
    }
    for card in cards {
        out.push_str(&summary(card));
    }
    out
}

/// What the manager is shown for every decision of the day.
#[derive(Clone, Debug)]
pub struct Book<'a> {
    pub day: &'a str,
    pub focus: &'a Focus,
    pub memory: &'a Memory,
    pub holdings: Vec<&'a Card>,
    pub candidates: Vec<&'a Card>,
}

fn book_text(book: &Book) -> String {
    let focus: Vec<&str> = book
        .focus
        .sectors
        .iter()
        .map(|s| sectors::name_of(s))
        .collect();
    format!(
        "DATE: {}\n\n{}\nTODAY'S SECTOR FOCUS: {}. {}\n\n{}\n{}",
        book.day,
        memory_text(book.memory),
        if focus.is_empty() {
            "none chosen".to_string()
        } else {
            focus.join("; ")
        },
        book.focus.thesis,
        cards_text("HOLDINGS", &book.holdings),
        cards_text("CANDIDATES YOU COULD BUY INSTEAD", &book.candidates),
    )
}

/// Decide KEEP or SELL for every holding. Returns `symbol -> (sell, reason)` and always
/// covers every holding, or fails.
pub fn review(brain: &dyn Brain, book: &Book) -> Result<BTreeMap<String, (bool, String)>> {
    if book.holdings.is_empty() {
        return Ok(BTreeMap::new());
    }
    let symbols: Vec<&str> = book.holdings.iter().map(|c| c.symbol.as_str()).collect();
    let mut properties = serde_json::Map::new();
    for symbol in &symbols {
        properties.insert(
            (*symbol).to_string(),
            object(
                &json!({"reason": string(260), "action": {"type": "string", "enum": ["KEEP", "SELL"]}}),
                &["reason", "action"],
            ),
        );
    }
    let schema = object(&Value::Object(properties), &symbols);
    let user = format!(
        "{}\nDecide every holding now: {}. For each one give the reason first, then KEEP or SELL.",
        book_text(book),
        symbols.join(", ")
    );
    let answer = brain.ask(Agent::Manager, "holdings", MANAGER, &user, &schema)?;
    let mut out = BTreeMap::new();
    for symbol in symbols {
        let action = text(&answer[symbol], "action");
        if action != "KEEP" && action != "SELL" {
            return Err(anyhow!("the manager gave no decision for {symbol}"));
        }
        out.insert(
            symbol.to_string(),
            (action == "SELL", text(&answer[symbol], "reason")),
        );
    }
    Ok(out)
}

/// Choose exactly `count` more holdings to sell from `kept`, because the book is over its
/// size. Returns `symbol -> reason`.
pub fn trim(
    brain: &dyn Brain,
    book: &Book,
    kept: &[String],
    count: usize,
) -> Result<BTreeMap<String, String>> {
    let user = format!(
        "{}\nThe account holds more stocks than its limit of five. You kept {}. Choose exactly {count} of them to sell: the ones with the weakest catalyst and the least buzz.",
        book_text(book),
        kept.join(", ")
    );
    let schema = object(
        &json!({"sell": {
            "type": "array", "minItems": count, "maxItems": count,
            "items": object(
                &json!({"symbol": {"type": "string", "enum": kept}, "reason": string(220)}),
                &["symbol", "reason"],
            ),
        }}),
        &["sell"],
    );
    let answer = brain.ask(Agent::Manager, "trim", MANAGER, &user, &schema)?;
    let mut out = BTreeMap::new();
    for item in answer["sell"].as_array().into_iter().flatten() {
        let symbol = text(item, "symbol");
        if kept.contains(&symbol) && out.len() < count {
            out.insert(symbol, text(item, "reason"));
        }
    }
    Ok(out)
}

/// One purchase the manager chose.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Purchase {
    pub symbol: String,
    /// 1 to 3: its share of the money relative to the other purchases.
    pub weight: i64,
    pub reason: String,
}

/// Choose exactly `count` stocks to buy from `allowed`. Fewer come back only when the
/// model repeats itself; the caller asks again for the rest.
pub fn pick(
    brain: &dyn Brain,
    book: &Book,
    allowed: &[String],
    count: usize,
    already: &[String],
) -> Result<Vec<Purchase>> {
    let mut user = format!(
        "{}\nMoney is available and it must all be invested. Choose exactly {count} different stocks to buy from this list: {}.",
        book_text(book),
        allowed.join(", ")
    );
    if !already.is_empty() {
        let _ = write!(user, " You have already chosen {}.", already.join(", "));
    }
    user.push_str(" Give each a weight from 1 to 3: a stock with weight 3 gets three times the money of a stock with weight 1. Prefer today's focus sectors and the cases the skeptic could not break.");
    let schema = object(
        &json!({"buys": {
            "type": "array", "minItems": count, "maxItems": count,
            "items": object(
                &json!({
                    "symbol": {"type": "string", "enum": allowed},
                    "reason": string(240),
                    "weight": {"type": "integer", "minimum": 1, "maximum": 3},
                }),
                &["symbol", "reason", "weight"],
            ),
        }}),
        &["buys"],
    );
    let answer = brain.ask(Agent::Manager, "purchases", MANAGER, &user, &schema)?;
    let mut out: Vec<Purchase> = Vec::new();
    for item in answer["buys"].as_array().into_iter().flatten() {
        let symbol = text(item, "symbol");
        if allowed.contains(&symbol) && out.iter().all(|p| p.symbol != symbol) && out.len() < count
        {
            out.push(Purchase {
                symbol,
                weight: item["weight"].as_i64().unwrap_or(1).clamp(1, 3),
                reason: text(item, "reason"),
            });
        }
    }
    Ok(out)
}

/// The manager's note to itself for tomorrow.
pub fn note(brain: &dyn Brain, book: &Book, decisions: &[Entry]) -> Result<String> {
    let mut user = format!("{}\nWHAT YOU DECIDED TODAY:\n", book_text(book));
    if decisions.is_empty() {
        user.push_str("  (no changes)\n");
    }
    for entry in decisions {
        let _ = writeln!(user, "  {} {}: {}", entry.kind, entry.symbol, entry.text);
    }
    user.push_str("\nWrite a note to yourself for tomorrow in two or three sentences: what you are betting on, what you expect to happen, and what would change your mind.");
    let schema = object(&json!({"note": string(520)}), &["note"]);
    let answer = brain.ask(Agent::Manager, "note", MANAGER, &user, &schema)?;
    Ok(text(&answer, "note"))
}

/// Check that every sector id the agents may choose exists.
pub fn sector_ids() -> Vec<&'static str> {
    SECTORS.iter().map(|s| s.id).collect()
}

#[cfg(test)]
pub mod testing {
    use super::*;
    use crate::llm::Call;
    use std::cell::RefCell;

    type Script = Box<dyn Fn(Agent, &str, &str, &Value) -> Result<Value>>;

    /// A brain that answers from a closure and records what it was asked.
    pub struct Scripted {
        script: Script,
        pub asked: RefCell<Vec<(Agent, String, String, Value)>>,
    }

    impl std::fmt::Debug for Scripted {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.write_str("Scripted")
        }
    }

    impl Scripted {
        pub fn new(script: impl Fn(Agent, &str, &str, &Value) -> Result<Value> + 'static) -> Self {
            Scripted {
                script: Box::new(script),
                asked: RefCell::new(Vec::new()),
            }
        }

        pub fn count(&self, agent: Agent) -> usize {
            self.asked.borrow().iter().filter(|a| a.0 == agent).count()
        }
    }

    impl Brain for Scripted {
        fn ask(
            &self,
            agent: Agent,
            subject: &str,
            _: &str,
            user: &str,
            schema: &Value,
        ) -> Result<Value> {
            self.asked.borrow_mut().push((
                agent,
                subject.to_string(),
                user.to_string(),
                schema.clone(),
            ));
            (self.script)(agent, subject, user, schema)
        }

        fn model_name(&self, _: Agent) -> String {
            "scripted".into()
        }

        fn take_calls(&self) -> Vec<Call> {
            Vec::new()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::testing::Scripted;
    use super::*;

    fn card(symbol: &str, held: bool) -> Card {
        Card {
            symbol: symbol.into(),
            name: format!("{symbol} Corp"),
            sector: "chips".into(),
            facts: Facts {
                price: 100.0,
                market_cap: 5.0e10,
                change_1d: 4.2,
                change_5d: Some(9.0),
                relative_volume: Some(2.5),
                reddit: 120,
                reddit_before: 40,
                trending: vec!["Yahoo".into()],
                ..Facts::default()
            },
            headlines: vec![
                Tagged {
                    id: 1,
                    day: "2026-11-13".into(),
                    title: format!("{symbol} wins $2B AI contract"),
                    source: "Reuters".into(),
                    label: "big_deal".into(),
                    buzz: 3,
                    ..Tagged::default()
                },
                Tagged {
                    id: 2,
                    day: "2026-11-12".into(),
                    title: format!("{symbol} names new CFO"),
                    label: "noise".into(),
                    ..Tagged::default()
                },
            ],
            holding: held.then(|| Holding {
                cost: 200.0,
                market_value: 210.0,
                pnl_pct: 5.0,
                days_held: 3,
                thesis: "Contract win".into(),
                history: vec![Entry {
                    day: "2026-11-12".into(),
                    kind: "KEEP".into(),
                    text: "still running".into(),
                    ..Entry::default()
                }],
            }),
            case: Some(Case {
                catalyst: "big_deal".into(),
                thesis: "Contract is new money.".into(),
                upside: 7,
                kill: "Contract cancelled".into(),
            }),
            rebuttal: Some(Rebuttal {
                objection: "Up 9% already.".into(),
                priced_in: true,
                verdict: "weakened".into(),
            }),
        }
    }

    #[test]
    fn the_dossier_carries_numbers_news_and_history() {
        let text = dossier(&card("ACME", true), None, "2026-11-13");
        for expected in [
            "ACME (ACME Corp)",
            "AI chips and semiconductors",
            "Latest session +4.2%, 5 sessions +9.0%",
            "Volume 2.5x",
            "Reddit mentions in the last 24 hours: 120 (the day before: 40)",
            "Trending on Yahoo",
            "today [big_deal, buzz 3/3]: ACME wins $2B AI contract (Reuters)",
            "2026-11-12 [noise, buzz 0/3]: ACME names new CFO",
            "held 3 days",
            "2026-11-12 the desk said KEEP: still running",
        ] {
            assert!(text.contains(expected), "missing {expected:?} in\n{text}");
        }
        let brief = summary(&card("ACME", true));
        assert!(brief.contains("Skeptic (case weakened, move already priced in)"));
        assert!(brief.contains("News (2026-11-13, big_deal)"));
        assert!(!brief.contains("names new CFO"));
    }

    fn book<'a>(
        focus: &'a Focus,
        memory: &'a Memory,
        holdings: &'a [Card],
        candidates: &'a [Card],
    ) -> Book<'a> {
        Book {
            day: "2026-11-13",
            focus,
            memory,
            holdings: holdings.iter().collect(),
            candidates: candidates.iter().collect(),
        }
    }

    #[test]
    fn the_manager_must_decide_every_holding() {
        let holdings = [card("AAA", true), card("BBB", true)];
        let candidates = [card("CCC", false)];
        let (focus, memory) = (Focus::default(), Memory::default());
        let book = book(&focus, &memory, &holdings, &candidates);
        let brain = Scripted::new(|_, _, _, schema| {
            assert_eq!(schema["required"], json!(["AAA", "BBB"]));
            Ok(
                json!({"AAA": {"action": "KEEP", "reason": "live"}, "BBB": {"action": "SELL", "reason": "faded"}}),
            )
        });
        let verdicts = review(&brain, &book).unwrap();
        assert_eq!(verdicts["AAA"], (false, "live".to_string()));
        assert_eq!(verdicts["BBB"], (true, "faded".to_string()));
        let asked = brain.asked.borrow();
        assert!(asked[0].2.contains("CANDIDATES YOU COULD BUY INSTEAD"));
        assert!(asked[0].2.contains("CCC (CCC Corp)"));
        drop(asked);
        let partial =
            Scripted::new(|_, _, _, _| Ok(json!({"AAA": {"action": "KEEP", "reason": "x"}})));
        assert!(review(&partial, &book).is_err());
    }

    #[test]
    fn purchases_are_distinct_and_from_the_list() {
        let candidates = [card("CCC", false), card("DDD", false)];
        let (focus, memory) = (Focus::default(), Memory::default());
        let book = book(&focus, &memory, &[], &candidates);
        let brain = Scripted::new(|_, _, _, _| {
            Ok(json!({"buys": [
                {"symbol": "CCC", "weight": 3, "reason": "best"},
                {"symbol": "CCC", "weight": 1, "reason": "again"},
                {"symbol": "ZZZ", "weight": 9, "reason": "not offered"}]}))
        });
        let allowed = vec!["CCC".to_string(), "DDD".to_string()];
        let picks = pick(&brain, &book, &allowed, 2, &[]).unwrap();
        assert_eq!(picks.len(), 1);
        assert_eq!((picks[0].symbol.as_str(), picks[0].weight), ("CCC", 3));
    }

    #[test]
    fn the_classifier_maps_labels_back_to_headlines() {
        let batch = card("ACME", false).headlines;
        let brain = Scripted::new(|_, _, user, _| {
            assert!(user.contains("1. ACME wins $2B AI contract"));
            Ok(json!({"labels": [
                {"n": 2, "label": "noise", "buzz": 2},
                {"n": 1, "label": "big_deal", "buzz": 3},
                {"n": 7, "label": "policy", "buzz": 1}]}))
        });
        let labels = classify(&brain, "ACME", "ACME Corp", &batch).unwrap();
        assert_eq!(
            labels,
            [(2, "noise".to_string(), 0), (1, "big_deal".to_string(), 3)]
        );
    }

    #[test]
    fn the_strategist_only_keeps_real_distinct_sectors() {
        let notes: Vec<SectorNote> = ["chips", "clean"]
            .iter()
            .map(|s| SectorNote {
                day: "2026-11-13".into(),
                sector: (*s).into(),
                heat: 7,
                outlook: "bullish".into(),
                entry: "Busy.".into(),
                driver: "bill".into(),
            })
            .collect();
        let brain = Scripted::new(|_, _, _, schema| {
            assert_eq!(
                schema["properties"]["focus"]["items"]["enum"],
                json!(["chips", "clean"])
            );
            Ok(json!({"focus": ["clean", "clean", "moon"], "thesis": "Bill passed."}))
        });
        let focus = strategist(
            &brain,
            "2026-11-13",
            &notes,
            &BTreeMap::new(),
            &BTreeMap::new(),
            &[],
            3,
        )
        .unwrap();
        assert_eq!(focus.sectors, ["clean"]);
        assert!(bullish_labels().contains(&"policy") && !bullish_labels().contains(&"noise"));
        assert_eq!(sector_ids().len(), SECTORS.len());
    }
}
