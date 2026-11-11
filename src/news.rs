//! The news policy: which items a model is allowed to read.
//!
//! The provider's per-ticker feed mixes three very different things: the company's own
//! announcements, reporting from newsrooms, and a large volume of templated commentary
//! ("Wall Street Analysts Believe X Could Rally 26.8%", "X Stock Looks Like A Bargain As
//! Shares Fell 58%", "Why Did A, B, C Surge Today?"). In the first week the desk's models
//! quoted the third kind as fact: an "analyst upside of 25.28%" and a "26.8% rally" went
//! into buy theses, and a valuation blog's "fell 58%" became "the stock recently fell 58%".
//!
//! So the policy is explicit and editable (`config/news_policy.json`, or the file named by
//! `PAPER_TRADER_NEWS_POLICY`). An item is graded:
//!
//! | Grade | What it is | Shown to models |
//! |---|---|---|
//! | `primary` | the issuer's own announcement: the title starts with the issuer's name and an announcing verb | yes |
//! | `reporting` | published on a listed newsroom or wire domain | yes |
//! | `commentary` | matched to the issuer but neither of the above | only if `allow_commentary` |
//! | `blocked` | from a blocked domain, or its title contains a blocked phrase | never |
//!
//! Grading is by rule. Nothing here asks a model whether an article is good.

use serde::{Deserialize, Serialize};
use std::sync::OnceLock;

/// The environment variable naming a policy file to use instead of the built-in one.
pub const POLICY_VAR: &str = "PAPER_TRADER_NEWS_POLICY";
const DEFAULT_POLICY: &str = include_str!("../config/news_policy.json");

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Quality {
    Primary,
    Reporting,
    #[default]
    Commentary,
    Blocked,
}

/// What kind of corporate event a title or filing describes. Only some kinds can move a
/// stock's value; a conference slot or a board appointment is a notice, not a catalyst.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EventKind {
    Results,
    Guidance,
    Deal,
    Financing,
    Regulatory,
    Product,
    Personnel,
    Insider,
    Holder,
    Administrative,
    #[default]
    Other,
}

impl EventKind {
    /// Whether an event of this kind may be cited as the basis of an event-driven pick.
    pub fn material(self) -> bool {
        matches!(
            self,
            Self::Results
                | Self::Guidance
                | Self::Deal
                | Self::Financing
                | Self::Regulatory
                | Self::Product
        )
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Results => "results",
            Self::Guidance => "guidance",
            Self::Deal => "merger or acquisition",
            Self::Financing => "financing or capital return",
            Self::Regulatory => "regulatory or listing",
            Self::Product => "product or contract",
            Self::Personnel => "personnel notice",
            Self::Insider => "insider form",
            Self::Holder => "holder form",
            Self::Administrative => "administrative notice",
            Self::Other => "unclassified notice",
        }
    }
}

/// Classify an announcement title by the kind of event it describes.
pub fn event_kind(title: &str) -> EventKind {
    let lower = format!(" {} ", title.to_lowercase());
    let has = |list: &[&str]| list.iter().any(|w| lower.contains(w));
    if has(&[
        " to report ",
        " to present ",
        " to host ",
        " to participate ",
        " to announce ",
        " to webcast ",
        "conference",
        "inducement",
        "webcast",
        "annual meeting",
        "investor day",
    ]) {
        EventKind::Administrative
    } else if has(&[
        "guidance",
        "outlook",
        "raises full",
        "lowers full",
        "forecast",
    ]) {
        EventKind::Guidance
    } else if has(&["results", "earnings", "quarter", "fiscal year"]) {
        EventKind::Results
    } else if has(&[
        "acquire",
        "acquisition",
        "merger",
        "to be acquired",
        "divest",
        "tender offer",
        "going private",
    ]) {
        EventKind::Deal
    } else if has(&[
        "offering",
        "pricing of",
        "private placement",
        "credit facility",
        "buyback",
        "repurchase",
        "dividend",
        "convertible",
        "share consolidation",
        "reverse split",
        "reverse stock split",
        "stock split",
    ]) {
        EventKind::Financing
    } else if has(&[
        " fda ",
        "approval",
        "clearance",
        "compliance",
        "notification",
        "delist",
        "bankruptcy",
        "chapter 11",
        "investigation",
        "settlement",
    ]) {
        EventKind::Regulatory
    } else if has(&[
        "appoint",
        " names ",
        " joins ",
        "board of directors",
        "chief ",
        "retire",
        "resign",
        "succession",
    ]) {
        EventKind::Personnel
    } else if has(&[
        "launch",
        "now available",
        "contract",
        "agreement",
        "partnership",
        "award",
        "introduces",
        "collaboration",
        "order",
    ]) {
        EventKind::Product
    } else {
        EventKind::Other
    }
}

/// The editable policy.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct NewsPolicy {
    /// An event older than this is history, not a current catalyst.
    pub max_age_days: i64,
    /// Most verified events listed in one dossier.
    pub max_items_per_stock: usize,
    /// Show matched commentary to the models (never as an event). Off by default.
    pub allow_commentary: bool,
    pub newsroom_domains: Vec<String>,
    pub blocked_domains: Vec<String>,
    pub blocked_phrases: Vec<String>,
    pub announcement_verbs: Vec<String>,
}

/// The host of a URL without `www.`.
pub fn host(url: &str) -> String {
    url.split("//")
        .nth(1)
        .unwrap_or(url)
        .split(['/', '?'])
        .next()
        .unwrap_or("")
        .trim_start_matches("www.")
        .to_lowercase()
}

fn on_domain(host: &str, domains: &[String]) -> bool {
    domains
        .iter()
        .any(|d| host == d.as_str() || host.ends_with(&format!(".{d}")))
}

impl NewsPolicy {
    /// Parse a policy file.
    pub fn parse(json: &str) -> anyhow::Result<Self> {
        let policy: NewsPolicy = serde_json::from_str(json)?;
        if policy.max_age_days < 1 || policy.max_items_per_stock < 1 {
            anyhow::bail!("news policy: max_age_days and max_items_per_stock must be positive");
        }
        Ok(policy)
    }

    /// The built-in policy.
    pub fn builtin() -> Self {
        Self::parse(DEFAULT_POLICY).expect("config/news_policy.json is valid")
    }

    /// The policy in force: the file named by [`POLICY_VAR`] when it is set and valid,
    /// otherwise the built-in one. A file that is set but unreadable is reported once on
    /// stderr and the built-in policy is used, which is the stricter choice.
    pub fn current() -> &'static NewsPolicy {
        static POLICY: OnceLock<NewsPolicy> = OnceLock::new();
        POLICY.get_or_init(|| {
            let Some(path) = std::env::var(POLICY_VAR)
                .ok()
                .filter(|p| !p.trim().is_empty())
            else {
                return Self::builtin();
            };
            match std::fs::read_to_string(&path)
                .map_err(anyhow::Error::from)
                .and_then(|text| Self::parse(&text))
            {
                Ok(policy) => policy,
                Err(error) => {
                    eprintln!(
                        "news policy {path} is unusable ({error}); using the built-in policy"
                    );
                    Self::builtin()
                }
            }
        })
    }

    /// Grade one matched item. `after_issuer` is the title's words that follow the issuer's
    /// name when the title starts with it (lowercase), or None when it does not. `ticker`
    /// is the symbol in lowercase.
    pub fn grade(
        &self,
        title: &str,
        link: &str,
        after_issuer: Option<&[String]>,
        ticker: &str,
    ) -> (Quality, String) {
        let host = host(link);
        if on_domain(&host, &self.blocked_domains) {
            return (Quality::Blocked, format!("{host} is a blocked source"));
        }
        let lower = title.to_lowercase();
        if let Some(phrase) = self
            .blocked_phrases
            .iter()
            .find(|p| lower.contains(p.as_str()))
        {
            return (
                Quality::Blocked,
                format!("the title contains the blocked phrase \"{phrase}\""),
            );
        }
        if lower.trim_end().ends_with('?') {
            return (
                Quality::Blocked,
                "a question headline is not a report".into(),
            );
        }
        if on_domain(&host, &self.newsroom_domains) {
            return (Quality::Reporting, format!("published by {host}"));
        }
        // An issuer's own release reads "<Issuer> Announces ...". Aggregators write
        // "<Issuer> (TICK) Beats ...", so a ticker straight after the name is not one.
        if let Some(rest) = after_issuer {
            let aggregator = rest.first().is_some_and(|w| w == ticker);
            let verb = rest
                .iter()
                .take(2)
                .any(|w| self.announcement_verbs.iter().any(|v| v == w));
            if !aggregator && verb {
                return (Quality::Primary, "the issuer's own announcement".into());
            }
        }
        (
            Quality::Commentary,
            "matched to the issuer, but neither its own announcement nor a listed newsroom".into(),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn words(text: &str) -> Vec<String> {
        text.split(|c: char| !c.is_alphanumeric())
            .filter(|w| !w.is_empty())
            .map(str::to_lowercase)
            .collect()
    }

    fn grade(title: &str, link: &str, issuer_words: usize, ticker: &str) -> Quality {
        let all = words(title);
        let rest = (issuer_words > 0).then(|| all[issuer_words.min(all.len())..].to_vec());
        NewsPolicy::builtin()
            .grade(title, link, rest.as_deref(), ticker)
            .0
    }

    const YAHOO: &str = "https://finance.yahoo.com/news/x.html";

    #[test]
    fn the_articles_the_models_quoted_in_week_one_are_blocked() {
        for (title, link) in [
            (
                "Wall Street Analysts Believe Replimune Group (REPL) Could Rally 26.8%: Here's is How to Trade",
                YAHOO,
            ),
            (
                "How Much Upside is Left in The RealReal (REAL)? Wall Street Analysts Think 25.28%",
                YAHOO,
            ),
            (
                "The RealReal (REAL) is a Great Momentum Stock: Should You Buy?",
                YAHOO,
            ),
            (
                "Pool (POOL) Stock Looks Like A Bargain As Shares Fell 58%",
                YAHOO,
            ),
            (
                "Replimune Soars 17% as TUDRIQEV Melanoma Launch Draws “Enthusiastic” Response From Physicians",
                "https://www.tikr.com/blog/x",
            ),
            (
                "Why Did ETSY, REPL, GMAB Stocks Surge To 52-Week Highs Today?",
                "https://stocktwits.com/news-articles/x",
            ),
            (
                "Replimune: 3 Key Takeaways for Long-Term Investors",
                "https://www.fool.com/earnings/x",
            ),
            (
                "ASX Penny Stocks With Market Caps Over A$100M To Consider",
                YAHOO,
            ),
            (
                "AI in Virtual Medical Assistants Market to Worth USD 8.85 Billion by 2030, at a CAGR of 36.6%",
                YAHOO,
            ),
            ("RealReal Stock Is Up 13.8% What’s Changed", YAHOO),
            (
                "Do Options Traders Know Something About Pool Stock We Don't?",
                YAHOO,
            ),
        ] {
            assert_eq!(grade(title, link, 0, "x"), Quality::Blocked, "{title}");
        }
    }

    #[test]
    fn an_issuers_own_release_is_primary_and_an_aggregators_rewrite_is_not() {
        assert_eq!(
            grade(
                "Replimune Reports Fiscal Second Quarter 2027 Financial Results and Provides Corporate Update",
                YAHOO,
                1,
                "repl"
            ),
            Quality::Primary
        );
        assert_eq!(
            grade(
                "The RealReal Appoints Shar Dubey to Board of Directors",
                YAHOO,
                2,
                "real"
            ),
            Quality::Primary
        );
        assert_eq!(
            grade(
                "THE REALREAL ANNOUNCES THIRD QUARTER 2026 RESULTS",
                YAHOO,
                2,
                "real"
            ),
            Quality::Primary
        );
        // "<Issuer> (TICK) Tops ..." is an aggregator's template.
        assert_eq!(
            grade(
                "The RealReal (REAL) Tops Q3 Earnings and Revenue Estimates",
                YAHOO,
                2,
                "real"
            ),
            Quality::Commentary
        );
        assert_eq!(
            grade(
                "MediaAlpha Q3 Earnings Call Highlights Broadening Insurance Demand",
                YAHOO,
                1,
                "max"
            ),
            Quality::Commentary
        );
        // A title that does not start with the issuer is not its release.
        assert_eq!(
            grade(
                "Univest Securities Announces Closing of Offering for its Client Ohmyhome",
                YAHOO,
                0,
                "omh"
            ),
            Quality::Commentary
        );
        assert_eq!(
            grade(
                "Acme wins Pentagon contract",
                "https://www.reuters.com/business/x",
                0,
                "acme"
            ),
            Quality::Reporting
        );
    }

    #[test]
    fn hosts_and_event_kinds_parse() {
        assert_eq!(host("https://www.fool.com/earnings/x"), "fool.com");
        assert_eq!(
            host("https://finance.yahoo.com/markets/a.html?.tsrc=rss"),
            "finance.yahoo.com"
        );
        assert!(on_domain("app.moby.co", &["moby.co".to_string()]));
        assert!(!on_domain("notmoby.co", &["moby.co".to_string()]));
        for (title, kind) in [
            (
                "Replimune Reports Fiscal Second Quarter 2027 Financial Results",
                EventKind::Results,
            ),
            ("Acme Raises Full-Year Guidance", EventKind::Guidance),
            (
                "The RealReal Appoints Shar Dubey to Board of Directors",
                EventKind::Personnel,
            ),
            (
                "Replimune to Present at the Cantor Global Healthcare Conference 2026",
                EventKind::Administrative,
            ),
            (
                "Replimune to Report the Fiscal Second Quarter 2027 Financial Results",
                EventKind::Administrative,
            ),
            (
                "Teladoc Health Announces Employee Inducement Awards under NYSE Rule 303A.08",
                EventKind::Administrative,
            ),
            (
                "Ohmyhome Ltd Announces Pricing of $4 Million Registered Direct Offering",
                EventKind::Financing,
            ),
            (
                "Ohmyhome Limited Regains Compliance with Nasdaq Minimum Closing Bid Price Rule",
                EventKind::Regulatory,
            ),
            (
                "Replimune Announces TUDRIQEV Now Available in the U.S. for Advanced Melanoma",
                EventKind::Product,
            ),
            ("Acme to Acquire Widget Corp", EventKind::Deal),
            ("Acme Issues Statement", EventKind::Other),
        ] {
            assert_eq!(event_kind(title), kind, "{title}");
        }
        assert!(EventKind::Results.material() && EventKind::Financing.material());
        assert!(!EventKind::Personnel.material() && !EventKind::Administrative.material());
        assert!(!EventKind::Other.material());
    }

    #[test]
    fn a_policy_file_can_loosen_or_tighten_the_defaults() {
        let mut edited = NewsPolicy::builtin();
        edited.blocked_domains.push("finance.yahoo.com".into());
        let json = serde_json::to_string(&edited).unwrap();
        let policy = NewsPolicy::parse(&json).unwrap();
        assert_eq!(
            policy
                .grade(
                    "Acme Announces Results",
                    YAHOO,
                    Some(&["announces".into()]),
                    "acme"
                )
                .0,
            Quality::Blocked
        );
        assert!(NewsPolicy::parse("{}").is_err());
        assert!(
            NewsPolicy::parse(&json.replace("\"max_age_days\":30", "\"max_age_days\":0")).is_err()
        );
        assert!(!NewsPolicy::builtin().allow_commentary);
    }
}
