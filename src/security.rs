//! The security master: what each ticker actually is.
//!
//! A ticker string says nothing about economic exposure. `RAMZ` is a fund that seeks −200%
//! of another fund's daily return, `SSG` is −200% of a semiconductor index, and on the
//! Australian exchange `SSG` is a retailer. Every stage that screens, researches or buys
//! therefore works from a [`SecurityIdentity`], never from the ticker alone.
//!
//! Sources, in the order they are trusted:
//!
//! 1. [`PINNED`]: records checked by hand against the issuer's own fund page and the SEC
//!    filing. They double as regression fixtures.
//! 2. The Nasdaq Trader symbol directory: the exchange's registered security name, listing
//!    exchange and ETF flag. A geared fund's registered name states its multiple and
//!    direction ("2X Inverse", "Bear 3X", "`UltraShort`"), so the classification is read from
//!    the listing record and not guessed from the ticker.
//! 3. The price provider's instrument type, used only to corroborate or contradict the
//!    directory. A disagreement leaves the identity unverified.
//!
//! Anything that cannot be resolved is [`InstrumentType::Unknown`] and unverified. The gates
//! treat that as a reason not to buy; it is never silently treated as a common stock.

use crate::model::Observation;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::path::Path;

/// What kind of instrument a symbol is.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InstrumentType {
    CommonStock,
    Adr,
    /// An unleveraged, long-only exchange-traded fund.
    OrdinaryEtf,
    /// Seeks a multiple above 1 of its reference's return.
    LeveragedEtf,
    /// Seeks the opposite of its reference's return, at 1x.
    InverseEtf,
    /// Seeks a negative multiple beyond −1 of its reference's return.
    LeveragedInverseEtf,
    /// An exchange-traded note: unsecured issuer debt, not a fund.
    Etn,
    /// Resolved, but not something the account trades: warrants, units, rights, preferred
    /// shares, notes, closed-end funds.
    Other,
    /// Not resolved. Never treated as an ordinary equity.
    #[default]
    Unknown,
}

impl InstrumentType {
    /// A short lowercase label for prompts and the dashboard.
    pub fn label(self) -> &'static str {
        match self {
            Self::CommonStock => "common stock",
            Self::Adr => "ADR",
            Self::OrdinaryEtf => "ETF (unleveraged)",
            Self::LeveragedEtf => "leveraged ETF",
            Self::InverseEtf => "inverse ETF",
            Self::LeveragedInverseEtf => "leveraged inverse ETF",
            Self::Etn => "exchange-traded note",
            Self::Other => "unsupported instrument",
            Self::Unknown => "unknown instrument",
        }
    }

    pub fn is_fund(self) -> bool {
        matches!(
            self,
            Self::OrdinaryEtf
                | Self::LeveragedEtf
                | Self::InverseEtf
                | Self::LeveragedInverseEtf
                | Self::Etn
        )
    }

    pub fn is_geared(self) -> bool {
        matches!(
            self,
            Self::LeveragedEtf | Self::InverseEtf | Self::LeveragedInverseEtf
        )
    }
}

/// The validated record for one symbol.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct SecurityIdentity {
    pub symbol: String,
    /// Listing exchange code from the directory (`Q` Nasdaq, `N` NYSE, `P` NYSE Arca,
    /// `Z` Cboe BZX, `A` NYSE American), or empty when unknown.
    #[serde(default)]
    pub exchange: String,
    /// The registered security name.
    #[serde(default)]
    pub canonical_name: String,
    #[serde(default)]
    pub instrument_type: InstrumentType,
    /// The company, or a fund's sponsor.
    #[serde(default)]
    pub issuer_name: Option<String>,
    #[serde(default)]
    pub cik: Option<String>,
    /// No free source provides these three; they stay empty rather than guessed.
    #[serde(default)]
    pub figi: Option<String>,
    #[serde(default)]
    pub cusip: Option<String>,
    #[serde(default)]
    pub isin: Option<String>,
    /// For a single-stock or single-fund product, the ticker it tracks.
    #[serde(default)]
    pub underlying_symbol: Option<String>,
    /// For an index or sector product, what it tracks in words.
    #[serde(default)]
    pub underlying_name: Option<String>,
    /// Absolute multiple of the reference's return the product seeks. None when not geared
    /// or not stated.
    #[serde(default)]
    pub leverage_multiple: Option<f64>,
    #[serde(default)]
    pub inverse: bool,
    /// The multiple applies to one day's return and compounds, so longer holding periods
    /// drift from the multiple.
    #[serde(default)]
    pub daily_reset: bool,
    /// False when the sources are missing, disagree, or the name could not be classified.
    #[serde(default)]
    pub verified: bool,
    /// The date the record was last checked, `YYYY-MM-DD`.
    #[serde(default)]
    pub verified_as_of: String,
    /// Which sources produced the record.
    #[serde(default)]
    pub source: String,
    /// Why the record is unverified, or what was notable while resolving it.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub notes: Vec<String>,
}

impl SecurityIdentity {
    /// An unresolved record for `symbol`.
    pub fn unknown(symbol: &str, why: &str) -> Self {
        SecurityIdentity {
            symbol: symbol.to_string(),
            notes: vec![why.to_string()],
            ..SecurityIdentity::default()
        }
    }

    /// A stable id for evidence records: listing exchange and symbol.
    pub fn security_id(&self) -> String {
        let venue = match self.exchange.as_str() {
            "Q" => "XNAS",
            "N" => "XNYS",
            "P" => "ARCX",
            "Z" => "BATS",
            "A" => "XASE",
            "V" => "IEXG",
            "" => "US",
            other => other,
        };
        format!("{venue}:{}", self.symbol)
    }

    /// The signed multiple, for example −2.0 for a 2x inverse fund. None when not geared.
    pub fn signed_multiple(&self) -> Option<f64> {
        if !self.instrument_type.is_geared() {
            return None;
        }
        let m = self.leverage_multiple.unwrap_or(1.0);
        Some(if self.inverse { -m } else { m })
    }

    /// One line a model or a reader cannot misread.
    pub fn describe(&self) -> String {
        let mut out = format!(
            "{} = {} [{}]",
            self.symbol,
            if self.canonical_name.is_empty() {
                "name unknown"
            } else {
                &self.canonical_name
            },
            self.instrument_type.label()
        );
        if let Some(m) = self.signed_multiple() {
            let target = self
                .underlying_symbol
                .as_deref()
                .or(self.underlying_name.as_deref())
                .unwrap_or("its reference");
            let _ = write!(
                out,
                "; seeks {m:+}x the {} return of {target}",
                if self.daily_reset { "daily" } else { "stated" }
            );
        }
        if !self.verified {
            out.push_str("; identity NOT verified");
        }
        out
    }
}

/// A hand-checked record.
#[derive(Debug)]
pub struct Pinned {
    pub symbol: &'static str,
    pub exchange: &'static str,
    pub name: &'static str,
    pub kind: InstrumentType,
    pub issuer: &'static str,
    pub underlying_symbol: Option<&'static str>,
    pub underlying_name: Option<&'static str>,
    pub multiple: Option<f64>,
    pub inverse: bool,
    pub daily_reset: bool,
    pub verified_as_of: &'static str,
    pub source: &'static str,
}

/// Records checked against issuer documentation. They override the directory parse and are
/// the regression fixtures for the 10 November 2026 incident, when the desk bought `RAMZ`
/// while calling it "not leveraged".
pub const PINNED: &[Pinned] = &[
    Pinned {
        symbol: "RAMZ",
        exchange: "Z",
        name: "T-REX 2X Inverse DRAM Daily Target ETF",
        kind: InstrumentType::LeveragedInverseEtf,
        issuer: "REX Shares / Tuttle Capital (ETF Opportunities Trust)",
        underlying_symbol: Some("DRAM"),
        underlying_name: Some("Roundhill Memory ETF"),
        multiple: Some(2.0),
        inverse: true,
        daily_reset: true,
        verified_as_of: "2026-11-10",
        source: "https://www.rexshares.com/ramz/ ; https://www.sec.gov/Archives/edgar/data/1771146/000177114626001501/ck0001771146-20260727.htm",
    },
    Pinned {
        symbol: "SSG",
        exchange: "P",
        name: "ProShares UltraShort Semiconductors",
        kind: InstrumentType::LeveragedInverseEtf,
        issuer: "ProShares Trust",
        underlying_symbol: None,
        underlying_name: Some("Dow Jones U.S. Semiconductors index"),
        multiple: Some(2.0),
        inverse: true,
        daily_reset: true,
        verified_as_of: "2026-11-10",
        source: "https://www.proshares.com/our-etfs/leveraged-and-inverse/ssg",
    },
    Pinned {
        symbol: "SNDQ",
        exchange: "Z",
        name: "Tradr 2X Short SNDK Daily ETF",
        kind: InstrumentType::LeveragedInverseEtf,
        issuer: "Tradr ETFs (AXS Investments)",
        underlying_symbol: Some("SNDK"),
        underlying_name: Some("Sandisk Corporation"),
        multiple: Some(2.0),
        inverse: true,
        daily_reset: true,
        verified_as_of: "2026-11-10",
        source: "https://www.tradretfs.com/ ; Nasdaq Trader symbol directory",
    },
    Pinned {
        symbol: "SOXS",
        exchange: "P",
        name: "Direxion Daily Semiconductor Bear 3X ETF",
        kind: InstrumentType::LeveragedInverseEtf,
        issuer: "Direxion Shares ETF Trust",
        underlying_symbol: None,
        underlying_name: Some("NYSE Semiconductor Index"),
        multiple: Some(3.0),
        inverse: true,
        daily_reset: true,
        verified_as_of: "2026-11-10",
        source: "https://www.direxion.com/product/daily-semiconductor-bull-bear-3x-etfs",
    },
    Pinned {
        symbol: "NVDS",
        exchange: "Q",
        name: "Tradr 1.5X Short NVDA Daily ETF",
        kind: InstrumentType::LeveragedInverseEtf,
        issuer: "Tradr ETFs (AXS Investments)",
        underlying_symbol: Some("NVDA"),
        underlying_name: Some("NVIDIA Corporation"),
        multiple: Some(1.5),
        inverse: true,
        daily_reset: true,
        verified_as_of: "2026-11-10",
        source: "https://www.tradretfs.com/ ; Nasdaq Trader symbol directory",
    },
    Pinned {
        symbol: "MAX",
        exchange: "N",
        name: "MediaAlpha, Inc. Class A Common Stock",
        kind: InstrumentType::CommonStock,
        issuer: "MediaAlpha, Inc.",
        underlying_symbol: None,
        underlying_name: None,
        multiple: None,
        inverse: false,
        daily_reset: false,
        verified_as_of: "2026-11-10",
        source: "Nasdaq Trader symbol directory; SEC EDGAR company record",
    },
    Pinned {
        symbol: "POOL",
        exchange: "Q",
        name: "Pool Corporation - Common Stock",
        kind: InstrumentType::CommonStock,
        issuer: "Pool Corporation",
        underlying_symbol: None,
        underlying_name: None,
        multiple: None,
        inverse: false,
        daily_reset: false,
        verified_as_of: "2026-11-10",
        source: "Nasdaq Trader symbol directory; SEC EDGAR company record",
    },
];

/// What a registered security name says about the instrument.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Parsed {
    pub kind: InstrumentType,
    pub multiple: Option<f64>,
    pub inverse: bool,
    pub daily_reset: bool,
    pub underlying_symbol: Option<String>,
    pub underlying_name: Option<String>,
    pub issuer: Option<String>,
    /// The name alone was enough. False for a plain company name with no share-class
    /// words, which needs a second source.
    pub conclusive: bool,
    pub note: Option<String>,
}

/// Words after "short" that make it a bond tenor and not a direction.
const TENOR_WORDS: [&str; 18] = [
    "duration",
    "term",
    "maturity",
    "treasury",
    "treasuries",
    "bond",
    "bonds",
    "income",
    "municipal",
    "muni",
    "corporate",
    "government",
    "credit",
    "dated",
    "tips",
    "high",
    "investment",
    "t-bill",
];

/// Share-class words that mark a company's own equity.
const EQUITY_WORDS: [&str; 9] = [
    "common stock",
    "common stcok",
    "common shares",
    "common share",
    "ordinary shares",
    "ordinary share",
    "capital stock",
    "subordinate voting",
    "class a",
];

const ADR_WORDS: [&str; 3] = [
    "american depositary",
    "american depository",
    "depositary receipt",
];

/// Words that mark something listed but not an equity the account trades.
const UNSUPPORTED_WORDS: [&str; 18] = [
    "warrant",
    "warrants",
    "right",
    "rights",
    "unit",
    "units",
    "preferred",
    "preference",
    "notes",
    "note",
    "debenture",
    "debentures",
    "bond",
    "bonds",
    "depositary shares",
    "depositary share",
    "fund",
    "%",
];

fn multiplier(token: &str) -> Option<(f64, bool)> {
    let negative = token.starts_with('-') || token.starts_with('−');
    let body = token.trim_start_matches(['-', '−']);
    let digits = body.strip_suffix(['x', '×', '?'])?;
    let value: f64 = digits.parse().ok()?;
    (value.is_finite() && (0.5..=10.0).contains(&value)).then_some((value, negative))
}

fn tokens(name: &str) -> Vec<String> {
    name.split(|c: char| c.is_whitespace() || matches!(c, '(' | ')' | ',' | ';'))
        .map(|t| {
            t.trim_matches(|c: char| matches!(c, '.' | ':' | '"' | '\''))
                .to_lowercase()
        })
        .filter(|t| !t.is_empty())
        .collect()
}

fn ticker_like(word: &str) -> bool {
    (1..=5).contains(&word.len()) && word.chars().all(|c| c.is_ascii_uppercase())
}

/// The sponsor's brand: the words before the first one that describes the product.
fn fund_brand(name: &str) -> Option<String> {
    let stop = [
        "daily",
        "ultra",
        "ultrashort",
        "ultrapro",
        "short",
        "long",
        "inverse",
        "bull",
        "bear",
        "etf",
        "fund",
        "trust",
    ];
    let words: Vec<&str> = name.split_whitespace().collect();
    let cut = words
        .iter()
        .position(|w| {
            let lower = w.to_lowercase();
            stop.contains(&lower.as_str()) || multiplier(&lower).is_some()
        })
        .unwrap_or(words.len().min(2));
    let brand = words[..cut.clamp(1, 3).min(words.len())].join(" ");
    (!brand.is_empty()).then_some(brand)
}

fn parse_fund(name: &str) -> Parsed {
    let words = tokens(name);
    let raw: Vec<&str> = name.split_whitespace().collect();
    let has = |w: &str| words.iter().any(|t| t == w);
    let issuer = fund_brand(name);
    if has("etn") || has("etns") || name.to_lowercase().contains("exchange traded note") {
        return Parsed {
            kind: InstrumentType::Etn,
            issuer,
            conclusive: true,
            ..Parsed::default()
        };
    }
    let proshares = words.first().is_some_and(|w| w == "proshares");
    let mut multiple: Option<f64> = None;
    let mut inverse = false;
    let mut long = false;
    for (i, word) in words.iter().enumerate() {
        if let Some((value, negative)) = multiplier(word) {
            multiple = Some(multiple.map_or(value, |m: f64| m.max(value)));
            inverse |= negative;
            continue;
        }
        let next = words.get(i + 1).map_or("", String::as_str);
        match word.as_str() {
            "ultrashort" if proshares => {
                multiple = Some(2.0);
                inverse = true;
            }
            "ultrapro" if proshares => {
                multiple = Some(3.0);
                inverse |= next == "short";
            }
            "ultra" if proshares => multiple = Some(multiple.unwrap_or(2.0)),
            "short" if proshares => inverse = true,
            "short" => {
                let tenor = TENOR_WORDS.contains(&next)
                    || next.starts_with("term")
                    || (i > 0 && words[i - 1] == "ultra");
                if !tenor {
                    inverse = true;
                }
            }
            "inverse" | "bear" => inverse = true,
            "long" | "bull" => long = true,
            _ => {}
        }
    }
    // "Long/Short" names a strategy that holds both sides, not an inverse fund.
    let lower = name.to_lowercase();
    if lower.contains("long/short") || lower.contains("long-short") {
        inverse = false;
    }
    let geared_word = has("leveraged") || lower.contains("daily target");
    let kind = match (multiple, inverse) {
        (Some(m), true) if m > 1.0 => InstrumentType::LeveragedInverseEtf,
        (_, true) => InstrumentType::InverseEtf,
        (Some(m), false) if m > 1.0 => InstrumentType::LeveragedEtf,
        (None, false) if geared_word => InstrumentType::Other,
        _ => InstrumentType::OrdinaryEtf,
    };
    let _ = long;
    // A single-stock product names its reference ticker right after the direction word.
    let underlying_symbol = kind
        .is_geared()
        .then(|| {
            raw.iter().enumerate().find_map(|(i, w)| {
                let lower = w.to_lowercase();
                let after_direction = i > 0
                    && matches!(
                        raw[i - 1].to_lowercase().as_str(),
                        "short" | "long" | "inverse"
                    );
                let before_direction = raw.get(i + 1).is_some_and(|n| {
                    let n = n.to_lowercase();
                    matches!(n.as_str(), "bull" | "bear") || multiplier(&n).is_some()
                });
                (ticker_like(w)
                    && !matches!(lower.as_str(), "etf" | "etn" | "x")
                    && (after_direction || before_direction))
                    .then(|| (*w).to_string())
            })
        })
        .flatten();
    Parsed {
        kind,
        multiple: if kind.is_geared() {
            Some(multiple.unwrap_or(1.0))
        } else {
            None
        },
        inverse: kind.is_geared() && inverse,
        daily_reset: kind.is_geared() && (has("daily") || proshares),
        underlying_symbol,
        underlying_name: None,
        issuer,
        conclusive: kind != InstrumentType::Other,
        note: (kind == InstrumentType::Other)
            .then(|| "fund name suggests gearing but states no multiple".to_string()),
    }
}

/// The company name: the registered name up to its share-class description.
pub fn company_name(name: &str) -> String {
    let lower = name.to_lowercase();
    let mut cut = name.len();
    if let Some(at) = lower.find(" - ") {
        cut = cut.min(at);
    }
    for marker in EQUITY_WORDS.iter().chain(ADR_WORDS.iter()).chain(
        [
            "class b",
            "class c",
            "(reit)",
            "each representing",
            " ads",
            " adr",
        ]
        .iter(),
    ) {
        if let Some(at) = lower.find(marker) {
            cut = cut.min(at);
        }
    }
    name[..cut]
        .trim()
        .trim_end_matches([',', '-'])
        .trim()
        .to_string()
}

/// Classify one directory record from its registered name and ETF flag.
pub fn parse_name(name: &str, etf: bool) -> Parsed {
    let lower = format!(" {} ", name.to_lowercase());
    let words = tokens(name);
    if etf
        || words
            .iter()
            .any(|w| w == "etn" || w == "etns" || w == "etf")
    {
        return parse_fund(name);
    }
    let issuer = Some(company_name(name)).filter(|n| !n.is_empty());
    // "American Depositary Shares" is an ADR; "Depositary Shares ... Preferred" is not.
    let adr = (ADR_WORDS.iter().any(|w| lower.contains(w))
        || words.iter().any(|t| t == "ads" || t == "adr"))
        && !["preferred", "preference", "warrant", "unit", "right"]
            .iter()
            .any(|w| words.iter().any(|t| t.starts_with(w)));
    if adr {
        return Parsed {
            kind: InstrumentType::Adr,
            issuer,
            conclusive: true,
            ..Parsed::default()
        };
    }
    let unsupported = UNSUPPORTED_WORDS.iter().find(|w| {
        if **w == "%" {
            lower.contains('%')
        } else {
            words.iter().any(|t| t == *w) || (w.contains(' ') && lower.contains(*w))
        }
    });
    if let Some(word) = unsupported {
        return Parsed {
            kind: InstrumentType::Other,
            issuer,
            conclusive: true,
            note: Some(format!("registered name contains \"{word}\"")),
            ..Parsed::default()
        };
    }
    let equity = EQUITY_WORDS.iter().any(|w| lower.contains(w))
        || lower.contains("class b")
        || lower.contains("class c")
        || lower.contains("(reit)")
        || lower.contains("beneficial interest");
    Parsed {
        kind: InstrumentType::CommonStock,
        issuer,
        conclusive: equity,
        note: (!equity)
            .then(|| "registered name has no share-class words; needs a second source".into()),
        ..Parsed::default()
    }
}

/// Resolves symbols for one observation.
#[derive(Debug)]
pub struct Master<'a> {
    directory: BTreeMap<&'a str, &'a Value>,
    observation: &'a Observation,
    as_of: String,
}

impl<'a> Master<'a> {
    pub fn new(observation: &'a Observation) -> Self {
        let directory = observation
            .universe
            .iter()
            .filter_map(|row| Some((row["symbol"].as_str()?, row)))
            .collect();
        let as_of = chrono::DateTime::from_timestamp(observation.at as i64, 0)
            .unwrap_or_default()
            .date_naive()
            .to_string();
        Master {
            directory,
            observation,
            as_of,
        }
    }

    /// The identity of `symbol`: a record frozen into the observation if there is one,
    /// otherwise resolved from the pinned table, the directory and the price provider.
    pub fn identify(&self, symbol: &str) -> SecurityIdentity {
        if let Some(frozen) = self.observation.securities.get(symbol) {
            return frozen.clone();
        }
        let row = self.directory.get(symbol).copied();
        let provider_type = self
            .observation
            .quotes
            .get(symbol)
            .and_then(|q| q.instrument_type.as_deref());
        resolve(symbol, row, provider_type, &self.as_of)
    }
}

fn from_pinned(pin: &Pinned, row: Option<&Value>) -> SecurityIdentity {
    let mut identity = SecurityIdentity {
        symbol: pin.symbol.to_string(),
        exchange: pin.exchange.to_string(),
        canonical_name: pin.name.to_string(),
        instrument_type: pin.kind,
        issuer_name: Some(pin.issuer.to_string()),
        underlying_symbol: pin.underlying_symbol.map(str::to_string),
        underlying_name: pin.underlying_name.map(str::to_string),
        leverage_multiple: pin.multiple,
        inverse: pin.inverse,
        daily_reset: pin.daily_reset,
        verified: true,
        verified_as_of: pin.verified_as_of.to_string(),
        source: format!("pinned issuer documentation: {}", pin.source),
        ..SecurityIdentity::default()
    };
    // A ticker can be reassigned. If today's directory lists a different security under
    // the pinned symbol, the pin no longer applies and nothing is verified.
    if let Some(listed) = row
        .and_then(|r| r["name"].as_str())
        .filter(|n| !n.is_empty())
    {
        let first = |s: &str| {
            s.split_whitespace()
                .next()
                .unwrap_or("")
                .trim_matches(',')
                .to_lowercase()
        };
        if first(listed) != first(pin.name) {
            identity.verified = false;
            identity.instrument_type = InstrumentType::Unknown;
            identity.notes.push(format!(
                "directory now lists \"{listed}\" under this symbol; the pinned record is for \"{}\"",
                pin.name
            ));
        }
    }
    identity
}

/// Resolve one symbol from its directory row and the price provider's instrument type.
pub fn resolve(
    symbol: &str,
    row: Option<&Value>,
    provider_type: Option<&str>,
    as_of: &str,
) -> SecurityIdentity {
    if let Some(pin) = PINNED.iter().find(|p| p.symbol == symbol) {
        return from_pinned(pin, row);
    }
    let Some(row) = row else {
        return SecurityIdentity::unknown(symbol, "symbol is not in the exchange directory");
    };
    let name = row["name"].as_str().unwrap_or("").trim();
    if name.is_empty() {
        return SecurityIdentity::unknown(symbol, "the directory record has no security name");
    }
    let etf = row["etf"].as_bool().unwrap_or(false);
    let parsed = parse_name(name, etf);
    let mut identity = SecurityIdentity {
        symbol: symbol.to_string(),
        exchange: row["exchange"].as_str().unwrap_or("").to_string(),
        canonical_name: name.to_string(),
        instrument_type: parsed.kind,
        issuer_name: parsed.issuer.clone(),
        underlying_symbol: parsed.underlying_symbol.clone(),
        underlying_name: parsed.underlying_name.clone(),
        leverage_multiple: parsed.multiple,
        inverse: parsed.inverse,
        daily_reset: parsed.daily_reset,
        verified: parsed.conclusive,
        verified_as_of: as_of.to_string(),
        source: "Nasdaq Trader symbol directory (registered name, exchange, ETF flag)".into(),
        ..SecurityIdentity::default()
    };
    if let Some(note) = parsed.note {
        identity.notes.push(note);
    }
    // The second source can corroborate a plain company name or contradict the directory.
    if let Some(kind) = provider_type {
        let fund = parsed.kind.is_fund();
        match kind {
            "EQUITY" if !fund && parsed.kind != InstrumentType::Other => {
                identity.verified = true;
                identity
                    .source
                    .push_str("; price provider instrument type EQUITY");
                identity.notes.clear();
            }
            "ETF" if fund => identity
                .source
                .push_str("; price provider instrument type ETF"),
            "EQUITY" | "ETF" if parsed.kind != InstrumentType::Other => {
                identity.verified = false;
                identity.instrument_type = InstrumentType::Unknown;
                identity.notes.push(format!(
                    "sources disagree: the directory implies {}, the price provider says {kind}",
                    parsed.kind.label()
                ));
            }
            _ => {}
        }
    }
    if !identity.verified && identity.instrument_type == InstrumentType::CommonStock {
        identity.instrument_type = InstrumentType::Unknown;
    }
    identity
}

/// How long a cached record may stand in for a directory that no longer resolves a symbol.
pub const CACHE_DAYS: i64 = 7;

/// Identities kept between runs, with the date each was last verified.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Cache {
    pub securities: BTreeMap<String, SecurityIdentity>,
}

impl Cache {
    fn path(dir: &Path) -> std::path::PathBuf {
        dir.join("security_master.json")
    }

    /// Load the cache. A missing or unreadable file is an empty cache, never an error:
    /// the directory is the source of truth and the cache only bridges an outage.
    pub fn load(dir: &Path) -> Self {
        std::fs::read_to_string(Self::path(dir))
            .ok()
            .and_then(|text| serde_json::from_str(&text).ok())
            .unwrap_or_default()
    }

    pub fn save(&self, dir: &Path) -> anyhow::Result<()> {
        std::fs::create_dir_all(dir)?;
        let target = Self::path(dir);
        let temporary = target.with_extension("tmp");
        std::fs::write(&temporary, serde_json::to_string_pretty(self)?)?;
        std::fs::rename(temporary, target)?;
        Ok(())
    }

    /// Record a verified identity, or fall back to a recent cached one when today's
    /// resolution failed. A stale or missing cache entry leaves the identity unverified.
    pub fn reconcile(&mut self, fresh: SecurityIdentity, today: &str) -> SecurityIdentity {
        if fresh.verified {
            self.securities.insert(fresh.symbol.clone(), fresh.clone());
            return fresh;
        }
        let age = |cached: &SecurityIdentity| {
            let parse = |d: &str| d.parse::<chrono::NaiveDate>().ok();
            Some((parse(today)? - parse(&cached.verified_as_of)?).num_days())
        };
        match self.securities.get(&fresh.symbol) {
            Some(cached)
                if cached.verified
                    && fresh.instrument_type == InstrumentType::Unknown
                    && fresh.canonical_name.is_empty()
                    && age(cached).is_some_and(|d| (0..=CACHE_DAYS).contains(&d)) =>
            {
                let mut reused = cached.clone();
                reused.notes.push(format!(
                    "from the cache, last verified {}; today's directory did not resolve it",
                    cached.verified_as_of
                ));
                reused
            }
            _ => fresh,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn row(symbol: &str, name: &str, exchange: &str, etf: bool) -> Value {
        json!({"symbol": symbol, "name": name, "exchange": exchange, "etf": etf})
    }

    fn from_directory(symbol: &str, name: &str, etf: bool) -> SecurityIdentity {
        resolve(
            symbol,
            Some(&row(symbol, name, "P", etf)),
            None,
            "2026-11-10",
        )
    }

    #[test]
    fn ramz_is_a_two_times_inverse_daily_fund_on_dram() {
        let id = resolve("RAMZ", None, None, "2026-11-10");
        assert_eq!(id.instrument_type, InstrumentType::LeveragedInverseEtf);
        assert_eq!(id.leverage_multiple, Some(2.0));
        assert!(id.inverse && id.daily_reset && id.verified);
        assert_eq!(id.underlying_symbol.as_deref(), Some("DRAM"));
        assert_eq!(id.signed_multiple(), Some(-2.0));
        assert!(id.describe().contains("-2x the daily return of DRAM"));
        assert_eq!(id.security_id(), "BATS:RAMZ");
    }

    #[test]
    fn the_incident_funds_are_all_leveraged_inverse() {
        for (symbol, multiple, underlying) in [
            ("SOXS", 3.0, None),
            ("SNDQ", 2.0, Some("SNDK")),
            ("NVDS", 1.5, Some("NVDA")),
            ("SSG", 2.0, None),
        ] {
            let id = resolve(symbol, None, None, "2026-11-10");
            assert_eq!(
                id.instrument_type,
                InstrumentType::LeveragedInverseEtf,
                "{symbol}"
            );
            assert_eq!(id.leverage_multiple, Some(multiple), "{symbol}");
            assert_eq!(id.underlying_symbol.as_deref(), underlying, "{symbol}");
            assert!(id.inverse && id.verified, "{symbol}");
        }
    }

    #[test]
    fn the_directory_name_alone_gives_the_same_answer_as_the_pinned_record() {
        // The pinned table must not be the only thing standing between a geared fund and
        // the screen: the registered name parses to the same classification.
        for pin in PINNED {
            let parsed = parse_name(pin.name, pin.kind.is_fund());
            assert_eq!(parsed.kind, pin.kind, "{}", pin.symbol);
            assert_eq!(parsed.multiple, pin.multiple, "{}", pin.symbol);
            assert_eq!(parsed.inverse, pin.inverse, "{}", pin.symbol);
            assert_eq!(
                parsed.underlying_symbol.as_deref(),
                pin.underlying_symbol,
                "{}",
                pin.symbol
            );
        }
    }

    #[test]
    fn ordinary_equity_controls_stay_ordinary() {
        for symbol in ["MAX", "POOL"] {
            let id = resolve(symbol, None, None, "2026-11-10");
            assert_eq!(id.instrument_type, InstrumentType::CommonStock, "{symbol}");
            assert!(id.verified && !id.inverse && id.leverage_multiple.is_none());
            assert_eq!(id.signed_multiple(), None);
        }
    }

    #[test]
    fn geared_fund_names_from_other_sponsors_parse() {
        let cases: [(&str, InstrumentType, Option<f64>, bool); 14] = [
            (
                "Tradr 2X Short AAOI Daily ETF",
                InstrumentType::LeveragedInverseEtf,
                Some(2.0),
                true,
            ),
            (
                "Direxion Daily S&P 500 Bull 2X ETF",
                InstrumentType::LeveragedEtf,
                Some(2.0),
                false,
            ),
            (
                "Direxion Daily MU Bear 1X ETF",
                InstrumentType::InverseEtf,
                Some(1.0),
                true,
            ),
            (
                "ProShares UltraPro Short S&P500",
                InstrumentType::LeveragedInverseEtf,
                Some(3.0),
                true,
            ),
            (
                "ProShares Ultra Technology",
                InstrumentType::LeveragedEtf,
                Some(2.0),
                false,
            ),
            (
                "ProShares Short S&P500",
                InstrumentType::InverseEtf,
                Some(1.0),
                true,
            ),
            (
                "Corgi AGIX 2x Daily ETF",
                InstrumentType::LeveragedEtf,
                Some(2.0),
                false,
            ),
            (
                "-1x Short VIX Futures ETF",
                InstrumentType::InverseEtf,
                Some(1.0),
                true,
            ),
            (
                "T-Rex 2X Long Alphabet Daily Target ETF",
                InstrumentType::LeveragedEtf,
                Some(2.0),
                false,
            ),
            (
                "GraniteShares 2x Long TSM Daily ETF",
                InstrumentType::LeveragedEtf,
                Some(2.0),
                false,
            ),
            (
                "Direxion Daily 20-Yr Treasury Bull 3x Shrs",
                InstrumentType::LeveragedEtf,
                Some(3.0),
                false,
            ),
            (
                "State Street SPDR S&P 500 ETF Trust",
                InstrumentType::OrdinaryEtf,
                None,
                false,
            ),
            (
                "Roundhill Memory ETF",
                InstrumentType::OrdinaryEtf,
                None,
                false,
            ),
            (
                "Militia Long/Short Equity ETF",
                InstrumentType::OrdinaryEtf,
                None,
                false,
            ),
        ];
        for (name, kind, multiple, inverse) in cases {
            let parsed = parse_name(name, true);
            assert_eq!(parsed.kind, kind, "{name}");
            assert_eq!(parsed.multiple, multiple, "{name}");
            assert_eq!(parsed.inverse, inverse, "{name}");
        }
    }

    #[test]
    fn short_as_a_bond_tenor_is_not_an_inverse_fund() {
        for name in [
            "First Trust Short Duration Managed Municipal ETF",
            "Xtrackers Short Duration High Yield Bond ETF",
            "American Century Ultrashort Income ETF",
            "Vanguard Ultra-Short Bond ETF",
            "iShares Short Treasury Bond ETF",
            "JPMorgan Ultra Short Income ETF",
        ] {
            assert_eq!(
                parse_name(name, true).kind,
                InstrumentType::OrdinaryEtf,
                "{name}"
            );
        }
    }

    #[test]
    fn notes_warrants_units_and_funds_are_not_equities() {
        assert_eq!(
            parse_name("MicroSectors -3x Short Consumer Staples (XLP) ETNs", true).kind,
            InstrumentType::Etn
        );
        assert_eq!(
            parse_name("iPath Select MLP ETN", false).kind,
            InstrumentType::Etn
        );
        for name in [
            "Acme Acquisition Corp. - Warrant",
            "Acme Acquisition Corp. - Warrants",
            "Acme Acquisition Corp. - Units",
            "Bank of Somewhere Depositary Shares each representing 1/40th of 6.5% Preferred",
            "Somewhere Capital 7.25% Notes due 2030",
            "Nuveen Municipal Value Fund, Inc.",
        ] {
            let parsed = parse_name(name, false);
            assert_eq!(parsed.kind, InstrumentType::Other, "{name}");
            assert!(parsed.conclusive);
        }
    }

    #[test]
    fn company_names_drop_the_share_class_words() {
        assert_eq!(
            company_name("MediaAlpha, Inc. Class A Common Stock"),
            "MediaAlpha, Inc."
        );
        assert_eq!(
            company_name("Pool Corporation - Common Stock"),
            "Pool Corporation"
        );
        assert_eq!(
            company_name("Ohmyhome Limited - Class A Ordinary Shares"),
            "Ohmyhome Limited"
        );
        assert_eq!(company_name("Agora, Inc. - ADS"), "Agora, Inc.");
        assert_eq!(
            parse_name("Agora, Inc. - ADS", false).kind,
            InstrumentType::Adr
        );
        assert_eq!(
            parse_name("ADS-TEC ENERGY PLC - Ordinary Shares", false).kind,
            InstrumentType::CommonStock
        );
        assert_eq!(
            parse_name("Direxion Daily AI and Big Data Bear 2X ETF", true).underlying_symbol,
            None
        );
        assert_eq!(
            parse_name("Corgi MU 2x Daily ETF", true)
                .underlying_symbol
                .as_deref(),
            Some("MU")
        );
    }

    #[test]
    fn a_plain_company_name_needs_a_second_source() {
        // "Kemper Corporation" has no share-class words. Alone it is not verified, and an
        // unverified record is unknown, never a common stock.
        let alone = from_directory("KMPR", "Kemper Corporation", false);
        assert_eq!(alone.instrument_type, InstrumentType::Unknown);
        assert!(!alone.verified);
        let corroborated = resolve(
            "KMPR",
            Some(&row("KMPR", "Kemper Corporation", "N", false)),
            Some("EQUITY"),
            "2026-11-10",
        );
        assert_eq!(corroborated.instrument_type, InstrumentType::CommonStock);
        assert!(corroborated.verified);
        assert_eq!(
            corroborated.issuer_name.as_deref(),
            Some("Kemper Corporation")
        );
    }

    #[test]
    fn disagreeing_sources_and_missing_records_are_unknown() {
        let clash = resolve(
            "XYZ",
            Some(&row("XYZ", "Xyz Inc. - Common Stock", "Q", false)),
            Some("ETF"),
            "2026-11-10",
        );
        assert_eq!(clash.instrument_type, InstrumentType::Unknown);
        assert!(!clash.verified);
        assert!(clash.notes[0].contains("sources disagree"));
        let missing = resolve("NOPE", None, Some("EQUITY"), "2026-11-10");
        assert_eq!(missing.instrument_type, InstrumentType::Unknown);
        assert!(!missing.verified);
        assert!(missing.describe().contains("NOT verified"));
        let unnamed = resolve("BARE", Some(&json!({"symbol": "BARE"})), None, "2026-11-10");
        assert!(!unnamed.verified);
    }

    #[test]
    fn a_reassigned_ticker_voids_its_pinned_record() {
        let id = resolve(
            "RAMZ",
            Some(&row("RAMZ", "Ramz Biotech Inc. - Common Stock", "Q", false)),
            None,
            "2027-03-01",
        );
        assert!(!id.verified);
        assert_eq!(id.instrument_type, InstrumentType::Unknown);
    }

    #[test]
    fn the_cache_bridges_a_short_outage_and_then_expires() {
        let mut cache = Cache::default();
        let good = from_directory("ABC", "Abc Corp. - Common Stock", false);
        assert!(cache.reconcile(good.clone(), "2026-11-10").verified);
        let missing = SecurityIdentity::unknown("ABC", "symbol is not in the exchange directory");
        let bridged = cache.reconcile(missing.clone(), "2026-11-13");
        assert!(bridged.verified);
        assert!(bridged.notes.iter().any(|n| n.contains("from the cache")));
        let expired = cache.reconcile(missing, "2026-11-30");
        assert!(!expired.verified);
        // A record the directory resolved to something else is never papered over.
        let now_fund = from_directory("ABC", "Abc 2X Short XYZ Daily ETF", true);
        assert_eq!(
            cache.reconcile(now_fund, "2026-11-11").instrument_type,
            InstrumentType::LeveragedInverseEtf
        );
    }

    #[test]
    fn the_cache_round_trips_through_its_file() {
        let dir = std::env::temp_dir().join(format!("pt-secmaster-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let mut cache = Cache::default();
        cache.reconcile(resolve("RAMZ", None, None, "2026-11-10"), "2026-11-10");
        cache.save(&dir).unwrap();
        let loaded = Cache::load(&dir);
        assert_eq!(
            loaded.securities["RAMZ"].instrument_type,
            InstrumentType::LeveragedInverseEtf
        );
        assert_eq!(loaded.securities["RAMZ"].verified_as_of, "2026-11-10");
        assert_eq!(Cache::load(&dir.join("absent")).securities.len(), 0);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
