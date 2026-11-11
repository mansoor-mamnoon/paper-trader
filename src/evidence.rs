//! Evidence: what the desk may treat as known about a security, and a deterministic check
//! of what its models then claim.
//!
//! Two things went wrong on 10 November 2026 that this module exists to stop. News about
//! Shaver Shop Group (ASX: SSG) was shown as evidence for `ProShares` `UltraShort`
//! Semiconductors (NYSE Arca: SSG) because the news feed is keyed by ticker text. And a
//! judge wrote that a fund with no news had "a tangible, recent catalyst (strong price
//! action and volume spikes)", turning a price chart into a fundamental event.
//!
//! So every item becomes an [`EvidenceRecord`] with a stable id, a source, a date, the
//! issuer and listing it was matched to and how confident the match is. Only an accepted
//! record about the security itself, describing a company event, with a source and a
//! recent date, counts as a verified event. Price and volume are technical observations.
//! An empty earnings calendar is a calendar observation. A missing item is unknown.
//!
//! [`audit`] then reads a model's text sentence by sentence and marks each claim against
//! the records. It is rule-based on purpose: a second model that agrees with the first is
//! not a check. Where a rule cannot decide, the claim is left unverified.

use crate::model::{Filing, Headline};
use crate::news::{EventKind, NewsPolicy, Quality};
use crate::security::{SecurityIdentity, company_name};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::BTreeSet;

/// A company event older than this is history, not a current catalyst.
pub const RECENT_DAYS: i64 = 30;
/// Entity-match confidence at and above which a record is accepted.
pub const ACCEPT: f64 = 0.75;

/// What kind of information a record carries.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EvidenceClass {
    /// Something the issuer did or disclosed: results, guidance, a contract, an offering.
    CompanyEvent,
    /// An economy-wide or industry-wide event.
    MacroIndustryEvent,
    /// A fact about price or volume.
    TechnicalSignal,
    /// Somebody's opinion or framing: a rating, a list, a question headline.
    AnalystInterpretation,
    /// What the earnings calendar lists, or does not list.
    CalendarObservation,
    Unknown,
}

/// What a record is about, relative to the security.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Relation {
    /// The company whose shares these are.
    Issuer,
    /// The fund itself.
    Fund,
    /// The fund's sponsor, not this fund.
    FundSponsor,
    /// The company or fund a geared product tracks. For an inverse product, good news
    /// about the underlying is bad news for the product.
    Underlying,
    /// The index or sector a fund tracks.
    IndexOrSector,
    /// The security's own price history or calendar entry.
    Market,
    /// Nothing established.
    None,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MatchStatus {
    /// About this security, with enough confidence to use.
    Accepted,
    /// Matched to something related (sponsor, underlying, sector): context, never a
    /// company event for this security.
    Context,
    /// The match is ambiguous. Kept for audit and withheld from the models.
    Quarantined,
    /// About a different issuer or listing.
    Rejected,
}

/// One auditable item of evidence.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct EvidenceRecord {
    /// Stable id: `N-` news, `F-` filing, `T-` technical, `C-` calendar.
    pub id: String,
    /// Canonical security id, for example `ARCX:SSG`.
    pub security_id: String,
    pub symbol: String,
    /// URL or document identifier.
    pub source: String,
    pub publisher: String,
    /// Publication date, `YYYY-MM-DD`, or empty when the source gave none.
    pub published: String,
    /// The date the item was retrieved, `YYYY-MM-DD`.
    pub retrieved: String,
    /// The issuer and listing the item was matched to, or what it named instead.
    pub matched_issuer: String,
    pub matched_exchange: String,
    /// The fact itself: a headline, a filing line, or numbers.
    pub excerpt: String,
    pub match_confidence: f64,
    pub status: MatchStatus,
    pub relation: Relation,
    pub class: EvidenceClass,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub age_days: Option<i64>,
    /// Why the record has the status it has.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub note: String,
    /// The news policy's grade of the source.
    #[serde(default)]
    pub quality: Quality,
    /// What kind of corporate event it describes.
    #[serde(default)]
    pub kind: EventKind,
}

impl EvidenceRecord {
    /// A sourced, recent company event about this security itself. Nothing else may be
    /// called a verified catalyst.
    pub fn verified_event(&self) -> bool {
        self.sourced_notice() && self.kind.material()
    }

    /// A sourced, recent notice from the issuer that cannot by itself move the stock's
    /// value: a board appointment, a conference slot, an insider form. Shown, never citable
    /// as a catalyst.
    pub fn minor_notice(&self) -> bool {
        self.sourced_notice() && !self.kind.material()
    }

    fn sourced_notice(&self) -> bool {
        self.status == MatchStatus::Accepted
            && matches!(self.relation, Relation::Issuer | Relation::Fund)
            && self.class == EvidenceClass::CompanyEvent
            && matches!(self.quality, Quality::Primary | Quality::Reporting)
            && !self.source.is_empty()
            && self
                .age_days
                .is_some_and(|d| (-1..=NewsPolicy::current().max_age_days).contains(&d))
    }

    /// Whether a model may read this record at all.
    pub fn shown(&self) -> bool {
        matches!(self.status, MatchStatus::Accepted | MatchStatus::Context)
            && match self.quality {
                Quality::Primary | Quality::Reporting => true,
                Quality::Commentary => NewsPolicy::current().allow_commentary,
                Quality::Blocked => false,
            }
    }

    /// One line for a prompt.
    pub fn line(&self) -> String {
        let when = if self.published.is_empty() {
            "undated".to_string()
        } else {
            self.published.clone()
        };
        format!("[{}] {when}: {}", self.id, self.excerpt)
    }
}

/// Everything gathered for one security on one day.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct EvidenceSet {
    pub records: Vec<EvidenceRecord>,
}

impl EvidenceSet {
    pub fn get(&self, id: &str) -> Option<&EvidenceRecord> {
        let id = id.trim().trim_matches(['[', ']']);
        self.records.iter().find(|r| r.id.eq_ignore_ascii_case(id))
    }

    pub fn verified_events(&self) -> Vec<&EvidenceRecord> {
        self.records.iter().filter(|r| r.verified_event()).collect()
    }

    /// Matched items a model may read that are neither verified events nor minor notices:
    /// older announcements, and commentary when the policy allows it.
    pub fn other_accepted(&self) -> Vec<&EvidenceRecord> {
        self.records
            .iter()
            .filter(|r| {
                r.status == MatchStatus::Accepted
                    && r.shown()
                    && !r.verified_event()
                    && !r.minor_notice()
                    && !matches!(r.relation, Relation::Market)
            })
            .collect()
    }

    pub fn minor_notices(&self) -> Vec<&EvidenceRecord> {
        self.records.iter().filter(|r| r.minor_notice()).collect()
    }

    /// Items kept from the models: unmatched, about another issuer, or refused by the
    /// news policy.
    pub fn withheld(&self) -> Vec<&EvidenceRecord> {
        self.records.iter().filter(|r| !r.shown()).collect()
    }

    pub fn context(&self) -> Vec<&EvidenceRecord> {
        self.with_status(MatchStatus::Context)
    }

    pub fn with_status(&self, status: MatchStatus) -> Vec<&EvidenceRecord> {
        self.records.iter().filter(|r| r.status == status).collect()
    }

    pub fn technical(&self) -> Option<&EvidenceRecord> {
        self.records
            .iter()
            .find(|r| r.class == EvidenceClass::TechnicalSignal && r.relation == Relation::Market)
    }

    /// Counts for the transcript and the comparison table.
    pub fn counts(&self) -> serde_json::Value {
        serde_json::json!({
            "verified_events": self.verified_events().len(),
            "minor_notices": self.minor_notices().len(),
            "other_accepted": self.other_accepted().len(),
            "withheld_by_news_policy": self.records.iter().filter(|r| r.status == MatchStatus::Accepted && !r.shown()).count(),
            "context": self.context().len(),
            "quarantined": self.with_status(MatchStatus::Quarantined).len(),
            "rejected": self.with_status(MatchStatus::Rejected).len(),
        })
    }
}

fn short_hash(text: &str) -> String {
    use std::fmt::Write as _;
    Sha256::digest(text.as_bytes())
        .iter()
        .take(4)
        .fold(String::new(), |mut out, b| {
            let _ = write!(out, "{b:02x}");
            out
        })
}

/// Lowercase alphanumeric words.
fn words(text: &str) -> Vec<String> {
    text.split(|c: char| !c.is_alphanumeric())
        .filter(|w| !w.is_empty())
        .map(str::to_lowercase)
        .collect()
}

/// Spelling variants of corporate suffixes, reduced to one form.
fn suffix(word: &str) -> Option<&'static str> {
    Some(match word {
        "inc" | "incorporated" => "inc",
        "corp" | "corporation" => "corp",
        "co" | "company" => "co",
        "ltd" | "limited" => "ltd",
        "plc" => "plc",
        "llc" => "llc",
        "lp" => "lp",
        "sa" | "nv" | "ag" | "se" | "ab" | "asa" => "sa",
        "holdings" | "holding" => "holdings",
        "group" => "group",
        "trust" => "trust",
        _ => return None,
    })
}

fn normalized(text: &str) -> Vec<String> {
    words(text)
        .into_iter()
        .filter(|w| w != "the")
        .map(|w| suffix(&w).map_or(w, str::to_string))
        .collect()
}

fn contains_phrase(haystack: &[String], needle: &[String]) -> bool {
    !needle.is_empty() && haystack.windows(needle.len()).any(|w| w == needle)
}

const US_VENUES: [&str; 12] = [
    "NASDAQ",
    "NYSE",
    "NYSEARCA",
    "NYSEAMERICAN",
    "NYSEMKT",
    "AMEX",
    "ARCA",
    "CBOE",
    "BATS",
    "NASDAQGS",
    "NASDAQGM",
    "NASDAQCM",
];

/// Listing tags such as `(ASX:SSG)` or `(NASDAQ: OMH)`: `(venue, ticker)` pairs.
pub fn listing_tags(text: &str) -> Vec<(String, String)> {
    let mut found = Vec::new();
    let mut rest = text;
    while let Some(open) = rest.find('(') {
        let tail = &rest[open + 1..];
        let Some(close) = tail.find(')') else { break };
        let inside = &tail[..close];
        if let Some((venue, ticker)) = inside.split_once(':') {
            let venue: String = venue
                .chars()
                .filter(char::is_ascii_alphabetic)
                .collect::<String>()
                .to_uppercase();
            let ticker = ticker.trim().to_uppercase();
            let plausible = (2..=14).contains(&venue.len())
                && (1..=8).contains(&ticker.len())
                && ticker
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '-');
            if plausible {
                found.push((venue, ticker));
            }
        }
        rest = &tail[close + 1..];
    }
    found
}

/// Bare parenthesised tickers such as `(POOL)`.
fn bare_tickers(text: &str) -> Vec<String> {
    let mut found = Vec::new();
    let mut rest = text;
    while let Some(open) = rest.find('(') {
        let tail = &rest[open + 1..];
        let Some(close) = tail.find(')') else { break };
        let inside = tail[..close].trim();
        if (1..=6).contains(&inside.len()) && inside.chars().all(|c| c.is_ascii_uppercase()) {
            found.push(inside.to_string());
        }
        rest = &tail[close + 1..];
    }
    found
}

const QUESTION_OR_LIST: [&str; 12] = [
    "should you",
    "to consider",
    "to buy",
    "to watch",
    "stocks to",
    "what to expect",
    "is it time",
    "here's why",
    "good position",
    "undervalued",
    "overvalued",
    "worth a look",
];
const COMPANY_WORDS: [&str; 34] = [
    "announce",
    "reports",
    "reported",
    "results",
    "earnings call",
    "guidance",
    "appoint",
    "acquire",
    "acquisition",
    "completes",
    "closing of",
    "pricing of",
    "offering",
    "receives",
    "regains compliance",
    "notification",
    "contract",
    "agreement",
    "launch",
    "dividend",
    "approval",
    "approves",
    "files",
    "partnership",
    "to present",
    "share consolidation",
    "reverse split",
    "reverse stock split",
    "buyback",
    "repurchase",
    "record sales",
    "record revenue",
    "chapter 11",
    "bankruptcy",
];
const TECHNICAL_WORDS: [&str; 22] = [
    "shares jump",
    "stock jumps",
    "soars",
    "surges",
    "plunges",
    "rallies",
    "most active",
    "52-week",
    "all-time high",
    "stock is moving",
    "shares rise",
    "shares fall",
    "stock drops",
    "skyrockets",
    "tumbles",
    "trading higher",
    "trading lower",
    "short squeeze",
    "top gainers",
    "movers",
    "why is",
    "stock is up",
];
const OPINION_WORDS: [&str; 9] = [
    "price target",
    "upgrade",
    "downgrade",
    "initiates coverage",
    "rating",
    "analyst",
    "outlook",
    "preview",
    "bull case",
];
const MACRO_WORDS: [&str; 10] = [
    "federal reserve",
    "the fed",
    "tariff",
    "inflation",
    "sector",
    "industry",
    "stocks slide",
    "wall street",
    "etfs",
    "penny stocks",
];

/// Classify a headline by what kind of information it carries.
pub fn classify(title: &str) -> EvidenceClass {
    let lower = title.to_lowercase();
    let has = |list: &[&str]| list.iter().any(|w| lower.contains(w));
    if lower.trim_end().ends_with('?') || has(&QUESTION_OR_LIST) {
        EvidenceClass::AnalystInterpretation
    } else if has(&COMPANY_WORDS) {
        EvidenceClass::CompanyEvent
    } else if has(&TECHNICAL_WORDS) {
        EvidenceClass::TechnicalSignal
    } else if has(&OPINION_WORDS) {
        EvidenceClass::AnalystInterpretation
    } else if has(&MACRO_WORDS) {
        EvidenceClass::MacroIndustryEvent
    } else {
        EvidenceClass::Unknown
    }
}

fn iso_date(published: &str) -> Option<chrono::NaiveDate> {
    chrono::DateTime::parse_from_rfc2822(published.trim())
        .map(|t| t.date_naive())
        .ok()
        .or_else(|| published.get(..10)?.parse().ok())
}

/// The distinctive words of a company name, without corporate suffixes.
fn core_name(name: &str) -> Vec<String> {
    let all = normalized(&company_name(name));
    let core: Vec<String> = all
        .iter()
        .filter(|w| suffix(w).is_none())
        .cloned()
        .collect();
    if core.is_empty() { all } else { core }
}

struct Match {
    confidence: f64,
    status: MatchStatus,
    relation: Relation,
    issuer: String,
    exchange: String,
    note: String,
}

fn match_item(identity: &SecurityIdentity, text: &str) -> Match {
    let fund = identity.instrument_type.is_fund();
    let own = if fund {
        Relation::Fund
    } else {
        Relation::Issuer
    };
    let tags = listing_tags(text);
    let symbol = identity.symbol.to_uppercase();
    let tokens = normalized(text);
    let made =
        |confidence: f64, status, relation, issuer: &str, exchange: &str, note: String| Match {
            confidence,
            status,
            relation,
            issuer: issuer.to_string(),
            exchange: exchange.to_string(),
            note,
        };
    // A listing tag is the strongest signal either way.
    if let Some((venue, _)) = tags
        .iter()
        .find(|(v, t)| *t == symbol && US_VENUES.contains(&v.as_str()))
    {
        return made(
            0.95,
            MatchStatus::Accepted,
            own,
            &identity.canonical_name,
            venue,
            format!("names {venue}:{symbol}"),
        );
    }
    if let Some((venue, _)) = tags.iter().find(|(_, t)| *t == symbol) {
        return made(
            0.0,
            MatchStatus::Rejected,
            Relation::None,
            "a different issuer",
            venue,
            format!(
                "names {venue}:{symbol}, a different listing from {} ({})",
                identity.security_id(),
                identity.canonical_name
            ),
        );
    }
    // The registered name, in full and then without its corporate suffix.
    let full = normalized(&company_name(&identity.canonical_name));
    let core = core_name(&identity.canonical_name);
    let generic = ["etf", "fund", "shares", "trust", "daily", "target"];
    let fund_core: Vec<String> = full
        .iter()
        .filter(|w| !generic.contains(&w.as_str()))
        .cloned()
        .collect();
    let named = if fund {
        fund_core.len() >= 2 && contains_phrase(&tokens, &fund_core)
    } else {
        (full.len() >= 2 && contains_phrase(&tokens, &full))
            || (contains_phrase(&tokens, &core)
                && (core.len() >= 2 || core.first().is_some_and(|w| w.len() >= 6)))
    };
    if named {
        return made(
            if tags.is_empty() { 0.88 } else { 0.8 },
            MatchStatus::Accepted,
            own,
            &identity.canonical_name,
            &identity.exchange,
            "names the registered issuer".into(),
        );
    }
    if fund {
        // Related, but not the fund: its underlying, its sector, its sponsor.
        if let Some(under) = &identity.underlying_symbol {
            let tagged = tags.iter().any(|(_, t)| t == under) || bare_tickers(text).contains(under);
            if tagged {
                return made(
                    0.9,
                    MatchStatus::Context,
                    Relation::Underlying,
                    under,
                    "",
                    format!(
                        "about {under}, which this fund tracks at {:+}x; not about the fund",
                        identity.signed_multiple().unwrap_or(1.0)
                    ),
                );
            }
        }
        if let Some(name) = &identity.underlying_name {
            let wanted: Vec<String> = core_name(name)
                .into_iter()
                .filter(|w| {
                    w.len() >= 6 && !["index", "nyse", "jones", "roundhill"].contains(&w.as_str())
                })
                .collect();
            let stem = |w: &str| w.trim_end_matches('s').to_string();
            if wanted
                .iter()
                .any(|w| tokens.iter().any(|t| stem(t) == stem(w)))
            {
                return made(
                    0.6,
                    MatchStatus::Context,
                    Relation::IndexOrSector,
                    name,
                    "",
                    format!("about {name}, which this fund tracks; not about the fund"),
                );
            }
        }
        let brand: Vec<String> = identity
            .issuer_name
            .as_deref()
            .map(|n| normalized(n).into_iter().take(1).collect())
            .unwrap_or_default();
        if brand.first().is_some_and(|w| w.len() >= 5) && contains_phrase(&tokens, &brand) {
            return made(
                0.6,
                MatchStatus::Context,
                Relation::FundSponsor,
                identity.issuer_name.as_deref().unwrap_or(""),
                "",
                "about the fund's sponsor, not this fund".into(),
            );
        }
    }
    if !tags.is_empty() {
        let others: Vec<String> = tags.iter().map(|(v, t)| format!("{v}:{t}")).collect();
        return made(
            0.0,
            MatchStatus::Rejected,
            Relation::None,
            "other issuers",
            "",
            format!(
                "names {} and not {}",
                others.join(", "),
                identity.security_id()
            ),
        );
    }
    // A short, common-word name ("Pool") or a bare ticker is a hint, not a match.
    let short_name = !fund
        && core.len() == 1
        && text.split(|c: char| !c.is_alphanumeric()).any(|w| {
            w.len() >= 3 && w.to_lowercase() == core[0] && w.starts_with(char::is_uppercase)
        });
    let ticker = bare_tickers(text).contains(&symbol);
    match (short_name, ticker) {
        (true, true) => made(
            0.8,
            MatchStatus::Accepted,
            own,
            &identity.canonical_name,
            &identity.exchange,
            "names the issuer's short name and its ticker".into(),
        ),
        (true, false) | (false, true) => made(
            0.5,
            MatchStatus::Quarantined,
            Relation::None,
            "",
            "",
            "mentions only a short name or a bare ticker, which other issuers share".into(),
        ),
        (false, false) => made(
            0.2,
            MatchStatus::Quarantined,
            Relation::None,
            "",
            "",
            "returned by the ticker feed, but names neither the issuer nor its listing".into(),
        ),
    }
}

/// Turn one feed item into an evidence record for `identity`.
pub fn news_record(
    identity: &SecurityIdentity,
    item: &Headline,
    today: chrono::NaiveDate,
) -> EvidenceRecord {
    let text = format!("{} {}", item.title, item.summary);
    let matched = match_item(identity, &text);
    let published = iso_date(&item.published);
    let key = if item.guid.is_empty() {
        format!("{}|{}", item.title, item.published)
    } else {
        item.guid.clone()
    };
    // The news policy grades only items already matched to this security.
    let (quality, policy_note) = if matched.status == MatchStatus::Accepted {
        let title = normalized(&item.title);
        let names = [
            normalized(&identity.canonical_name),
            normalized(&company_name(&identity.canonical_name)),
            core_name(&identity.canonical_name),
        ];
        let after = names
            .iter()
            .filter(|n| !n.is_empty() && title.starts_with(n))
            .map(|n| &title[n.len()..])
            .min_by_key(|rest| rest.len());
        NewsPolicy::current().grade(
            &item.title,
            &item.link,
            after,
            &identity.symbol.to_lowercase(),
        )
    } else {
        (Quality::Commentary, String::new())
    };
    let mut class = classify(&item.title);
    if matches!(quality, Quality::Primary | Quality::Reporting) {
        // An issuer's release or a newsroom report is an event, whatever words it uses.
        class = EvidenceClass::CompanyEvent;
    }
    if matches!(
        matched.relation,
        Relation::IndexOrSector | Relation::FundSponsor
    ) && class == EvidenceClass::Unknown
    {
        class = EvidenceClass::MacroIndustryEvent;
    }
    EvidenceRecord {
        id: format!("N-{}", short_hash(&format!("{}|{key}", identity.symbol))),
        security_id: identity.security_id(),
        symbol: identity.symbol.clone(),
        source: if item.link.is_empty() {
            item.guid.clone()
        } else {
            item.link.clone()
        },
        publisher: "Yahoo Finance ticker feed".into(),
        published: published.map(|d| d.to_string()).unwrap_or_default(),
        retrieved: today.to_string(),
        matched_issuer: matched.issuer,
        matched_exchange: matched.exchange,
        excerpt: item.title.chars().take(160).collect(),
        match_confidence: matched.confidence,
        status: matched.status,
        relation: matched.relation,
        class,
        age_days: published.map(|d| (today - d).num_days()),
        note: if policy_note.is_empty() {
            matched.note
        } else {
            format!("{}; {policy_note}", matched.note)
        },
        quality,
        kind: crate::news::event_kind(&item.title),
    }
}

/// The kind of event a filing reports, from its form type and 8-K items.
fn filing_kind(filing: &Filing) -> EventKind {
    match filing.form.as_str() {
        "8-K" => {
            let kinds: Vec<EventKind> = filing
                .items
                .split(',')
                .map(|item| match item.trim() {
                    "2.02" => EventKind::Results,
                    "1.01" => EventKind::Product,
                    "2.01" | "5.01" => EventKind::Deal,
                    "1.03" | "3.01" | "4.01" | "4.02" => EventKind::Regulatory,
                    "2.03" | "3.02" | "2.05" | "2.06" => EventKind::Financing,
                    "5.02" => EventKind::Personnel,
                    "5.07" => EventKind::Administrative,
                    _ => EventKind::Other,
                })
                .collect();
            kinds
                .iter()
                .copied()
                .find(|k| k.material())
                .or_else(|| kinds.first().copied())
                .unwrap_or_default()
        }
        "10-Q" | "10-K" => EventKind::Results,
        "4" => EventKind::Insider,
        "S-3" | "424B5" => EventKind::Financing,
        "S-4" | "DEFM14A" | "PREM14A" | "425" | "SC TO-T" | "SC 13E3" => EventKind::Deal,
        "15-12B" | "15-12G" | "25-NSE" => EventKind::Regulatory,
        "DEF 14A" => EventKind::Administrative,
        form if form.starts_with("SC 13") => EventKind::Holder,
        _ => EventKind::Other,
    }
}

/// Turn one SEC filing into an evidence record. The SEC's own ticker file maps the symbol
/// to the filer, so the match is as good as that file. A fund's filer is its trust, which
/// files for every fund it holds, so those records are context only.
pub fn filing_record(
    identity: &SecurityIdentity,
    filing: &Filing,
    today: chrono::NaiveDate,
) -> EvidenceRecord {
    let fund = identity.instrument_type.is_fund();
    let published = filing.date.parse::<chrono::NaiveDate>().ok();
    let cik = if filing.cik.is_empty() {
        identity.cik.clone().unwrap_or_default()
    } else {
        filing.cik.clone()
    };
    let source = if cik.is_empty() || filing.accession.is_empty() {
        format!("SEC accession {}", filing.accession)
    } else {
        format!(
            "https://www.sec.gov/Archives/edgar/data/{}/{}/{}",
            cik.trim_start_matches('0'),
            filing.accession.replace('-', ""),
            filing.document
        )
    };
    EvidenceRecord {
        id: format!(
            "F-{}",
            short_hash(&format!("{}|{}", identity.symbol, filing.accession))
        ),
        security_id: identity.security_id(),
        symbol: identity.symbol.clone(),
        source,
        publisher: "SEC EDGAR".into(),
        published: filing.date.clone(),
        retrieved: today.to_string(),
        matched_issuer: identity
            .issuer_name
            .clone()
            .unwrap_or_else(|| identity.canonical_name.clone()),
        matched_exchange: identity.exchange.clone(),
        excerpt: crate::edgar::describe(filing),
        match_confidence: if fund { 0.6 } else { 0.95 },
        status: if fund {
            MatchStatus::Context
        } else {
            MatchStatus::Accepted
        },
        relation: if fund {
            Relation::FundSponsor
        } else {
            Relation::Issuer
        },
        class: EvidenceClass::CompanyEvent,
        age_days: published.map(|d| (today - d).num_days()),
        note: if fund {
            "filed by the fund's trust, which files for many funds".into()
        } else {
            String::new()
        },
        quality: Quality::Primary,
        kind: filing_kind(filing),
    }
}

/// The price and volume facts about one security, as measured from its bars.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Technicals {
    pub close: f64,
    pub momentum_20d: f64,
    pub momentum_5d: f64,
    pub volatility_20d: f64,
    pub max_gap_20d: f64,
    pub avg_dollar_volume: f64,
    /// Date of the latest bar, `YYYY-MM-DD`.
    pub bar_date: String,
    /// Largest peak-to-trough fall of the close over the bars held (about three months),
    /// as a positive fraction. Zero when not measured.
    #[serde(default)]
    pub max_drawdown: f64,
}

/// Gather every record for one security: its matched news, its filings, its technical
/// observation and its calendar observation.
pub fn gather(
    identity: &SecurityIdentity,
    technicals: &Technicals,
    headlines: &[Headline],
    filings: &[Filing],
    earnings_date: Option<&str>,
    today: chrono::NaiveDate,
) -> EvidenceSet {
    let mut records: Vec<EvidenceRecord> = Vec::new();
    let mut seen = BTreeSet::new();
    for item in headlines {
        let record = news_record(identity, item, today);
        if seen.insert(record.id.clone()) {
            records.push(record);
        }
    }
    for filing in filings {
        let record = filing_record(identity, filing, today);
        if seen.insert(record.id.clone()) {
            records.push(record);
        }
    }
    let market = |id: String, class, excerpt: String, note: &str| EvidenceRecord {
        id,
        security_id: identity.security_id(),
        symbol: identity.symbol.clone(),
        source: "price provider daily bars; Nasdaq earnings calendar".into(),
        publisher: "paper-trader measurement".into(),
        published: technicals.bar_date.clone(),
        retrieved: today.to_string(),
        matched_issuer: identity.canonical_name.clone(),
        matched_exchange: identity.exchange.clone(),
        excerpt,
        match_confidence: 1.0,
        status: MatchStatus::Accepted,
        relation: Relation::Market,
        class,
        age_days: Some(0),
        note: note.to_string(),
        quality: Quality::Primary,
        kind: EventKind::Other,
    };
    records.push(market(
        format!("T-{}-{}", identity.symbol, technicals.bar_date),
        EvidenceClass::TechnicalSignal,
        format!(
            "close {:.2}; 20-day {:+.1}%; 5-day {:+.1}%; volatility {:.1}%/day; largest overnight gap {:.1}%; traded ${:.1}M/day",
            technicals.close,
            technicals.momentum_20d * 100.0,
            technicals.momentum_5d * 100.0,
            technicals.volatility_20d * 100.0,
            technicals.max_gap_20d * 100.0,
            technicals.avg_dollar_volume / 1e6
        ),
        "what the price did; says nothing about why",
    ));
    records.push(market(
        format!("C-{}-{today}", identity.symbol),
        EvidenceClass::CalendarObservation,
        match earnings_date {
            Some(date) => format!("earnings report listed for {date}"),
            None => "no earnings report listed in the next 14 days".to_string(),
        },
        "a calendar entry; an empty calendar is unknown, not good or bad",
    ));
    EvidenceSet { records }
}

/// What kind of statement a claim is.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ClaimType {
    /// What the instrument is.
    Instrument,
    /// A catalyst, news item or other company or macro event.
    Event,
    /// Insider or large-holder buying or selling.
    InsiderActivity,
    /// A price, volume or volatility statement.
    Technical,
    /// An earnings-calendar statement.
    Calendar,
    /// A forward-looking view.
    Hypothesis,
    /// A reference to an evidence id.
    Citation,
}

/// How a claim stands against the evidence. Ordered from best to worst.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VerificationStatus {
    /// Matches a sourced record or the security master.
    VerifiedFact,
    /// A correct statement about price or volume. Not a cause.
    TechnicalObservation,
    /// A view about the future, stated as one.
    Hypothesis,
    /// No rule could confirm or refute it.
    Unverified,
    /// Stated as fact with no matching record.
    UnsupportedAssertion,
    /// Conflicts with the security master, the numbers or the calendar.
    ContradictedAssertion,
}

/// One claim found in a model's text, with its verdict.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct EvidenceClaim {
    pub text: String,
    pub claim_type: ClaimType,
    pub evidence_ids: Vec<String>,
    pub verification_status: VerificationStatus,
    /// Entity-match confidence of the supporting record, when there is one. Never a
    /// probability that the claim is true.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub confidence: Option<f64>,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub note: String,
}

/// What a text is checked against.
#[derive(Debug)]
pub struct Context<'a> {
    pub identity: &'a SecurityIdentity,
    pub evidence: &'a EvidenceSet,
    pub technicals: &'a Technicals,
    pub earnings_date: Option<&'a str>,
}

const NOT_GEARED: [&str; 12] = [
    "not leveraged",
    "non-leveraged",
    "unleveraged",
    "not a leveraged",
    "without being a speculative or leveraged",
    "without being leveraged",
    "without leverage",
    "no leverage",
    "not an inverse",
    "not inverse",
    "isn't leveraged",
    "is not a leveraged or inverse",
];
const GEARED_WORDS: [&str; 6] = [
    "leveraged etf",
    "inverse etf",
    "leveraged product",
    "inverse product",
    "leveraged fund",
    "inverse fund",
];
const COMPANY_TALK: [&str; 11] = [
    "the company",
    "company's",
    "penny stock",
    "management",
    "record sales",
    "revenue",
    "margins",
    "its earnings",
    "earnings call",
    "the firm",
    "insider",
];
const EVENT_WORDS: [&str; 17] = [
    "catalyst",
    "news",
    "announc",
    "guidance",
    "contract",
    "upgrade",
    "earnings beat",
    "record sales",
    "record revenue",
    "record margins",
    "partnership",
    "approval",
    "acquisition",
    "product launch",
    "fundamental support",
    "fundamentally supported",
    "market interest",
];
const NEGATIONS: [&str; 21] = [
    "no news",
    "no catalyst",
    "without a catalyst",
    "without news",
    "lack of",
    "lacks",
    "absence of",
    "no clear",
    "no recent",
    "no specific",
    "no verified",
    "no verified event",
    "not supported by",
    "no fundamental",
    "no company",
    "none listed",
    "no evidence",
    "unknown",
    "no matched",
    "technical only",
    "technical-only",
];
const INSIDER_WORDS: [&str; 9] = [
    "insider buying",
    "insiders are buying",
    "insiders buying",
    "insider purchase",
    "insiders bought",
    "insider activity",
    "insider accumulation",
    "large holders are buying",
    "insider selling",
];
const PRICE_WORDS: [&str; 17] = [
    "price",
    "volume",
    "momentum",
    "spike",
    "gain",
    "rally",
    "trend",
    "20-day",
    "5-day",
    "20d",
    "5d",
    "volatility",
    "pullback",
    "breakout",
    "gap",
    "moving average",
    "candle",
];
const EARNINGS_AHEAD: [&str; 7] = [
    "upcoming earnings",
    "earnings next week",
    "ahead of earnings",
    "earnings report on",
    "earnings in ",
    "reports earnings",
    "pending earnings",
];
const NO_EARNINGS: [&str; 9] = [
    "no earnings",
    "lack of earnings",
    "absence of earnings",
    "not reported earnings",
    "no upcoming earnings",
    "without earnings",
    "no earnings catalyst",
    "no earnings announcement",
    "no earnings report",
];
const JUDGEMENT_WORDS: [&str; 17] = [
    "risk",
    "concern",
    "unsustainable",
    "sustainab",
    "weak",
    "negative",
    "red flag",
    "support",
    "bullish",
    "positive",
    "safe",
    "not supported",
    "raises",
    "makes",
    "meaning",
    "suggest",
    "indicat",
];
const FALL_WORDS: [&str; 8] = [
    " fell ",
    " fallen ",
    " dropped ",
    " declined ",
    " plunged ",
    " lost ",
    " drop of ",
    " decline of ",
];
const HYPOTHESIS_WORDS: [&str; 10] = [
    " may ",
    " might ",
    " could ",
    "likely",
    "expect",
    "potential",
    " if ",
    "should ",
    "probably",
    "hypothesis",
];

fn sentences(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut current = String::new();
    let chars: Vec<char> = text.chars().collect();
    for (i, c) in chars.iter().enumerate() {
        current.push(*c);
        let boundary = matches!(c, '!' | '?' | ';' | '\n')
            || (*c == '.'
                && chars.get(i + 1).is_none_or(|n| n.is_whitespace())
                && !chars
                    .get(i.wrapping_sub(1))
                    .is_some_and(char::is_ascii_digit));
        if boundary {
            if current.trim().len() > 3 {
                out.push(current.trim().to_string());
            }
            current.clear();
        }
    }
    if current.trim().len() > 3 {
        out.push(current.trim().to_string());
    }
    out
}

/// The first percentage that follows `marker` within a short distance, as a fraction.
fn percent_after(lower: &str, marker: &str) -> Option<f64> {
    let at = lower.find(marker)? + marker.len();
    let window: String = lower[at..].chars().take(40).collect();
    let end = window.find('%')?;
    let start = window[..end]
        .rfind(|c: char| !(c.is_ascii_digit() || c == '.' || c == '-' || c == '+' || c == ','))
        .map_or(0, |i| i + 1);
    window[start..end]
        .replace(',', "")
        .parse::<f64>()
        .ok()
        .map(|v| v / 100.0)
}

fn differs(stated: f64, actual: f64) -> bool {
    (stated - actual).abs() > 0.006 && (stated - actual).abs() > actual.abs() * 0.05
}

fn mentions(sentence: &str, symbol: &str) -> bool {
    sentence
        .split(|c: char| !c.is_alphanumeric())
        .any(|w| w == symbol)
}

/// Check `text` against the evidence for one security.
///
/// `cited` are the evidence ids the model attached to the text. When `scoped` is true the
/// whole text is about this security (a bull case, a research note); otherwise only the
/// sentences that name its symbol are read (a judge's reasoning over several finalists).
pub fn audit(text: &str, cited: &[String], ctx: &Context, scoped: bool) -> Vec<EvidenceClaim> {
    let mut claims = Vec::new();
    let identity = ctx.identity;
    let geared = identity.instrument_type.is_geared();
    let fund = identity.instrument_type.is_fund();
    let events = ctx.evidence.verified_events();
    let cited_events: Vec<&EvidenceRecord> = cited
        .iter()
        .filter_map(|id| ctx.evidence.get(id))
        .filter(|r| r.verified_event())
        .collect();
    for id in cited {
        if !id.trim().is_empty() && ctx.evidence.get(id).is_none() {
            claims.push(EvidenceClaim {
                text: format!("cites {id}"),
                claim_type: ClaimType::Citation,
                evidence_ids: vec![id.clone()],
                verification_status: VerificationStatus::UnsupportedAssertion,
                confidence: None,
                note: "this id is not in the evidence for this security".into(),
            });
        }
    }
    for sentence in sentences(text) {
        if !scoped && !mentions(&sentence, &identity.symbol) {
            continue;
        }
        let lower = format!(" {} ", sentence.to_lowercase());
        let has = |list: &[&str]| list.iter().any(|w| lower.contains(w));
        let which = |list: &[&'static str]| list.iter().copied().find(|w| lower.contains(w));
        let mut found: Vec<EvidenceClaim> = Vec::new();
        macro_rules! push {
            ($kind:expr, $status:expr, $ids:expr, $confidence:expr, $note:expr $(,)?) => {
                found.push(EvidenceClaim {
                    text: sentence.chars().take(240).collect(),
                    claim_type: $kind,
                    evidence_ids: $ids,
                    verification_status: $status,
                    confidence: $confidence,
                    note: $note,
                })
            };
        }
        // What the instrument is.
        if let Some(phrase) = which(&NOT_GEARED) {
            if geared {
                push!(
                    ClaimType::Instrument,
                    VerificationStatus::ContradictedAssertion,
                    vec![],
                    None,
                    format!(
                        "says \"{phrase}\", but the security master has {}",
                        identity.describe()
                    ),
                );
            } else if identity.verified {
                push!(
                    ClaimType::Instrument,
                    VerificationStatus::VerifiedFact,
                    vec![],
                    None,
                    String::new(),
                );
            }
        } else if let Some(phrase) = which(&GEARED_WORDS).filter(|_| identity.verified && !fund) {
            push!(
                ClaimType::Instrument,
                VerificationStatus::ContradictedAssertion,
                vec![],
                None,
                format!(
                    "says \"{phrase}\", but the security master has {}",
                    identity.describe()
                ),
            );
        }
        if fund && let Some(phrase) = which(&COMPANY_TALK) {
            push!(
                ClaimType::Instrument,
                VerificationStatus::ContradictedAssertion,
                vec![],
                None,
                format!(
                    "talks about \"{phrase}\", but {} is a fund with no sales, earnings or insiders of its own",
                    identity.symbol
                ),
            );
        }
        // Insider activity needs a parsed transaction, not the existence of a form.
        if let Some(phrase) = which(&INSIDER_WORDS).filter(|_| !has(&NEGATIONS)) {
            let wanted = if phrase.contains("sell") {
                "open-market sale"
            } else {
                "open-market purchase"
            };
            let forms: Vec<&EvidenceRecord> = ctx
                .evidence
                .records
                .iter()
                .filter(|r| r.id.starts_with("F-") && r.excerpt.contains("Form 4"))
                .collect();
            let proof = forms.iter().find(|r| r.excerpt.contains(wanted));
            match proof {
                Some(record) => push!(
                    ClaimType::InsiderActivity,
                    VerificationStatus::VerifiedFact,
                    vec![record.id.clone()],
                    Some(record.match_confidence),
                    String::new(),
                ),
                None => push!(
                    ClaimType::InsiderActivity,
                    VerificationStatus::UnsupportedAssertion,
                    forms.iter().map(|r| r.id.clone()).collect(),
                    None,
                    if forms.is_empty() {
                        "no Form 4 is in the evidence".to_string()
                    } else {
                        format!(
                            "a Form 4 was filed, but no {wanted} was extracted from it; a filing alone does not show direction"
                        )
                    },
                ),
            }
        }
        // Events and catalysts need a sourced record about this security.
        let negated = has(&NEGATIONS) || has(&NO_EARNINGS) || has(&EARNINGS_AHEAD);
        if let Some(phrase) = which(&EVENT_WORDS).filter(|_| !negated) {
            let support = if cited.is_empty() {
                events.first().copied()
            } else {
                cited_events.first().copied()
            };
            match support {
                Some(record) => push!(
                    ClaimType::Event,
                    VerificationStatus::VerifiedFact,
                    vec![record.id.clone()],
                    Some(record.match_confidence),
                    String::new(),
                ),
                None => push!(
                    ClaimType::Event,
                    VerificationStatus::UnsupportedAssertion,
                    vec![],
                    None,
                    if has(&PRICE_WORDS) {
                        format!(
                            "says \"{phrase}\" on the strength of price or volume; those are technical observations, not a verified event"
                        )
                    } else if events.is_empty() {
                        format!(
                            "says \"{phrase}\", but no verified event is in the evidence for {}",
                            identity.symbol
                        )
                    } else {
                        format!("says \"{phrase}\" without citing one of the verified events")
                    },
                ),
            }
        }
        // The earnings calendar.
        if has(&EARNINGS_AHEAD) && !has(&NO_EARNINGS) && ctx.earnings_date.is_none() {
            push!(
                ClaimType::Calendar,
                VerificationStatus::ContradictedAssertion,
                vec![],
                None,
                "refers to an upcoming earnings report, but the calendar lists none".into(),
            );
        } else if let Some(phrase) = which(&NO_EARNINGS) {
            if ctx.earnings_date.is_some() {
                push!(
                    ClaimType::Calendar,
                    VerificationStatus::ContradictedAssertion,
                    vec![],
                    None,
                    format!(
                        "says \"{phrase}\", but the calendar lists a report on {}",
                        ctx.earnings_date.unwrap_or("")
                    ),
                );
            } else if has(&JUDGEMENT_WORDS) {
                push!(
                    ClaimType::Calendar,
                    VerificationStatus::Unverified,
                    vec![],
                    None,
                    "treats an empty earnings calendar as a reason; it is a calendar observation and says nothing about the business".into(),
                );
            }
        }
        // "Penny stock" is a statement about price, and the price is known.
        if lower.contains("penny stock") && ctx.technicals.close >= crate::gates::MIN_PRICE {
            push!(
                ClaimType::Technical,
                VerificationStatus::ContradictedAssertion,
                vec![],
                None,
                format!(
                    "calls it a penny stock; the last close is ${:.2}",
                    ctx.technicals.close
                ),
            );
        }
        // A stated fall larger than anything in the bars did not happen to this stock.
        if ctx.technicals.max_drawdown > 0.0
            && let Some(stated) = FALL_WORDS
                .iter()
                .find_map(|w| percent_after(&lower, w))
                .map(f64::abs)
            && stated > ctx.technicals.max_drawdown + 0.05
        {
            push!(
                ClaimType::Technical,
                VerificationStatus::ContradictedAssertion,
                ctx.evidence
                    .technical()
                    .map(|r| vec![r.id.clone()])
                    .unwrap_or_default(),
                None,
                format!(
                    "says the price fell {:.0}%; the largest fall in three months of bars is {:.0}%",
                    stated * 100.0,
                    ctx.technicals.max_drawdown * 100.0
                ),
            );
        }
        // Numbers.
        let numbers = [
            ("20-day", ctx.technicals.momentum_20d),
            ("20d", ctx.technicals.momentum_20d),
            ("5-day", ctx.technicals.momentum_5d),
            ("5d", ctx.technicals.momentum_5d),
        ];
        let mut numeric = None;
        for (marker, actual) in numbers {
            if let Some(stated) = percent_after(&lower, marker) {
                numeric = Some((marker, stated, actual));
                break;
            }
        }
        if let Some((marker, stated, actual)) = numeric {
            if differs(stated, actual) {
                push!(
                    ClaimType::Technical,
                    VerificationStatus::ContradictedAssertion,
                    ctx.evidence
                        .technical()
                        .map(|r| vec![r.id.clone()])
                        .unwrap_or_default(),
                    None,
                    format!(
                        "says {marker} {:+.1}%, the bars show {:+.1}%",
                        stated * 100.0,
                        actual * 100.0
                    ),
                );
            } else if found.is_empty() {
                push!(
                    ClaimType::Technical,
                    VerificationStatus::TechnicalObservation,
                    ctx.evidence
                        .technical()
                        .map(|r| vec![r.id.clone()])
                        .unwrap_or_default(),
                    None,
                    String::new(),
                );
            }
        } else if found.is_empty() && has(&PRICE_WORDS) {
            let status = if has(&HYPOTHESIS_WORDS) {
                VerificationStatus::Hypothesis
            } else {
                VerificationStatus::TechnicalObservation
            };
            push!(
                if status == VerificationStatus::Hypothesis {
                    ClaimType::Hypothesis
                } else {
                    ClaimType::Technical
                },
                status,
                ctx.evidence
                    .technical()
                    .map(|r| vec![r.id.clone()])
                    .unwrap_or_default(),
                None,
                String::new(),
            );
        } else if found.is_empty() && has(&HYPOTHESIS_WORDS) {
            push!(
                ClaimType::Hypothesis,
                VerificationStatus::Hypothesis,
                vec![],
                None,
                String::new(),
            );
        }
        claims.append(&mut found);
    }
    claims
}

/// The worst status among `claims`, or None when there are none.
pub fn worst(claims: &[EvidenceClaim]) -> Option<VerificationStatus> {
    claims.iter().map(|c| c.verification_status).max()
}

/// The claims that must not be passed on as fact.
pub fn problems(claims: &[EvidenceClaim]) -> Vec<&EvidenceClaim> {
    claims
        .iter()
        .filter(|c| c.verification_status >= VerificationStatus::Unverified)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::security::resolve;

    fn today() -> chrono::NaiveDate {
        "2026-11-10".parse().unwrap()
    }

    fn item(title: &str, summary: &str, published: &str) -> Headline {
        Headline {
            title: title.into(),
            published: published.into(),
            link: format!("https://finance.yahoo.com/{}", short_hash(title)),
            guid: short_hash(title),
            summary: summary.into(),
        }
    }

    fn listed(symbol: &str, name: &str, etf: bool) -> SecurityIdentity {
        resolve(
            symbol,
            Some(&serde_json::json!({"symbol": symbol, "name": name, "exchange": "Q", "etf": etf})),
            None,
            "2026-11-10",
        )
    }

    fn pinned(symbol: &str) -> SecurityIdentity {
        resolve(symbol, None, None, "2026-11-10")
    }

    fn ramz_numbers() -> Technicals {
        Technicals {
            close: 24.11,
            momentum_20d: 0.358,
            momentum_5d: 0.216,
            volatility_20d: 0.114,
            max_gap_20d: 0.154,
            avg_dollar_volume: 10.2e6,
            bar_date: "2026-11-10".into(),
            max_drawdown: 0.0,
        }
    }

    #[test]
    fn shaver_shop_news_is_never_evidence_for_the_us_listed_ssg_fund() {
        // The two items the provider's feed returned for "SSG" on 10 November 2026.
        let ssg = pinned("SSG");
        let shaver = news_record(
            &ssg,
            &item(
                "Shaver Shop Group Ltd (ASX:SSG) (FY 2026) Earnings Call Highlights: Record Sales and Margins ...",
                "Shaver Shop Group Ltd (ASX:SSG) posts record revenue and gross profit margin for FY26.",
                "Tue, 01 Sep 2026 01:00:56 +0000",
            ),
            today(),
        );
        assert_eq!(shaver.status, MatchStatus::Rejected);
        assert_eq!(shaver.matched_exchange, "ASX");
        assert!(
            shaver.note.contains("a different listing from ARCX:SSG"),
            "{}",
            shaver.note
        );
        assert!(!shaver.verified_event());
        let penny = news_record(
            &ssg,
            &item(
                "ASX Penny Stocks With Market Caps Over A$100M To Consider",
                "As the Australian market reacts positively to the recent geopolitical developments...",
                "Mon, 15 Jun 2026 20:02:26 +0000",
            ),
            today(),
        );
        assert_eq!(penny.status, MatchStatus::Quarantined);
        let set = gather(
            &ssg,
            &Technicals::default(),
            &[
                item(
                    "Shaver Shop Group Ltd (ASX:SSG) Earnings Call Highlights: Record Sales",
                    "",
                    "Tue, 01 Sep 2026 01:00:56 +0000",
                ),
                item(
                    "ASX Penny Stocks With Market Caps Over A$100M To Consider",
                    "",
                    "",
                ),
            ],
            &[],
            None,
            today(),
        );
        assert_eq!(set.verified_events().len(), 0);
        assert_eq!(set.other_accepted().len(), 0);
        assert_eq!(set.counts()["rejected"], 1);
        assert_eq!(set.counts()["quarantined"], 1);
    }

    #[test]
    fn a_stock_gets_its_own_news_by_name_or_us_listing_and_not_a_namesakes() {
        let omh = listed("OMH", "Ohmyhome Limited - Class A Ordinary Shares", false);
        let recent = "Fri, 06 Nov 2026 13:00:00 +0000";
        let by_name = news_record(
            &omh,
            &item(
                "Ohmyhome Ltd Announces Pricing of $4 Million Registered Direct Offering",
                "",
                recent,
            ),
            today(),
        );
        assert_eq!(by_name.status, MatchStatus::Accepted);
        assert_eq!(by_name.relation, Relation::Issuer);
        assert_eq!(by_name.class, EvidenceClass::CompanyEvent);
        assert!(by_name.verified_event());
        assert_eq!(by_name.published, "2026-11-06");
        assert_eq!(by_name.retrieved, "2026-11-10");
        assert_eq!(by_name.security_id, "XNAS:OMH");
        assert!(by_name.source.starts_with("https://"));
        let by_tag = news_record(
            &omh,
            &item(
                "Is Ohmyhome (NASDAQ:OMH) In A Good Position To Deliver On Growth Plans?",
                "",
                recent,
            ),
            today(),
        );
        assert_eq!(by_tag.status, MatchStatus::Accepted);
        assert_eq!(by_tag.class, EvidenceClass::AnalystInterpretation);
        assert!(!by_tag.verified_event(), "an opinion piece is not an event");
        let generic = news_record(&omh, &item("BC-Most Active Stocks", "", recent), today());
        assert_eq!(generic.status, MatchStatus::Quarantined);
        let other = news_record(
            &omh,
            &item("Acme Corp (NYSE:ACME) wins contract", "", recent),
            today(),
        );
        assert_eq!(other.status, MatchStatus::Rejected);
        // Same record twice gives the same id; a different item a different one.
        let again = news_record(
            &omh,
            &item(
                "Ohmyhome Ltd Announces Pricing of $4 Million Registered Direct Offering",
                "",
                recent,
            ),
            today(),
        );
        assert_eq!(by_name.id, again.id);
        assert_ne!(by_name.id, by_tag.id);
        assert!(by_name.id.starts_with("N-"));
    }

    #[test]
    fn a_short_common_name_needs_more_than_the_word() {
        let pool = pinned("POOL");
        let recent = "Fri, 06 Nov 2026 13:00:00 +0000";
        let full = news_record(
            &pool,
            &item(
                "Pool Corporation Reports Record Third Quarter Results",
                "",
                recent,
            ),
            today(),
        );
        assert_eq!(full.status, MatchStatus::Accepted);
        let short = news_record(
            &pool,
            &item("Pool Corp. raises full-year guidance", "", recent),
            today(),
        );
        assert_eq!(short.status, MatchStatus::Accepted);
        let word = news_record(
            &pool,
            &item("Liquidity Pool Risks Grow In DeFi", "", recent),
            today(),
        );
        assert_eq!(word.status, MatchStatus::Quarantined);
        let with_ticker = news_record(
            &pool,
            &item("Why Pool (POOL) Is a Top Dividend Stock", "", recent),
            today(),
        );
        assert_eq!(with_ticker.status, MatchStatus::Accepted);
        let max = pinned("MAX");
        let namesake = news_record(
            &max,
            &item(
                "Max Verstappen wins again; HBO Max raises prices",
                "",
                recent,
            ),
            today(),
        );
        assert_eq!(namesake.status, MatchStatus::Quarantined);
        let real = news_record(
            &max,
            &item(
                "MediaAlpha Announces Third Quarter 2026 Financial Results",
                "",
                recent,
            ),
            today(),
        );
        assert!(real.verified_event());
    }

    #[test]
    fn old_news_is_accepted_but_is_not_a_current_catalyst() {
        let omh = listed("OMH", "Ohmyhome Limited - Class A Ordinary Shares", false);
        let old = news_record(
            &omh,
            &item(
                "Ohmyhome Limited Announces Share Consolidation",
                "",
                "Mon, 24 Aug 2026 12:00:00 +0000",
            ),
            today(),
        );
        assert_eq!(old.status, MatchStatus::Accepted);
        assert_eq!(old.age_days, Some(78));
        assert!(!old.verified_event());
        let undated = news_record(
            &omh,
            &item("Ohmyhome Limited Announces Share Consolidation", "", ""),
            today(),
        );
        assert!(!undated.verified_event(), "no date, no claim of recency");
    }

    #[test]
    fn fund_news_is_split_into_fund_sponsor_underlying_and_sector() {
        let nvds = pinned("NVDS");
        let recent = "Fri, 06 Nov 2026 13:00:00 +0000";
        let underlying = news_record(
            &nvds,
            &item(
                "NVIDIA (NVDA) Announces Record Data Center Revenue",
                "",
                recent,
            ),
            today(),
        );
        assert_eq!(underlying.relation, Relation::Underlying);
        assert_eq!(underlying.status, MatchStatus::Context);
        assert!(underlying.note.contains("-1.5x"));
        assert!(!underlying.verified_event());
        let fund = news_record(
            &nvds,
            &item(
                "Tradr 1.5X Short NVDA Daily ETF Announces Reverse Split",
                "",
                recent,
            ),
            today(),
        );
        assert_eq!(fund.relation, Relation::Fund);
        assert!(fund.verified_event());
        let ssg = pinned("SSG");
        let sector = news_record(
            &ssg,
            &item(
                "Semiconductor stocks slide as export rules tighten",
                "",
                recent,
            ),
            today(),
        );
        assert_eq!(sector.relation, Relation::IndexOrSector);
        assert_eq!(sector.status, MatchStatus::Context);
        let sponsor = news_record(
            &ssg,
            &item("ProShares Launches Three New Funds", "", recent),
            today(),
        );
        assert_eq!(sponsor.relation, Relation::FundSponsor);
        assert!(!sponsor.verified_event());
    }

    fn ramz_context<'a>(
        identity: &'a SecurityIdentity,
        evidence: &'a EvidenceSet,
        technicals: &'a Technicals,
    ) -> Context<'a> {
        Context {
            identity,
            evidence,
            technicals,
            earnings_date: None,
        }
    }

    #[test]
    fn the_incident_reasoning_is_contradicted_and_unsupported() {
        let ramz = pinned("RAMZ");
        let numbers = ramz_numbers();
        let evidence = gather(&ramz, &numbers, &[], &[], None, today());
        let ctx = ramz_context(&ramz, &evidence, &numbers);
        // The judge's own sentence, 10 November 2026.
        let reasoning = "While several stocks show strong momentum, only RAMZ has a tangible, recent catalyst (strong price action and volume spikes) without being a speculative or leveraged play. MAX has a pullback and lacks a clear catalyst.";
        let claims = audit(reasoning, &[], &ctx, false);
        assert_eq!(claims.len(), 2, "{claims:#?}");
        assert_eq!(claims[0].claim_type, ClaimType::Instrument);
        assert_eq!(
            claims[0].verification_status,
            VerificationStatus::ContradictedAssertion
        );
        assert!(claims[0].note.contains("-2x the daily return of DRAM"));
        assert_eq!(claims[1].claim_type, ClaimType::Event);
        assert_eq!(
            claims[1].verification_status,
            VerificationStatus::UnsupportedAssertion
        );
        assert!(
            claims[1]
                .note
                .contains("technical observations, not a verified event")
        );
        assert_eq!(
            worst(&claims),
            Some(VerificationStatus::ContradictedAssertion)
        );
        // Triage's sentence about the same fund.
        let triage = audit(
            "High 20-day performance (+35.8%) and positive news. No earnings announcement in the next 14 days.",
            &[],
            &ctx,
            true,
        );
        assert!(triage.iter().any(|c| c.claim_type == ClaimType::Event
            && c.verification_status == VerificationStatus::UnsupportedAssertion));
    }

    #[test]
    fn price_and_volume_alone_never_satisfy_a_catalyst_claim() {
        let max = pinned("MAX");
        let numbers = Technicals {
            momentum_20d: 0.372,
            momentum_5d: -0.03,
            ..ramz_numbers()
        };
        let none = gather(&max, &numbers, &[], &[], None, today());
        let ctx = ramz_context(&max, &none, &numbers);
        for text in [
            "The stock has a clear catalyst: a 20-day gain of +37.2% on rising volume.",
            "Strong market interest and recent gains support a bullish thesis.",
            "Recent positive news supports the trend.",
        ] {
            let claims = audit(text, &[], &ctx, true);
            assert!(
                claims.iter().any(|c| c.claim_type == ClaimType::Event
                    && c.verification_status == VerificationStatus::UnsupportedAssertion),
                "{text}: {claims:#?}"
            );
        }
        // Saying the same numbers as numbers is fine.
        let plain = audit(
            "The 20-day gain is +37.2% and volume is rising.",
            &[],
            &ctx,
            true,
        );
        assert_eq!(
            worst(&plain),
            Some(VerificationStatus::TechnicalObservation)
        );
        // Citing the technical record does not turn it into an event.
        let technical_id = none.technical().unwrap().id.clone();
        let cited = audit("The catalyst is the breakout.", &[technical_id], &ctx, true);
        assert_eq!(
            worst(&cited),
            Some(VerificationStatus::UnsupportedAssertion)
        );
        // Saying there is none is accurate.
        let honest = audit(
            "There is no verified catalyst; this is a technical-only setup.",
            &[],
            &ctx,
            true,
        );
        assert_eq!(problems(&honest).len(), 0, "{honest:#?}");
    }

    #[test]
    fn a_verified_event_supports_a_catalyst_claim_only_when_it_exists_and_is_cited() {
        let max = pinned("MAX");
        let numbers = ramz_numbers();
        let news = [item(
            "MediaAlpha Announces Third Quarter 2026 Financial Results",
            "",
            "Thu, 05 Nov 2026 21:05:00 +0000",
        )];
        let evidence = gather(&max, &numbers, &news, &[], None, today());
        let event_id = evidence.verified_events()[0].id.clone();
        let ctx = ramz_context(&max, &evidence, &numbers);
        let good = audit(
            "The catalyst is the third-quarter report.",
            std::slice::from_ref(&event_id),
            &ctx,
            true,
        );
        assert_eq!(
            good[0].verification_status,
            VerificationStatus::VerifiedFact
        );
        assert_eq!(good[0].evidence_ids, vec![event_id]);
        assert_eq!(good[0].confidence, Some(0.88));
        let invented = audit(
            "The catalyst is a new contract.",
            &["N-deadbeef".into()],
            &ctx,
            true,
        );
        assert!(invented.iter().any(|c| c.claim_type == ClaimType::Citation
            && c.verification_status == VerificationStatus::UnsupportedAssertion));
        assert!(invented.iter().any(|c| c.claim_type == ClaimType::Event
            && c.verification_status == VerificationStatus::UnsupportedAssertion));
    }

    #[test]
    fn a_form_4_alone_does_not_prove_insider_buying() {
        let max = pinned("MAX");
        let numbers = ramz_numbers();
        let form = |detail: &str| Filing {
            form: "4".into(),
            date: "2026-11-05".into(),
            accession: "0001-26-000004".into(),
            document: "form4.xml".into(),
            detail: detail.into(),
            ..Filing::default()
        };
        let text = "Strong trend, and insider buying activity supports it.";
        // The form exists but was not read.
        let unread = gather(&max, &numbers, &[], &[form("")], None, today());
        let claims = audit(text, &[], &ramz_context(&max, &unread, &numbers), true);
        let insider = claims
            .iter()
            .find(|c| c.claim_type == ClaimType::InsiderActivity)
            .unwrap();
        assert_eq!(
            insider.verification_status,
            VerificationStatus::UnsupportedAssertion
        );
        assert!(
            insider
                .note
                .contains("a filing alone does not show direction")
        );
        // It was read and shows a sale.
        let sale = gather(
            &max,
            &numbers,
            &[],
            &[form(
                "CFO J. Doe: open-market sale of 10,000 shares at $12.50",
            )],
            None,
            today(),
        );
        let claims = audit(text, &[], &ramz_context(&max, &sale, &numbers), true);
        assert!(
            claims
                .iter()
                .any(|c| c.claim_type == ClaimType::InsiderActivity
                    && c.verification_status == VerificationStatus::UnsupportedAssertion)
        );
        // It was read and shows a purchase.
        let buy = gather(
            &max,
            &numbers,
            &[],
            &[form(
                "Director A. Roe: open-market purchase of 5,000 shares at $11.90",
            )],
            None,
            today(),
        );
        let claims = audit(text, &[], &ramz_context(&max, &buy, &numbers), true);
        assert!(
            claims
                .iter()
                .any(|c| c.claim_type == ClaimType::InsiderActivity
                    && c.verification_status == VerificationStatus::VerifiedFact)
        );
    }

    #[test]
    fn wrong_numbers_calendar_claims_and_missing_data_are_flagged() {
        let max = pinned("MAX");
        let numbers = ramz_numbers();
        let evidence = gather(&max, &numbers, &[], &[], None, today());
        let ctx = ramz_context(&max, &evidence, &numbers);
        let wrong = audit("The 20-day gain of +58.0% is exceptional.", &[], &ctx, true);
        assert_eq!(
            wrong[0].verification_status,
            VerificationStatus::ContradictedAssertion
        );
        assert!(wrong[0].note.contains("+35.8%"));
        let right = audit(
            "A 20-day gain of +35.8% and a 5-day gain of +21.6%.",
            &[],
            &ctx,
            true,
        );
        assert_eq!(
            worst(&right),
            Some(VerificationStatus::TechnicalObservation)
        );
        let earnings = audit("Buy ahead of earnings next week.", &[], &ctx, true);
        assert_eq!(
            earnings[0].verification_status,
            VerificationStatus::ContradictedAssertion
        );
        let missing = audit(
            "There are no earnings reports scheduled, meaning the move is not supported by fundamentals.",
            &[],
            &ctx,
            true,
        );
        let calendar = missing
            .iter()
            .find(|c| c.claim_type == ClaimType::Calendar)
            .unwrap();
        assert_eq!(calendar.verification_status, VerificationStatus::Unverified);
        assert!(calendar.note.contains("calendar observation"));
        // Stating the calendar without leaning on it is fine.
        let neutral = audit(
            "No earnings report is listed in the next 14 days.",
            &[],
            &ctx,
            true,
        );
        assert_eq!(problems(&neutral).len(), 0);
        // A listed date contradicts "no earnings".
        let dated = Context {
            earnings_date: Some("2026-11-17"),
            ..ramz_context(&max, &evidence, &numbers)
        };
        let claims = audit(
            "There is no earnings report, which raises a concern.",
            &[],
            &dated,
            true,
        );
        assert_eq!(
            claims[0].verification_status,
            VerificationStatus::ContradictedAssertion
        );
    }

    #[test]
    fn invented_price_facts_from_week_one_are_contradicted() {
        // "POOL ... recently fell 58% in a prior report" came from a valuation blog's title.
        let pool = pinned("POOL");
        let numbers = Technicals {
            close: 310.0,
            max_drawdown: 0.12,
            ..ramz_numbers()
        };
        let evidence = gather(&pool, &numbers, &[], &[], None, today());
        let ctx = ramz_context(&pool, &evidence, &numbers);
        let claims = audit(
            "Despite margin stability, the stock recently fell 58% in a prior report.",
            &[],
            &ctx,
            true,
        );
        let fall = claims
            .iter()
            .find(|c| c.note.contains("largest fall"))
            .unwrap();
        assert_eq!(
            fall.verification_status,
            VerificationStatus::ContradictedAssertion
        );
        // A fall the bars do show is fine.
        assert_eq!(
            problems(&audit("It dropped 10% last week.", &[], &ctx, true)).len(),
            0
        );
        // "SSG ... it's a penny stock" at a $16 close.
        let claims = audit(
            "High volatility and penny stock classification indicate risk.",
            &[],
            &ctx,
            true,
        );
        assert!(
            claims
                .iter()
                .any(|c| c.note.contains("penny stock; the last close is $310.00"))
        );
    }

    #[test]
    fn calling_a_fund_a_company_or_a_stock_a_leveraged_product_is_contradicted() {
        let ssg = pinned("SSG");
        let numbers = ramz_numbers();
        let evidence = gather(&ssg, &numbers, &[], &[], None, today());
        let ctx = ramz_context(&ssg, &evidence, &numbers);
        let claims = audit(
            "SSG has strong momentum and record sales news, but it's a penny stock.",
            &[],
            &ctx,
            false,
        );
        assert!(claims.iter().any(|c| c.claim_type == ClaimType::Instrument
            && c.verification_status == VerificationStatus::ContradictedAssertion));
        let max = pinned("MAX");
        let evidence = gather(&max, &numbers, &[], &[], None, today());
        let ctx = ramz_context(&max, &evidence, &numbers);
        let claims = audit(
            "MAX is a leveraged ETF and should be avoided.",
            &[],
            &ctx,
            false,
        );
        assert_eq!(
            claims[0].verification_status,
            VerificationStatus::ContradictedAssertion
        );
        // Unscoped text about another symbol is not read.
        assert_eq!(audit("POOL is not leveraged.", &[], &ctx, false).len(), 0);
    }

    #[test]
    fn forward_views_are_hypotheses_not_facts() {
        let max = pinned("MAX");
        let numbers = ramz_numbers();
        let evidence = gather(&max, &numbers, &[], &[], None, today());
        let ctx = ramz_context(&max, &evidence, &numbers);
        let claims = audit(
            "If the trend holds, the price could reach the target within two weeks.",
            &[],
            &ctx,
            true,
        );
        assert_eq!(
            claims[0].verification_status,
            VerificationStatus::Hypothesis
        );
        assert_eq!(problems(&claims).len(), 0);
    }

    #[test]
    fn filings_become_sourced_records_and_a_trusts_filings_are_only_context() {
        let max = SecurityIdentity {
            cik: Some("0001818383".into()),
            ..pinned("MAX")
        };
        let filing = Filing {
            form: "8-K".into(),
            date: "2026-11-05".into(),
            items: "2.02,9.01".into(),
            accession: "0001818383-26-000040".into(),
            document: "max-8k.htm".into(),
            ..Filing::default()
        };
        let record = filing_record(&max, &filing, today());
        assert!(record.id.starts_with("F-"));
        assert_eq!(
            record.source,
            "https://www.sec.gov/Archives/edgar/data/1818383/000181838326000040/max-8k.htm"
        );
        assert_eq!(record.publisher, "SEC EDGAR");
        assert!(record.verified_event());
        let fund = filing_record(&pinned("SSG"), &filing, today());
        assert_eq!(fund.status, MatchStatus::Context);
        assert!(!fund.verified_event());
    }

    #[test]
    fn listing_tags_and_sentences_parse() {
        assert_eq!(
            listing_tags("Shaver Shop Group Ltd (ASX:SSG) and Ohmyhome (NASDAQ: OMH) (FY 2026)"),
            vec![
                ("ASX".to_string(), "SSG".to_string()),
                ("NASDAQ".to_string(), "OMH".to_string())
            ]
        );
        assert_eq!(listing_tags("Revenue (up 5%: a record)").len(), 0);
        assert_eq!(
            sentences("Gain of 35.8% today. Volume rose 2.7x. Done").len(),
            3
        );
        assert!(
            (percent_after(" a 20-day gain of +35.8% and ", "20-day").unwrap() - 0.358).abs()
                < 1e-9
        );
        assert!((percent_after(" 20d -3.6% ", "20d").unwrap() + 0.036).abs() < 1e-9);
    }

    fn feed(xml: &str) -> Vec<Headline> {
        crate::intel::parse_rss(xml, 40)
    }

    #[test]
    fn the_real_feed_of_10_november_is_reduced_to_what_the_issuers_announced() {
        // The RealReal: the desk bought it citing a board appointment as "a clear
        // governance upgrade" and an "analyst upside of 25.28%".
        let real = listed("REAL", "The RealReal, Inc. - Common Stock", false);
        let set = gather(
            &real,
            &Technicals::default(),
            &feed(include_str!(
                "../tests/fixtures/incident-2026-11-10/rss_REAL.xml"
            )),
            &[],
            None,
            today(),
        );
        let events: Vec<&str> = set
            .verified_events()
            .iter()
            .map(|r| r.excerpt.as_str())
            .collect();
        assert_eq!(
            events,
            ["THE REALREAL ANNOUNCES THIRD QUARTER 2026 RESULTS"]
        );
        let minor: Vec<&str> = set
            .minor_notices()
            .iter()
            .map(|r| r.excerpt.as_str())
            .collect();
        assert_eq!(
            minor,
            [
                "The RealReal Appoints Shar Dubey to Board of Directors",
                "The RealReal Appoints Alexis Hoopes as Chief Merchandising Officer"
            ]
        );
        for hidden in [
            "25.28%",
            "Great Momentum Stock",
            "Time to Buy",
            "What’s Changed",
            "Transcript",
        ] {
            let record = set
                .records
                .iter()
                .find(|r| r.excerpt.contains(hidden))
                .unwrap();
            assert!(
                !record.shown(),
                "{hidden} must be withheld: {}",
                record.note
            );
        }
        assert!(set.withheld().len() >= 12, "{}", set.withheld().len());
        // Replimune: "could rally 26.8%" and "soars 17%" went into the buy thesis.
        let repl = listed("REPL", "Replimune Group, Inc. - Common Stock", false);
        let set = gather(
            &repl,
            &Technicals::default(),
            &feed(include_str!(
                "../tests/fixtures/incident-2026-11-10/rss_REPL.xml"
            )),
            &[],
            None,
            today(),
        );
        let events: Vec<&str> = set
            .verified_events()
            .iter()
            .map(|r| r.excerpt.as_str())
            .collect();
        assert_eq!(
            events,
            [
                "Replimune Reports Fiscal Second Quarter 2027 Financial Results and Provides Corporate Update"
            ]
        );
        assert!(set.records.iter().filter(|r| r.shown()).all(|r| {
            !r.excerpt.contains("26.8%")
                && !r.excerpt.contains("Soars")
                && !r.excerpt.contains("52-Week")
        }));
        // Pool: a valuation blog's "Shares Fell 58%" became "the stock recently fell 58%".
        let set = gather(
            &pinned("POOL"),
            &Technicals::default(),
            &feed(include_str!(
                "../tests/fixtures/incident-2026-11-10/rss_POOL.xml"
            )),
            &[],
            None,
            today(),
        );
        assert!(
            set.records
                .iter()
                .filter(|r| r.shown())
                .all(|r| !r.excerpt.contains("58%"))
        );
        assert_eq!(set.verified_events().len(), 0);
    }
}
