//! Hard, deterministic gates that no model and no strategy can loosen.
//!
//! The desk's models only choose among stocks that already passed these rules, and they
//! never size a position. A strategy from the lab can make a gate stricter (a higher price
//! floor, say) but never looser: the account always applies the stricter of the two.
//!
//! | Gate | Rule |
//! |---|---|
//! | Instrument | only verified common stocks and ADRs, plus unleveraged ETFs when the strategy's universe says so; never a leveraged or inverse product, a note, or an unidentified symbol |
//! | Size | market capitalisation of at least [`MIN_MARKET_CAP`]; unknown is refused |
//! | Data | the latest session's bar is present, the last 21 sessions are complete, and no split-sized jump is unexplained |
//! | Price | close at least [`MIN_PRICE`] |
//! | Liquidity | average daily traded value over [`ADTV_DAYS`] sessions at least [`MIN_ADTV`] |
//! | Events | no earnings report, FDA decision or restructuring within [`EVENT_DAYS`] days |
//! | Volatility sizing | a slot shrinks by [`VOL_REFERENCE`] / σ when σ is higher |
//! | Industry | at most [`INDUSTRY_CAP`] of equity in one SEC industry |
//! | Drawdown | down [`DRAWDOWN_HALT`] from the high over [`DRAWDOWN_SESSIONS`] sessions: a [`CASH_LOCK_HOURS`]-hour cash lock |

use crate::model::{Action, Bar, Intel};
use crate::security::{InstrumentType, SecurityIdentity};
use crate::strategy::Universe;
use chrono::NaiveDate;
use serde::Serialize;

/// Reason codes written to the ledger with every hard-gate rejection.
pub mod code {
    pub const LEVERAGED_ETF: &str = "LEVERAGED_ETF";
    pub const INVERSE_ETF: &str = "INVERSE_ETF";
    pub const LEVERAGED_INVERSE_ETF: &str = "LEVERAGED_INVERSE_ETF";
    pub const UNSUPPORTED_INSTRUMENT: &str = "UNSUPPORTED_INSTRUMENT";
    pub const UNVERIFIED_SECURITY_IDENTITY: &str = "UNVERIFIED_SECURITY_IDENTITY";
    pub const ETF_OUTSIDE_UNIVERSE: &str = "ETF_OUTSIDE_UNIVERSE";
    pub const STALE_DATA: &str = "STALE_DATA";
    pub const MISSING_BARS: &str = "MISSING_BARS";
    pub const CORPORATE_ACTION_CHECK: &str = "CORPORATE_ACTION_CHECK";
    pub const SMALL_CAP: &str = "SMALL_CAP";
    pub const MARKET_CAP_UNKNOWN: &str = "MARKET_CAP_UNKNOWN";
    pub const PRICE_FLOOR: &str = "PRICE_FLOOR";
    pub const LIQUIDITY_FLOOR: &str = "LIQUIDITY_FLOOR";
    pub const EVENT_WINDOW: &str = "EVENT_WINDOW";
}

/// A hard gate's refusal: a stable code and a sentence a person can read.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Block {
    pub code: &'static str,
    pub reason: String,
}

impl Block {
    pub fn new(code: &'static str, reason: impl Into<String>) -> Self {
        Block {
            code,
            reason: reason.into(),
        }
    }

    /// The form written to the ledger: `[CODE] reason`.
    pub fn text(&self) -> String {
        format!("[{}] {}", self.code, self.reason)
    }
}

/// Smallest company the account buys: $2 billion of market capitalisation, the usual line
/// between small caps and mid caps. Small and micro caps are where the first week's worst
/// candidates came from: a $32M company up 432% in a day, a $390M one in a share
/// consolidation.
pub const MIN_MARKET_CAP: f64 = 2_000_000_000.0;

/// Why a company is too small to buy, or of unknown size. An unknown size is refused: it
/// is not assumed to be large. Funds have no market capitalisation and are not judged here.
pub fn size(identity: &SecurityIdentity, intel: &Intel) -> Option<Block> {
    if identity.instrument_type.is_fund() {
        return None;
    }
    match intel.market_caps.get(&identity.symbol) {
        Some(cap) if *cap >= MIN_MARKET_CAP => None,
        Some(cap) => Some(Block::new(
            code::SMALL_CAP,
            format!(
                "Market capitalisation ${:.2}B is below the ${:.0}B floor (small cap)",
                cap / 1e9,
                MIN_MARKET_CAP / 1e9
            ),
        )),
        None => Some(Block::new(
            code::MARKET_CAP_UNKNOWN,
            format!(
                "No market capitalisation is known for {}; it is not assumed to be large",
                identity.symbol
            ),
        )),
    }
}

/// Sessions that must be present, without a gap, before a stock may be bought. The screen
/// measures 20-day momentum as the last close over the close 20 sessions earlier, which is
/// only a 20-day return when all 21 bars are there.
pub const COMPLETE_SESSIONS: usize = 21;
/// Share-count ratios a split usually takes. A one-day price move within
/// [`SPLIT_TOLERANCE`] of one of these (or its inverse) with no reported split is treated
/// as a possible unadjusted corporate action, not as a return.
pub const SPLIT_RATIOS: [f64; 13] = [
    2.0, 3.0, 4.0, 5.0, 8.0, 10.0, 15.0, 20.0, 25.0, 30.0, 40.0, 50.0, 100.0,
];
pub const SPLIT_TOLERANCE: f64 = 0.015;

/// Why this instrument may not be bought under `universe`, if it may not.
///
/// Leveraged products, inverse products, notes, unsupported classes and unidentified
/// symbols are refused under every universe: there is no setting that lets them in. An
/// unleveraged ETF is refused unless the strategy's universe explicitly includes ETFs.
/// The decision reads the security master and never asks a model.
pub fn instrument(identity: &SecurityIdentity, universe: Universe) -> Option<Block> {
    let name = if identity.canonical_name.is_empty() {
        identity.symbol.as_str()
    } else {
        identity.canonical_name.as_str()
    };
    let exposure = identity.signed_multiple().map_or(String::new(), |m| {
        format!(
            ", which seeks {m:+}x the {} return of {}",
            if identity.daily_reset {
                "daily"
            } else {
                "stated"
            },
            identity
                .underlying_symbol
                .as_deref()
                .or(identity.underlying_name.as_deref())
                .unwrap_or("its reference")
        )
    });
    let block = |code, what: &str| Some(Block::new(code, format!("{name} is {what}{exposure}")));
    if !identity.verified {
        return Some(Block::new(
            code::UNVERIFIED_SECURITY_IDENTITY,
            format!(
                "The identity of {} is not verified ({}); it is not assumed to be a stock",
                identity.symbol,
                identity
                    .notes
                    .first()
                    .map_or("no source resolved it", String::as_str)
            ),
        ));
    }
    match identity.instrument_type {
        InstrumentType::CommonStock | InstrumentType::Adr => None,
        InstrumentType::OrdinaryEtf if universe == Universe::EquitiesAndEtfs => None,
        InstrumentType::OrdinaryEtf => Some(Block::new(
            code::ETF_OUTSIDE_UNIVERSE,
            format!("{name} is an ETF and this strategy's universe is equities only"),
        )),
        InstrumentType::LeveragedEtf => block(code::LEVERAGED_ETF, "a leveraged ETF"),
        InstrumentType::InverseEtf => block(code::INVERSE_ETF, "an inverse ETF"),
        InstrumentType::LeveragedInverseEtf => {
            block(code::LEVERAGED_INVERSE_ETF, "a leveraged inverse ETF")
        }
        InstrumentType::Etn => Some(Block::new(
            code::UNSUPPORTED_INSTRUMENT,
            format!("{name} is an exchange-traded note, which the account does not trade"),
        )),
        InstrumentType::Other => Some(Block::new(
            code::UNSUPPORTED_INSTRUMENT,
            format!(
                "{name} is not a common stock, ADR or supported fund{}",
                identity
                    .notes
                    .first()
                    .map_or(String::new(), |n| format!(" ({n})"))
            ),
        )),
        InstrumentType::Unknown => Some(Block::new(
            code::UNVERIFIED_SECURITY_IDENTITY,
            format!("{} could not be classified", identity.symbol),
        )),
    }
}

fn session(at: f64) -> NaiveDate {
    chrono::DateTime::from_timestamp(at as i64, 0)
        .unwrap_or_default()
        .date_naive()
}

/// The split ratio a one-day price ratio looks like, if it looks like one.
fn split_like(ratio: f64) -> Option<f64> {
    SPLIT_RATIOS.iter().copied().find(|k| {
        (ratio / k - 1.0).abs() <= SPLIT_TOLERANCE || (ratio * k - 1.0).abs() <= SPLIT_TOLERANCE
    })
}

/// Why a stock's price history is not fit to buy on, if it is not.
///
/// `reference` is the benchmark's bars (SPY), which define which sessions exist. The checks:
/// the stock has a bar for the benchmark's latest session; it has a bar for each of the last
/// [`COMPLETE_SESSIONS`] sessions; a reported split is not still visible as a price step
/// (which would mean the bars are not adjusted); and no unexplained one-day move in that
/// window is the size of a split. A large move that is not split-sized is a real return and
/// passes: the gate does not discard extreme days.
pub fn data_quality(bars: &[Bar], actions: &[Action], reference: &[Bar]) -> Option<Block> {
    let last = bars.last()?;
    if let Some(latest) = reference.last() {
        if session(last.at) < session(latest.at) {
            return Some(Block::new(
                code::STALE_DATA,
                format!(
                    "Latest bar is from {}, but the latest completed session is {}",
                    session(last.at),
                    session(latest.at)
                ),
            ));
        }
        let have: std::collections::BTreeSet<NaiveDate> =
            bars.iter().map(|b| session(b.at)).collect();
        let window = &reference[reference.len().saturating_sub(COMPLETE_SESSIONS)..];
        let missing: Vec<String> = window
            .iter()
            .map(|b| session(b.at))
            .filter(|d| !have.contains(d))
            .map(|d| d.to_string())
            .collect();
        if !missing.is_empty() {
            return Some(Block::new(
                code::MISSING_BARS,
                format!(
                    "No bar for {} of the last {COMPLETE_SESSIONS} sessions ({}), so 20-day figures are not comparable",
                    missing.len(),
                    missing
                        .iter()
                        .take(3)
                        .cloned()
                        .collect::<Vec<_>>()
                        .join(", ")
                ),
            ));
        }
    }
    let splits: Vec<(NaiveDate, f64)> = actions
        .iter()
        .filter(|a| a.kind == "splits")
        .filter_map(|a| {
            let ratio = a.numerator? / a.denominator?;
            (ratio.is_finite() && ratio > 0.0).then(|| (session(a.date), ratio))
        })
        .collect();
    let recent = &bars[bars.len().saturating_sub(COMPLETE_SESSIONS + 1)..];
    for pair in bars.windows(2) {
        let day = session(pair[1].at);
        let step = pair[1].open / pair[0].close;
        // A reported split: adjusted bars are continuous across it. A step the size of
        // the split means the provider's history was not adjusted.
        if let Some((_, ratio)) = splits.iter().find(|(d, _)| *d == day) {
            let expected = 1.0 / ratio;
            if (ratio - 1.0).abs() > 1e-9 && (step / expected - 1.0).abs() <= 0.15 {
                return Some(Block::new(
                    code::CORPORATE_ACTION_CHECK,
                    format!(
                        "Bars are not adjusted for the {ratio}-for-1 split on {day}: the open is {step:.2}x the prior close"
                    ),
                ));
            }
        }
    }
    for pair in recent.windows(2) {
        let day = session(pair[1].at);
        let move_ = pair[1].close / pair[0].close;
        let near_split = splits.iter().any(|(d, _)| (*d - day).num_days().abs() <= 3);
        if let Some(k) = split_like(move_).filter(|_| !near_split) {
            return Some(Block::new(
                code::CORPORATE_ACTION_CHECK,
                format!(
                    "The close on {day} is {move_:.3}x the prior close, the size of a {k}-for-1 split, and no split is reported"
                ),
            ));
        }
    }
    None
}

/// Lowest close a stock may have when it is bought.
pub const MIN_PRICE: f64 = 5.0;
/// Lowest average daily traded value (close × volume), in dollars.
pub const MIN_ADTV: f64 = 5_000_000.0;
/// Sessions the traded value is averaged over.
pub const ADTV_DAYS: usize = 30;
/// Calendar days on either side of today that an event blocks a buy.
pub const EVENT_DAYS: i64 = 7;
/// Sessions of close-to-close returns behind the volatility estimate.
pub const VOL_DAYS: usize = 14;
/// Daily volatility at and below which a position gets its full slot. Above it the slot is
/// scaled by `VOL_REFERENCE / σ`, so every such position carries the same daily dollar risk.
pub const VOL_REFERENCE: f64 = 0.08;
/// Most of equity that may sit in one SEC industry.
pub const INDUSTRY_CAP: f64 = 0.30;
/// Fall from the recent high that trips the cash lock.
pub const DRAWDOWN_HALT: f64 = 0.10;
/// Sessions the recent high is taken over.
pub const DRAWDOWN_SESSIONS: usize = 5;
/// How long the cash lock lasts once tripped.
pub const CASH_LOCK_HOURS: f64 = 48.0;

/// 8-K items that mark a restructuring: bankruptcy (1.03), exit or disposal costs (2.05),
/// a delisting notice (3.01), a change to shareholders' rights (3.03) and an amendment to
/// the articles (5.03, which is how most reverse splits are filed).
pub const RESTRUCTURING_ITEMS: [&str; 5] = ["1.03", "2.05", "3.01", "3.03", "5.03"];
/// Forms that mark a merger, tender offer, going-private deal or delisting.
pub const RESTRUCTURING_FORMS: [&str; 9] = [
    "S-4", "DEFM14A", "PREM14A", "425", "SC TO-T", "SC 13E3", "15-12B", "15-12G", "25-NSE",
];
/// Headline phrases that mark an FDA decision.
pub const FDA_WORDS: [&str; 4] = [
    "fda",
    "pdufa",
    "advisory committee",
    "complete response letter",
];
/// Headline phrases that mark a restructuring.
pub const RESTRUCTURING_WORDS: [&str; 11] = [
    "reverse split",
    "reverse stock split",
    "share consolidation",
    "bankruptcy",
    "chapter 11",
    "restructuring",
    "delisting",
    "delisted",
    "going private",
    "merger",
    "to be acquired",
];

/// Average daily traded value (close × volume) over the last `days` bars.
pub fn adtv(bars: &[Bar], days: usize) -> f64 {
    let recent = &bars[bars.len().saturating_sub(days)..];
    if recent.is_empty() {
        return 0.0;
    }
    recent.iter().map(|b| b.close * b.volume).sum::<f64>() / recent.len() as f64
}

/// Standard deviation of the last `days` close-to-close returns. None without enough bars.
pub fn daily_volatility(bars: &[Bar], days: usize) -> Option<f64> {
    if days == 0 || bars.len() < days + 1 {
        return None;
    }
    let returns: Vec<f64> = bars[bars.len() - days - 1..]
        .windows(2)
        .map(|w| w[1].close / w[0].close - 1.0)
        .collect();
    let mean = returns.iter().sum::<f64>() / returns.len() as f64;
    let variance = returns.iter().map(|r| (r - mean).powi(2)).sum::<f64>() / returns.len() as f64;
    Some(variance.sqrt())
}

/// Share of a full slot a stock with daily volatility `sigma` may use.
pub fn volatility_scale(sigma: Option<f64>) -> f64 {
    match sigma {
        Some(s) if s.is_finite() && s > VOL_REFERENCE => VOL_REFERENCE / s,
        _ => 1.0,
    }
}

/// Why a stock fails the price or liquidity floor, if it does. `min_price` and `min_adtv`
/// are a strategy's own floors; the hard ones apply when they are lower.
pub fn liquidity(close: f64, adtv: f64, min_price: f64, min_adtv: f64) -> Option<Block> {
    let (price_floor, adtv_floor) = (min_price.max(MIN_PRICE), min_adtv.max(MIN_ADTV));
    if close < price_floor {
        Some(Block::new(
            code::PRICE_FLOOR,
            format!("Price ${close:.2} is below the ${price_floor:.2} floor"),
        ))
    } else if adtv < adtv_floor {
        Some(Block::new(
            code::LIQUIDITY_FLOOR,
            format!(
                "Average daily traded value ${:.1}M over {ADTV_DAYS} sessions is below ${:.1}M",
                adtv / 1e6,
                adtv_floor / 1e6
            ),
        ))
    } else {
        None
    }
}

/// Lowercase words separated by single spaces, padded, for whole-phrase matching.
fn words(text: &str) -> String {
    let cleaned: String = text
        .chars()
        .map(|c| {
            if c.is_alphanumeric() {
                c.to_ascii_lowercase()
            } else {
                ' '
            }
        })
        .collect();
    format!(
        " {} ",
        cleaned.split_whitespace().collect::<Vec<_>>().join(" ")
    )
}

fn within(date: &str, today: NaiveDate) -> bool {
    date.get(..10)
        .and_then(|d| d.parse::<NaiveDate>().ok())
        .is_some_and(|d| (d - today).num_days().abs() <= EVENT_DAYS)
}

/// A headline's date is within the event window. Undated headlines count, since the feed
/// lists current items and the gate errs toward blocking.
fn recent_headline(published: &str, today: NaiveDate) -> bool {
    match chrono::DateTime::parse_from_rfc2822(published.trim()) {
        Ok(t) => (t.date_naive() - today).num_days().abs() <= EVENT_DAYS,
        Err(_) => true,
    }
}

/// Why a stock is blocked by an event within [`EVENT_DAYS`] days, if it is.
///
/// Headlines come from a feed keyed by ticker text. One that names a different listing of
/// the same ticker (an Australian retailer for a US fund) says nothing about this security
/// and is skipped; an ambiguous one still counts, because the gate errs toward blocking.
pub fn event(identity: &SecurityIdentity, intel: &Intel, today: NaiveDate) -> Option<Block> {
    event_reason(identity, intel, today).map(|reason| Block::new(code::EVENT_WINDOW, reason))
}

fn event_reason(identity: &SecurityIdentity, intel: &Intel, today: NaiveDate) -> Option<String> {
    let symbol = identity.symbol.as_str();
    for (map, what) in [(&intel.earnings, "upcoming"), (&intel.reported, "recent")] {
        if let Some(date) = map.get(symbol).filter(|d| within(d, today)) {
            return Some(format!(
                "Earnings report on {date} ({what}) is within {EVENT_DAYS} days"
            ));
        }
    }
    for filing in intel.filings.get(symbol).into_iter().flatten() {
        if !within(&filing.date, today) {
            continue;
        }
        if RESTRUCTURING_FORMS.contains(&filing.form.as_str()) {
            return Some(format!(
                "SEC {} filed {} marks a restructuring",
                filing.form, filing.date
            ));
        }
        if filing.form == "8-K"
            && let Some(item) = filing
                .items
                .split(',')
                .map(str::trim)
                .find(|i| RESTRUCTURING_ITEMS.contains(i))
        {
            return Some(format!(
                "8-K item {item} filed {} marks a restructuring",
                filing.date
            ));
        }
    }
    for headline in intel.news.get(symbol).into_iter().flatten() {
        if !recent_headline(&headline.published, today) {
            continue;
        }
        let matched = crate::evidence::news_record(identity, headline, today);
        if matched.status == crate::evidence::MatchStatus::Rejected {
            continue;
        }
        let text = words(&headline.title);
        for (list, kind) in [
            (&FDA_WORDS[..], "an FDA decision"),
            (&RESTRUCTURING_WORDS[..], "a restructuring"),
        ] {
            if let Some(word) = list.iter().find(|w| text.contains(&words(w))) {
                return Some(format!(
                    "Headline \"{}\" mentions \"{word}\", {kind}",
                    headline.title.chars().take(120).collect::<String>()
                ));
            }
        }
    }
    None
}

/// The largest share of a full slot a new buy in `industry` may use, given `exposure`
/// dollars already held or queued there, at `equity` and a slot of `slot` dollars.
pub fn industry_room(exposure: f64, equity: f64, slot: f64) -> f64 {
    if slot <= 0.0 {
        return 0.0;
    }
    ((INDUSTRY_CAP * equity - exposure) / slot).clamp(0.0, 1.0)
}

/// The fall from the recent high when it is at least [`DRAWDOWN_HALT`], else None.
pub fn drawdown_breach(recent_high: f64, equity: f64) -> Option<f64> {
    let high = recent_high.max(equity);
    let fall = if high > 0.0 { 1.0 - equity / high } else { 0.0 };
    (fall >= DRAWDOWN_HALT - 1e-12).then_some(fall)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Filing, Headline};

    fn why(symbol: &str, intel: &Intel, today: NaiveDate) -> Option<String> {
        let identity = SecurityIdentity::unknown(symbol, "test");
        event(&identity, intel, today).map(|b| {
            assert_eq!(b.code, code::EVENT_WINDOW);
            b.reason
        })
    }

    fn bar(close: f64, volume: f64) -> Bar {
        Bar {
            at: 0.0,
            open: close,
            close,
            volume,
        }
    }

    fn day(s: &str) -> NaiveDate {
        s.parse().unwrap()
    }

    #[test]
    fn adtv_averages_the_last_thirty_sessions() {
        let mut bars: Vec<Bar> = (0..40).map(|_| bar(10.0, 1_000.0)).collect();
        for b in bars.iter_mut().skip(10) {
            b.volume = 600_000.0;
        }
        assert!((adtv(&bars, 30) - 6_000_000.0).abs() < 1e-6);
        assert!((adtv(&bars[..5], 30) - 10_000.0).abs() < 1e-6);
        assert_eq!(adtv(&[], 30), 0.0);
    }

    #[test]
    fn price_and_liquidity_floors_cannot_be_loosened() {
        let low = liquidity(4.99, 1e9, 1.0, 0.0).unwrap();
        assert!(low.reason.contains("$5.00"));
        assert_eq!(low.code, code::PRICE_FLOOR);
        let thin = liquidity(20.0, 4.9e6, 1.0, 1e6).unwrap();
        assert!(thin.reason.contains("$5.0M"));
        assert_eq!(thin.code, code::LIQUIDITY_FLOOR);
        assert!(liquidity(20.0, 5e6, 1.0, 1e6).is_none());
        // A stricter strategy floor wins.
        assert!(
            liquidity(8.0, 9e6, 10.0, 1e6)
                .unwrap()
                .reason
                .contains("$10.00")
        );
    }

    #[test]
    fn volatility_scales_only_above_eight_percent() {
        // Alternating +10% / -10% days: sigma of 10%.
        let mut bars = vec![bar(100.0, 1.0)];
        for i in 0..14 {
            let last = bars.last().unwrap().close;
            bars.push(bar(if i % 2 == 0 { last * 1.1 } else { last * 0.9 }, 1.0));
        }
        let sigma = daily_volatility(&bars, VOL_DAYS).unwrap();
        assert!((sigma - 0.1).abs() < 1e-9, "{sigma}");
        assert!((volatility_scale(Some(sigma)) - 0.8).abs() < 1e-9);
        assert_eq!(volatility_scale(Some(0.03)), 1.0);
        assert_eq!(volatility_scale(None), 1.0);
        assert_eq!(daily_volatility(&bars[..10], VOL_DAYS), None);
    }

    #[test]
    fn earnings_within_a_week_either_side_block_a_buy() {
        let today = day("2026-11-10");
        let mut intel = Intel::default();
        intel.earnings.insert("UP".into(), "2026-11-17".into());
        intel.earnings.insert("FAR".into(), "2026-11-18".into());
        intel.reported.insert("PAST".into(), "2026-11-03".into());
        assert!(why("UP", &intel, today).unwrap().contains("2026-11-17"));
        assert!(why("PAST", &intel, today).unwrap().contains("recent"));
        assert!(why("FAR", &intel, today).is_none());
    }

    #[test]
    fn restructuring_filings_block_a_buy_inside_the_window() {
        let today = day("2026-11-10");
        let filing = |form: &str, items: &str, date: &str| Filing {
            form: form.into(),
            date: date.into(),
            items: items.into(),
            description: String::new(),
            accession: String::new(),
            document: String::new(),
            ..Filing::default()
        };
        let mut intel = Intel::default();
        intel
            .filings
            .insert("RS".into(), vec![filing("8-K", "5.03,9.01", "2026-11-06")]);
        intel
            .filings
            .insert("MERGE".into(), vec![filing("S-4", "", "2026-11-09")]);
        intel
            .filings
            .insert("OLD".into(), vec![filing("8-K", "1.03", "2026-10-01")]);
        intel.filings.insert(
            "EARN".into(),
            vec![filing("8-K", "2.02,9.01", "2026-11-09")],
        );
        assert!(why("RS", &intel, today).unwrap().contains("item 5.03"));
        assert!(why("MERGE", &intel, today).unwrap().contains("S-4"));
        assert!(why("OLD", &intel, today).is_none());
        assert!(why("EARN", &intel, today).is_none());
    }

    #[test]
    fn fda_and_restructuring_headlines_block_a_buy() {
        let today = day("2026-11-10");
        let news = |title: &str, published: &str| Headline {
            title: title.into(),
            published: published.into(),
            ..Headline::default()
        };
        let mut intel = Intel::default();
        intel.news.insert(
            "GIBO".into(),
            vec![news(
                "GIBO Holdings Limited Announces Ratio and Effective Date of Share Consolidation",
                "Fri, 06 Nov 2026 13:00:00 +0000",
            )],
        );
        intel.news.insert(
            "BIO".into(),
            vec![news("BioCo sets PDUFA date for lead drug", "")],
        );
        intel.news.insert(
            "STALE".into(),
            vec![news("Old merger news", "Mon, 05 Oct 2026 13:00:00 +0000")],
        );
        intel.news.insert(
            "WORD".into(),
            vec![news("Fdaxx Corp ships new laptops", "")],
        );
        assert!(
            why("GIBO", &intel, today)
                .unwrap()
                .contains("share consolidation")
        );
        assert!(why("BIO", &intel, today).unwrap().contains("FDA decision"));
        assert!(why("STALE", &intel, today).is_none());
        assert!(why("WORD", &intel, today).is_none(), "whole words only");
        // A namesake's event on another exchange does not block the US-listed security.
        intel.news.insert(
            "SSG".into(),
            vec![news(
                "Shaver Shop Group Ltd (ASX:SSG) agrees merger with rival",
                "",
            )],
        );
        let ssg = crate::security::resolve("SSG", None, None, "2026-11-10");
        assert!(event(&ssg, &intel, today).is_none());
        intel.news.insert(
            "SSG".into(),
            vec![news(
                "ProShares UltraShort Semiconductors to be delisted",
                "",
            )],
        );
        assert!(event(&ssg, &intel, today).is_some());
    }

    #[test]
    fn industry_room_keeps_one_industry_under_thirty_percent() {
        // $1,000 equity, $200 slots: one full position leaves room for half a slot.
        assert!((industry_room(0.0, 1000.0, 200.0) - 1.0).abs() < 1e-9);
        assert!((industry_room(200.0, 1000.0, 200.0) - 0.5).abs() < 1e-9);
        assert_eq!(industry_room(300.0, 1000.0, 200.0), 0.0);
        assert_eq!(industry_room(0.0, 1000.0, 0.0), 0.0);
    }

    #[test]
    fn a_ten_percent_fall_from_the_recent_high_trips_the_lock() {
        assert!(drawdown_breach(1000.0, 900.0).is_some());
        assert!(drawdown_breach(1000.0, 901.0).is_none());
        assert!(drawdown_breach(800.0, 900.0).is_none());
        assert!((drawdown_breach(1100.0, 950.0).unwrap() - (1.0 - 950.0 / 1100.0)).abs() < 1e-12);
    }

    fn identity(symbol: &str) -> SecurityIdentity {
        crate::security::resolve(symbol, None, None, "2026-11-10")
    }

    #[test]
    fn ramz_is_rejected_by_the_default_equity_gate() {
        let block = instrument(&identity("RAMZ"), Universe::Equities).unwrap();
        assert_eq!(block.code, code::LEVERAGED_INVERSE_ETF);
        assert!(
            block.reason.contains("-2x the daily return of DRAM"),
            "{}",
            block.reason
        );
        assert!(block.text().starts_with("[LEVERAGED_INVERSE_ETF] "));
        // No universe setting lets a geared product in.
        assert_eq!(
            instrument(&identity("RAMZ"), Universe::EquitiesAndEtfs)
                .unwrap()
                .code,
            code::LEVERAGED_INVERSE_ETF
        );
    }

    #[test]
    fn every_geared_fund_in_the_incident_is_rejected_and_the_stocks_pass() {
        for symbol in ["SOXS", "SNDQ", "NVDS", "SSG"] {
            for universe in [Universe::Equities, Universe::EquitiesAndEtfs] {
                assert_eq!(
                    instrument(&identity(symbol), universe).unwrap().code,
                    code::LEVERAGED_INVERSE_ETF,
                    "{symbol}"
                );
            }
        }
        for symbol in ["MAX", "POOL"] {
            assert_eq!(instrument(&identity(symbol), Universe::Equities), None);
        }
    }

    #[test]
    fn each_instrument_class_has_its_own_reason_code() {
        let listed = |name: &str, etf: bool| {
            crate::security::resolve(
                "X",
                Some(
                    &serde_json::json!({"symbol": "X", "name": name, "exchange": "P", "etf": etf}),
                ),
                None,
                "2026-11-10",
            )
        };
        let code_of = |name: &str, etf: bool, universe| {
            instrument(&listed(name, etf), universe).map(|b| b.code)
        };
        let eq = Universe::Equities;
        assert_eq!(code_of("Acme Corp. - Common Stock", false, eq), None);
        assert_eq!(
            code_of("Acme S.A. American Depositary Shares", false, eq),
            None
        );
        assert_eq!(
            code_of("Direxion Daily S&P 500 Bull 2X ETF", true, eq),
            Some(code::LEVERAGED_ETF)
        );
        assert_eq!(
            code_of("ProShares Short S&P500", true, eq),
            Some(code::INVERSE_ETF)
        );
        assert_eq!(
            code_of("iPath Select MLP ETN", true, eq),
            Some(code::UNSUPPORTED_INSTRUMENT)
        );
        assert_eq!(
            code_of("Acme Corp. - Warrant", false, eq),
            Some(code::UNSUPPORTED_INSTRUMENT)
        );
        assert_eq!(
            code_of("Acme Corporation", false, eq),
            Some(code::UNVERIFIED_SECURITY_IDENTITY)
        );
        assert_eq!(
            instrument(&identity("NOT_LISTED"), eq).unwrap().code,
            code::UNVERIFIED_SECURITY_IDENTITY
        );
        // An unleveraged ETF is a universe decision, stated in the strategy.
        let spy = "State Street SPDR S&P 500 ETF Trust";
        assert_eq!(code_of(spy, true, eq), Some(code::ETF_OUTSIDE_UNIVERSE));
        assert_eq!(code_of(spy, true, Universe::EquitiesAndEtfs), None);
    }

    #[test]
    fn small_caps_and_companies_of_unknown_size_are_refused() {
        let mut intel = Intel::default();
        // Real values from 10 November 2026.
        for (symbol, cap) in [("POOL", 7.77e9), ("MAX", 0.85e9), ("OMH", 0.032e9)] {
            intel.market_caps.insert(symbol.into(), cap);
        }
        assert_eq!(size(&identity("POOL"), &intel), None);
        let small = size(&identity("MAX"), &intel).unwrap();
        assert_eq!(small.code, code::SMALL_CAP);
        assert!(small.reason.contains("$0.85B"), "{}", small.reason);
        let omh = SecurityIdentity {
            symbol: "OMH".into(),
            ..identity("POOL")
        };
        assert_eq!(size(&omh, &intel).unwrap().code, code::SMALL_CAP);
        let unknown = SecurityIdentity {
            symbol: "NEWCO".into(),
            ..identity("POOL")
        };
        assert_eq!(
            size(&unknown, &intel).unwrap().code,
            code::MARKET_CAP_UNKNOWN
        );
        // A fund has no market capitalisation; the instrument gate decides about funds.
        assert_eq!(size(&identity("SSG"), &Intel::default()), None);
    }

    fn daily(closes: &[f64]) -> Vec<Bar> {
        closes
            .iter()
            .enumerate()
            .map(|(i, c)| Bar {
                at: 1_700_000_000.0 + 86400.0 * i as f64,
                open: *c,
                close: *c,
                volume: 1e6,
            })
            .collect()
    }

    #[test]
    fn stale_market_data_blocks_new_purchases() {
        let reference = daily(&[100.0; 30]);
        let mut stock = daily(&[20.0; 30]);
        assert_eq!(data_quality(&stock, &[], &reference), None);
        stock.pop();
        let block = data_quality(&stock, &[], &reference).unwrap();
        assert_eq!(block.code, code::STALE_DATA);
    }

    #[test]
    fn a_gap_in_the_last_twenty_one_sessions_blocks_a_buy() {
        let reference = daily(&[100.0; 30]);
        let mut stock = daily(&[20.0; 30]);
        stock.remove(20);
        let block = data_quality(&stock, &[], &reference).unwrap();
        assert_eq!(block.code, code::MISSING_BARS);
        // A gap before the window does not matter.
        let mut older = daily(&[20.0; 30]);
        older.remove(2);
        assert_eq!(data_quality(&older, &[], &reference), None);
    }

    #[test]
    fn split_sized_steps_need_a_corporate_action_check_but_real_moves_pass() {
        let reference = daily(&[100.0; 30]);
        let split = |index: usize, numerator: f64, denominator: f64| Action {
            kind: "splits".into(),
            date: 1_700_000_000.0 + 86400.0 * index as f64,
            amount: None,
            numerator: Some(numerator),
            denominator: Some(denominator),
        };
        // An unreported 1-for-50 reverse split: the price is suddenly 50 times higher.
        let mut closes = vec![0.07; 30];
        for c in closes.iter_mut().skip(25) {
            *c = 3.5;
        }
        let unreported = data_quality(&daily(&closes), &[], &reference).unwrap();
        assert_eq!(unreported.code, code::CORPORATE_ACTION_CHECK);
        assert!(unreported.reason.contains("50-for-1"));
        // The same step with the split reported means the bars were never adjusted.
        let unadjusted =
            data_quality(&daily(&closes), &[split(25, 1.0, 50.0)], &reference).unwrap();
        assert_eq!(unadjusted.code, code::CORPORATE_ACTION_CHECK);
        assert!(unadjusted.reason.contains("not adjusted"));
        // Properly adjusted bars are continuous across a reported split: nothing to block.
        assert_eq!(
            data_quality(&daily(&[3.5; 30]), &[split(25, 1.0, 50.0)], &reference),
            None
        );
        // OMH on 4 November 2026: +432% on real volume. Not split-sized, so not discarded.
        let mut omh = vec![2.92; 30];
        for c in omh.iter_mut().skip(26) {
            *c = 15.55;
        }
        assert_eq!(data_quality(&daily(&omh), &[], &reference), None);
    }
}
