//! Hard, deterministic gates that no model and no strategy can loosen.
//!
//! The desk's models only choose among stocks that already passed these rules, and they
//! never size a position. A strategy from the lab can make a gate stricter (a higher price
//! floor, say) but never looser: the account always applies the stricter of the two.
//!
//! | Gate | Rule |
//! |---|---|
//! | Price | close at least [`MIN_PRICE`] |
//! | Liquidity | average daily traded value over [`ADTV_DAYS`] sessions at least [`MIN_ADTV`] |
//! | Events | no earnings report, FDA decision or restructuring within [`EVENT_DAYS`] days |
//! | Volatility sizing | a slot shrinks by [`VOL_REFERENCE`] / σ when σ is higher |
//! | Industry | at most [`INDUSTRY_CAP`] of equity in one SEC industry |
//! | Drawdown | down [`DRAWDOWN_HALT`] from the high over [`DRAWDOWN_SESSIONS`] sessions: a [`CASH_LOCK_HOURS`]-hour cash lock |

use crate::model::{Bar, Intel};
use chrono::NaiveDate;

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
pub fn liquidity(close: f64, adtv: f64, min_price: f64, min_adtv: f64) -> Option<String> {
    let (price_floor, adtv_floor) = (min_price.max(MIN_PRICE), min_adtv.max(MIN_ADTV));
    if close < price_floor {
        Some(format!(
            "Price ${close:.2} is below the ${price_floor:.2} floor"
        ))
    } else if adtv < adtv_floor {
        Some(format!(
            "Average daily traded value ${:.1}M over {ADTV_DAYS} sessions is below ${:.1}M",
            adtv / 1e6,
            adtv_floor / 1e6
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
pub fn event(symbol: &str, intel: &Intel, today: NaiveDate) -> Option<String> {
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
        assert!(liquidity(4.99, 1e9, 1.0, 0.0).unwrap().contains("$5.00"));
        assert!(liquidity(20.0, 4.9e6, 1.0, 1e6).unwrap().contains("$5.0M"));
        assert!(liquidity(20.0, 5e6, 1.0, 1e6).is_none());
        // A stricter strategy floor wins.
        assert!(liquidity(8.0, 9e6, 10.0, 1e6).unwrap().contains("$10.00"));
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
        assert!(event("UP", &intel, today).unwrap().contains("2026-11-17"));
        assert!(event("PAST", &intel, today).unwrap().contains("recent"));
        assert!(event("FAR", &intel, today).is_none());
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
        assert!(event("RS", &intel, today).unwrap().contains("item 5.03"));
        assert!(event("MERGE", &intel, today).unwrap().contains("S-4"));
        assert!(event("OLD", &intel, today).is_none());
        assert!(event("EARN", &intel, today).is_none());
    }

    #[test]
    fn fda_and_restructuring_headlines_block_a_buy() {
        let today = day("2026-11-10");
        let news = |title: &str, published: &str| Headline {
            title: title.into(),
            published: published.into(),
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
            event("GIBO", &intel, today)
                .unwrap()
                .contains("share consolidation")
        );
        assert!(
            event("BIO", &intel, today)
                .unwrap()
                .contains("FDA decision")
        );
        assert!(event("STALE", &intel, today).is_none());
        assert!(event("WORD", &intel, today).is_none(), "whole words only");
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
}
