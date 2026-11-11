//! SEC EDGAR filings: the recent 8-Ks, quarterly reports, insider forms and large-holder
//! stakes of a company, and the text of one filing on request. No key is needed, but the
//! SEC asks every client to identify itself and to stay under ten requests a second.

use crate::http;
use crate::model::Filing;
use anyhow::{Result, anyhow};
use chrono::{Duration as Days, NaiveDate};
use serde_json::Value;
use std::collections::{BTreeMap, VecDeque};
use std::fmt::Write as _;
use std::sync::Mutex;
use std::time::Duration;

/// The SEC's fair-access policy asks automated clients to identify themselves with a real
/// contact, and its servers refuse requests that do not carry one. The contact is therefore
/// a setting, `PAPER_TRADER_SEC_CONTACT`, and EDGAR stays off until someone provides it.
pub const CONTACT_VAR: &str = "PAPER_TRADER_SEC_CONTACT";

/// The user agent for a contact, in the form the SEC asks for.
pub fn user_agent(contact: &str) -> String {
    format!("paper-trader research {}", contact.trim())
}

const TICKER_URL: &str = "https://www.sec.gov/files/company_tickers.json";
const FORMS: [&str; 20] = [
    "8-K", "10-Q", "10-K", "4", "SC 13D", "SC 13G", "SC 13D/A", "SC 13G/A", "S-3", "424B5",
    "DEF 14A", // Restructuring, merger and delisting forms, which the event gate reads:
    "S-4", "DEFM14A", "PREM14A", "425", "SC TO-T", "SC 13E3", "15-12B", "15-12G", "25-NSE",
];

/// Ticker to CIK lookup, loaded once per run.
#[derive(Debug)]
pub struct Edgar {
    cik_by_ticker: BTreeMap<String, u64>,
    title_by_ticker: BTreeMap<String, String>,
    agent: String,
}

/// Parse the SEC company ticker file into upper-case ticker to registrant name.
pub fn parse_ticker_titles(json: &str) -> BTreeMap<String, String> {
    let Ok(Value::Object(entries)) = serde_json::from_str::<Value>(json) else {
        return BTreeMap::new();
    };
    entries
        .values()
        .filter_map(|e| {
            Some((
                e["ticker"].as_str()?.trim().to_uppercase(),
                e["title"].as_str()?.trim().to_string(),
            ))
        })
        .filter(|(_, title)| !title.is_empty())
        .collect()
}

/// Parse the SEC company ticker file into upper-case ticker to CIK.
pub fn parse_ticker_map(json: &str) -> BTreeMap<String, u64> {
    let Ok(Value::Object(entries)) = serde_json::from_str::<Value>(json) else {
        return BTreeMap::new();
    };
    entries
        .values()
        .filter_map(|e| {
            Some((
                e["ticker"].as_str()?.trim().to_uppercase(),
                e["cik_str"].as_u64()?,
            ))
        })
        .collect()
}

/// The company's SIC industry from a submissions feed, as `"7372 Services-Prepackaged
/// Software"`. None when the feed has no SIC code (funds and some foreign issuers).
pub fn parse_industry(json: &str) -> Option<String> {
    let value = serde_json::from_str::<Value>(json).ok()?;
    let code = value["sic"]
        .as_str()
        .map(str::trim)
        .filter(|c| !c.is_empty())?;
    let name = value["sicDescription"].as_str().unwrap_or("").trim();
    Some(if name.is_empty() {
        code.to_string()
    } else {
        format!("{code} {name}")
    })
}

/// Parse a submissions feed into the filings worth reading, newest first.
pub fn parse_submissions(json: &str, limit: usize) -> Vec<Filing> {
    let Ok(value) = serde_json::from_str::<Value>(json) else {
        return Vec::new();
    };
    let recent = &value["filings"]["recent"];
    let column = |name: &str, i: usize| {
        recent[name]
            .get(i)
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string()
    };
    let count = recent["form"].as_array().map_or(0, Vec::len);
    (0..count)
        .filter(|i| FORMS.contains(&column("form", *i).as_str()))
        .take(limit)
        .map(|i| Filing {
            form: column("form", i),
            date: column("filingDate", i),
            items: column("items", i),
            description: column("primaryDocDescription", i),
            accession: column("accessionNumber", i),
            document: column("primaryDocument", i),
            ..Filing::default()
        })
        .collect()
}

impl Edgar {
    pub fn load() -> Result<Self> {
        let contact = std::env::var(CONTACT_VAR)
            .ok()
            .filter(|c| c.contains('@'))
            .ok_or_else(|| {
                anyhow!("set {CONTACT_VAR} to an email address to turn SEC filings on")
            })?;
        Self::with_contact(&contact)
    }

    /// Load the ticker map, identifying ourselves to the SEC with `contact`.
    pub fn with_contact(contact: &str) -> Result<Self> {
        let agent = user_agent(contact);
        let body = http::get(TICKER_URL, &agent, Duration::from_secs(30))?;
        let cik_by_ticker = parse_ticker_map(&body);
        if cik_by_ticker.is_empty() {
            return Err(anyhow!("the SEC ticker file was empty or unreadable"));
        }
        Ok(Edgar {
            cik_by_ticker,
            title_by_ticker: parse_ticker_titles(&body),
            agent,
        })
    }

    /// The SEC's registrant for a ticker: its CIK, zero-padded to ten digits, and its name.
    pub fn registrant(&self, symbol: &str) -> Option<(String, String)> {
        let cik = self.cik(symbol)?;
        let upper = symbol.to_uppercase();
        let title = self
            .title_by_ticker
            .get(&upper)
            .or_else(|| self.title_by_ticker.get(&upper.replace('.', "-")))
            .cloned()
            .unwrap_or_default();
        Some((format!("{cik:010}"), title))
    }

    fn cik(&self, symbol: &str) -> Option<u64> {
        let upper = symbol.to_uppercase();
        self.cik_by_ticker
            .get(&upper)
            .or_else(|| self.cik_by_ticker.get(&upper.replace('.', "-")))
            .copied()
    }

    /// Filings dated on or after `since`, newest first. Empty on any error.
    pub fn recent_filings(&self, symbol: &str, since: NaiveDate, limit: usize) -> Vec<Filing> {
        self.recent(symbol, since, limit).0
    }

    /// Recent filings and the company's SEC industry, from one submissions request.
    pub fn recent(
        &self,
        symbol: &str,
        since: NaiveDate,
        limit: usize,
    ) -> (Vec<Filing>, Option<String>) {
        let Some(cik) = self.cik(symbol) else {
            return (Vec::new(), None);
        };
        let url = format!("https://data.sec.gov/submissions/CIK{cik:010}.json");
        let Ok(body) = http::get(&url, &self.agent, Duration::from_secs(20)) else {
            return (Vec::new(), None);
        };
        let filings = parse_submissions(&body, usize::MAX)
            .into_iter()
            .filter(|f| f.date.parse::<NaiveDate>().is_ok_and(|d| d >= since))
            .take(limit)
            .map(|f| Filing {
                cik: format!("{cik:010}"),
                ..f
            })
            .collect();
        (filings, parse_industry(&body))
    }

    /// The readable text of one filing's primary document, cut to `max_chars`.
    pub fn filing_text(&self, symbol: &str, filing: &Filing, max_chars: usize) -> Result<String> {
        let cik = self
            .cik(symbol)
            .ok_or_else(|| anyhow!("no CIK for {symbol}"))?;
        let url = format!(
            "https://www.sec.gov/Archives/edgar/data/{cik}/{}/{}",
            filing.accession.replace('-', ""),
            filing.document
        );
        let html = http::get(&url, &self.agent, Duration::from_secs(30))?;
        Ok(readable(&html, &filing.form, max_chars))
    }

    /// The transactions a Form 4 reports, read from its ownership XML. The submissions feed
    /// names the styled view (`xslF345X05/form4.xml`); the data is the same file without
    /// the style folder.
    pub fn form4_detail(&self, symbol: &str, filing: &Filing) -> Result<String> {
        let cik = self
            .cik(symbol)
            .ok_or_else(|| anyhow!("no CIK for {symbol}"))?;
        let document = filing
            .document
            .rsplit('/')
            .next()
            .unwrap_or(&filing.document);
        let url = format!(
            "https://www.sec.gov/Archives/edgar/data/{cik}/{}/{document}",
            filing.accession.replace('-', "")
        );
        let xml = http::get(&url, &self.agent, Duration::from_secs(30))?;
        parse_form4(&xml).ok_or_else(|| anyhow!("no ownership data in the Form 4"))
    }
}

/// The inner text of every `<name>` element in `xml`.
fn xml_blocks<'a>(xml: &'a str, name: &str) -> Vec<&'a str> {
    let (open, close) = (format!("<{name}>"), format!("</{name}>"));
    let mut found = Vec::new();
    let mut rest = xml;
    while let Some(start) = rest.find(&open) {
        let tail = &rest[start + open.len()..];
        let Some(end) = tail.find(&close) else { break };
        found.push(&tail[..end]);
        rest = &tail[end + close.len()..];
    }
    found
}

/// The text of the first `<name>` element, or of the `<value>` inside it.
fn xml_text(xml: &str, name: &str) -> Option<String> {
    let block = xml_blocks(xml, name).into_iter().next()?;
    let inner = xml_blocks(block, "value")
        .into_iter()
        .next()
        .unwrap_or(block);
    let text = inner.trim();
    (!text.is_empty() && !text.contains('<')).then(|| text.to_string())
}

/// What a Form 4 transaction code means. Only `P` is a purchase on the open market and
/// only `S` a sale; the rest are grants, exercises, withholdings and gifts, which say
/// little about an insider's view.
pub fn form4_code(code: &str) -> &'static str {
    match code.trim() {
        "P" => "open-market purchase",
        "S" => "open-market sale",
        "A" => "grant or award (not a purchase)",
        "M" | "X" => "option exercise",
        "F" => "shares withheld for tax",
        "G" => "gift",
        "D" => "disposition to the issuer",
        "C" => "conversion",
        "J" => "other transaction",
        _ => "transaction of another kind",
    }
}

fn thousands(value: f64) -> String {
    let whole = format!("{:.0}", value.abs());
    let mut out = String::new();
    for (i, c) in whole.chars().enumerate() {
        if i > 0 && (whole.len() - i) % 3 == 0 {
            out.push(',');
        }
        out.push(c);
    }
    out
}

/// Summarise a Form 4 ownership document: who reported, and each non-derivative
/// transaction with its code, direction, size, price and date. None when the text is not
/// an ownership document.
pub fn parse_form4(xml: &str) -> Option<String> {
    if !xml.contains("<ownershipDocument") {
        return None;
    }
    let flag = |name: &str| xml_text(xml, name).is_some_and(|v| v == "1" || v == "true");
    let mut role = xml_text(xml, "officerTitle").unwrap_or_default();
    if role.is_empty() {
        role = if flag("isDirector") {
            "Director".into()
        } else if flag("isTenPercentOwner") {
            "10% owner".into()
        } else {
            "Reporting person".into()
        };
    }
    let name = xml_text(xml, "rptOwnerName").unwrap_or_else(|| "name not given".into());
    let mut lines: Vec<String> = Vec::new();
    let transactions = xml_blocks(xml, "nonDerivativeTransaction");
    for block in transactions.iter().take(4) {
        let code = xml_text(block, "transactionCode").unwrap_or_default();
        let shares = xml_text(block, "transactionShares").and_then(|v| v.parse::<f64>().ok());
        let price = xml_text(block, "transactionPricePerShare")
            .and_then(|v| v.parse::<f64>().ok())
            .filter(|p| *p > 0.0);
        let date = xml_text(block, "transactionDate").unwrap_or_default();
        let mut line = form4_code(&code).to_string();
        if let Some(shares) = shares {
            let _ = write!(line, " of {} shares", thousands(shares));
        }
        if let Some(price) = price {
            let _ = write!(line, " at ${price:.2}");
        }
        if !date.is_empty() {
            let _ = write!(line, " on {date}");
        }
        lines.push(line);
    }
    if transactions.len() > 4 {
        lines.push(format!("and {} more", transactions.len() - 4));
    }
    if lines.is_empty() {
        lines
            .push("no share transaction reported (derivative grants or holdings only)".to_string());
    }
    Some(format!("{role} {name}: {}", lines.join("; ")))
}

/// Remove every `<name ...>...</name>` element, case-insensitively.
fn drop_elements(html: &str, name: &str) -> String {
    let lower = html.to_ascii_lowercase();
    let (open, close) = (format!("<{name}"), format!("</{name}>"));
    let mut out = String::with_capacity(html.len());
    let mut at = 0;
    while let Some(start) = lower[at..].find(&open).map(|i| i + at) {
        out.push_str(&html[at..start]);
        let Some(end) = lower[start..].find(&close) else {
            at = html.len();
            break;
        };
        at = start + end + close.len();
    }
    out.push_str(&html[at..]);
    out
}

/// Financial concepts worth showing a reader, with a plain label.
const FACTS: [(&str, &str); 9] = [
    ("Revenues", "revenue"),
    (
        "RevenueFromContractWithCustomerExcludingAssessedTax",
        "revenue",
    ),
    ("SalesRevenueNet", "revenue"),
    ("OperatingIncomeLoss", "operating income"),
    ("NetIncomeLoss", "net income"),
    ("EarningsPerShareDiluted", "diluted EPS"),
    ("CashAndCashEquivalentsAtCarryingValue", "cash"),
    ("LongTermDebt", "long-term debt"),
    ("StockholdersEquity", "shareholders' equity"),
];

fn attribute(tag: &str, name: &str) -> Option<String> {
    let at = tag.find(&format!("{name}=\""))? + name.len() + 2;
    let end = tag[at..].find('"')? + at;
    Some(tag[at..end].to_string())
}

fn money(value: f64) -> String {
    let magnitude = value.abs();
    let sign = if value < 0.0 { "-" } else { "" };
    if magnitude >= 1e9 {
        format!("{sign}${:.2}B", magnitude / 1e9)
    } else if magnitude >= 1e6 {
        format!("{sign}${:.1}M", magnitude / 1e6)
    } else if magnitude >= 1e3 {
        format!("{sign}${:.0}K", magnitude / 1e3)
    } else {
        format!("{sign}${magnitude:.2}")
    }
}

/// The first tagged value of each concept in [`FACTS`] from an inline XBRL document. The
/// first value is normally the current period's, but the period is not checked, so the
/// line says so.
pub fn tagged_facts(html: &str) -> Vec<String> {
    let mut found: Vec<(&str, String)> = Vec::new();
    let mut rest = html;
    while let Some(start) = rest.find("<ix:nonFraction") {
        let tail = &rest[start..];
        let Some(open_end) = tail.find('>') else {
            break;
        };
        let tag = &tail[..open_end];
        let Some(close) = tail.find("</ix:nonFraction>") else {
            break;
        };
        let inner = strip_html(&tail[open_end + 1..close]);
        rest = &tail[close + 17..];
        let Some(concept) = attribute(tag, "name") else {
            continue;
        };
        let concept = concept.rsplit(':').next().unwrap_or("").to_string();
        let Some((_, label)) = FACTS.iter().find(|(c, _)| *c == concept) else {
            continue;
        };
        if found.iter().any(|(l, _)| l == label) {
            continue;
        }
        let Ok(number) = inner.replace([',', '$', ' '], "").parse::<f64>() else {
            continue;
        };
        let scale: i32 = attribute(tag, "scale")
            .and_then(|s| s.parse().ok())
            .unwrap_or(0);
        let negative = attribute(tag, "sign").is_some_and(|s| s == "-");
        let value = number * 10f64.powi(scale) * if negative { -1.0 } else { 1.0 };
        let shown = if label.contains("EPS") {
            format!("{value:.2}")
        } else {
            money(value)
        };
        found.push((label, shown));
    }
    found
        .into_iter()
        .map(|(label, shown)| format!("{label} {shown}"))
        .collect()
}

/// Where the part of a filing worth reading starts, by form type.
fn section_start(text: &str, form: &str) -> usize {
    let find_all = |needle: &str| -> Vec<usize> {
        let lower = text.to_ascii_lowercase();
        let mut found = Vec::new();
        let mut at = 0;
        while let Some(i) = lower[at..].find(needle) {
            found.push(at + i);
            at += i + needle.len();
        }
        found
    };
    match form {
        "8-K" => {
            // The first "Item N.NN" heading; the cover page before it is boilerplate.
            let bytes = text.as_bytes();
            find_all("item ")
                .into_iter()
                .find(|i| {
                    let b = &bytes[(i + 5).min(bytes.len())..];
                    b.len() >= 4 && b[0].is_ascii_digit() && b[1] == b'.' && b[2].is_ascii_digit()
                })
                .unwrap_or(0)
        }
        "10-Q" | "10-K" => {
            // The discussion of results. The first mention is the table of contents.
            let mentions = find_all("discussion and analysis of financial condition");
            mentions.get(1).or(mentions.first()).copied().unwrap_or(0)
        }
        _ => 0,
    }
}

/// The readable part of a filing: tagged figures first, then the text from the section a
/// reader wants, with the inline XBRL header (context definitions and hidden facts, which
/// is most of what the first screen of a raw filing contains) removed.
pub fn readable(html: &str, form: &str, max_chars: usize) -> String {
    let facts = tagged_facts(html);
    let cleaned = drop_elements(&drop_elements(html, "ix:header"), "ix:hidden");
    let text = strip_html(&cleaned);
    let mut start = section_start(&text, form);
    while !text.is_char_boundary(start) {
        start -= 1;
    }
    let mut out = String::new();
    if !facts.is_empty() {
        let _ = writeln!(
            out,
            "Figures as tagged in the filing (first value of each; the period is not checked): {}.",
            facts.join("; ")
        );
    }
    let room = max_chars.saturating_sub(out.chars().count());
    out.push_str(&truncate(&text[start..], room));
    out
}

/// Recent filings and industries for many symbols, politely: four workers, a pause after
/// each request.
pub fn gather_filings(
    edgar: &Edgar,
    symbols: &[String],
    today: NaiveDate,
    days: i64,
    per_symbol: usize,
) -> (BTreeMap<String, Vec<Filing>>, BTreeMap<String, String>) {
    let since = today - Days::days(days);
    let queue = Mutex::new(symbols.iter().cloned().collect::<VecDeque<_>>());
    let found = Mutex::new(BTreeMap::new());
    let industries = Mutex::new(BTreeMap::new());
    std::thread::scope(|scope| {
        for _ in 0..4 {
            scope.spawn(|| {
                while let Some(symbol) = queue.lock().unwrap().pop_front() {
                    let (filings, industry) = edgar.recent(&symbol, since, per_symbol);
                    std::thread::sleep(Duration::from_millis(120));
                    if let Some(industry) = industry {
                        industries.lock().unwrap().insert(symbol.clone(), industry);
                    }
                    if !filings.is_empty() {
                        found.lock().unwrap().insert(symbol, filings);
                    }
                }
            });
        }
    });
    (
        found.into_inner().unwrap(),
        industries.into_inner().unwrap(),
    )
}

fn truncate(text: &str, max_chars: usize) -> String {
    text.chars().take(max_chars).collect()
}

/// Remove scripts, styles and tags, decode common entities and collapse whitespace.
pub fn strip_html(html: &str) -> String {
    let mut text = String::with_capacity(html.len());
    let mut rest = html;
    while let Some(start) = rest.find('<') {
        text.push_str(&rest[..start]);
        let tail = &rest[start..];
        let lower: String = tail
            .chars()
            .take(8)
            .collect::<String>()
            .to_ascii_lowercase();
        let block = ["script", "style"]
            .into_iter()
            .find(|name| lower[1..].starts_with(name));
        if let Some(name) = block {
            let closing = format!("</{name}");
            let lowered = tail.to_ascii_lowercase();
            rest = lowered
                .find(&closing)
                .and_then(|at| tail[at..].find('>').map(|end| &tail[at + end + 1..]))
                .unwrap_or("");
        } else if let Some(end) = tail.find('>') {
            text.push(' ');
            rest = &tail[end + 1..];
        } else {
            rest = "";
        }
    }
    text.push_str(rest);
    let decoded = text
        .replace("&nbsp;", " ")
        .replace("&#160;", " ")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&amp;", "&");
    decoded.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// What an 8-K item code means, in a few words.
pub fn item_meaning(code: &str) -> &'static str {
    match code.trim() {
        "1.01" => "material agreement",
        "1.02" => "agreement terminated",
        "2.01" => "acquisition or disposal",
        "2.02" => "earnings results",
        "2.03" => "new debt",
        "2.05" => "restructuring costs",
        "2.06" => "impairment",
        "3.01" => "listing notice",
        "3.02" => "unregistered equity sale",
        "4.01" => "auditor change",
        "4.02" => "financials not reliable",
        "5.01" => "change of control",
        "5.02" => "officer or director change",
        "5.03" => "bylaws amended",
        "5.07" => "shareholder vote",
        "7.01" => "Reg FD disclosure",
        "8.01" => "other events",
        "9.01" => "exhibits",
        _ => "other",
    }
}

/// One compact line for a filing, for prompts and the dashboard.
pub fn describe(f: &Filing) -> String {
    let what = match f.form.as_str() {
        // A Form 4 reports a transaction in either direction, or a grant, a gift or a tax
        // withholding. Until the document is read, all that is known is that one was filed.
        "4" if f.detail.is_empty() => {
            "Form 4 filed (an insider transaction; direction not extracted)".to_string()
        }
        "4" => format!("Form 4: {}", f.detail),
        "10-Q" => "quarterly report".to_string(),
        "10-K" => "annual report".to_string(),
        "8-K" => {
            let meanings: Vec<&str> = f
                .items
                .split(',')
                .filter(|c| !c.trim().is_empty())
                .map(item_meaning)
                .collect();
            if meanings.is_empty() {
                "current report".to_string()
            } else {
                meanings.join("; ")
            }
        }
        form if form.starts_with("SC 13") => "large holder stake".to_string(),
        "S-3" | "424B5" => "securities offering".to_string(),
        "DEF 14A" => "proxy statement".to_string(),
        other => other.to_string(),
    };
    format!("{} {}: {what}", f.date, f.form)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ticker_map_uppercases_and_skips_bad_rows() {
        let json = r#"{"0":{"cik_str":320193,"ticker":"aapl","title":"Apple"},"1":{"cik_str":789019,"ticker":"MSFT","title":"Microsoft"},"2":{"ticker":"NOCIK"}}"#;
        let map = parse_ticker_map(json);
        assert_eq!(map.get("AAPL"), Some(&320_193));
        assert_eq!(map.get("MSFT"), Some(&789_019));
        assert_eq!(map.len(), 2);
        assert_eq!(parse_ticker_map("not json").len(), 0);
    }

    #[test]
    fn industry_comes_from_the_sic_code_and_description() {
        let json =
            r#"{"sic":"7372","sicDescription":"Services-Prepackaged Software","filings":{}}"#;
        assert_eq!(
            parse_industry(json).as_deref(),
            Some("7372 Services-Prepackaged Software")
        );
        assert_eq!(parse_industry(r#"{"sic":"","sicDescription":""}"#), None);
        assert_eq!(parse_industry(SUBMISSIONS), None);
    }

    const SUBMISSIONS: &str = r#"{"filings":{"recent":{
        "form":["8-K","144","4","10-Q","SC 13G","8-K"],
        "filingDate":["2026-11-05","2026-11-04","2026-11-03","2026-10-30","2026-10-01","2026-09-01"],
        "items":["2.02,9.01","","","","","5.02"],
        "primaryDocDescription":["8-K","144","Form 4","10-Q","SC 13G","8-K"],
        "accessionNumber":["0000320193-26-000001","a","b","c","d","e"],
        "primaryDocument":["x1.htm","x2.xml","x3.xml","x4.htm","x5.htm","x6.htm"]}}}"#;

    #[test]
    fn submissions_keep_relevant_forms_newest_first_and_respect_the_limit() {
        let all = parse_submissions(SUBMISSIONS, 10);
        assert_eq!(
            all.iter().map(|f| f.form.as_str()).collect::<Vec<_>>(),
            ["8-K", "4", "10-Q", "SC 13G", "8-K"]
        );
        assert_eq!(all[0].items, "2.02,9.01");
        assert_eq!(all[0].accession, "0000320193-26-000001");
        assert_eq!(all[0].document, "x1.htm");
        assert_eq!(parse_submissions(SUBMISSIONS, 2).len(), 2);
        assert_eq!(parse_submissions("{}", 5).len(), 0);
    }

    #[test]
    fn html_is_reduced_to_plain_text() {
        let html = "<html><head><style>p{color:red}</style><script>var a = '<b>';</script></head>\
            <body><p>Revenue &amp; profit&nbsp;rose</p>\n<P>  \"up\"  &quot;5%&quot; &#39;ok&#39; &lt;3 </P></body></html>";
        assert_eq!(
            strip_html(html),
            "Revenue & profit rose \"up\" \"5%\" 'ok' <3"
        );
        assert_eq!(strip_html("plain <unclosed"), "plain");
    }

    #[test]
    fn item_codes_have_meanings() {
        assert_eq!(item_meaning("2.02"), "earnings results");
        assert_eq!(item_meaning(" 5.02 "), "officer or director change");
        assert_eq!(item_meaning("9.9"), "other");
    }

    #[test]
    fn describe_builds_one_line() {
        let filings = parse_submissions(SUBMISSIONS, 10);
        assert_eq!(
            describe(&filings[0]),
            "2026-11-05 8-K: earnings results; exhibits"
        );
        assert_eq!(
            describe(&filings[1]),
            "2026-11-03 4: Form 4 filed (an insider transaction; direction not extracted)"
        );
        assert_eq!(describe(&filings[2]), "2026-10-30 10-Q: quarterly report");
        assert_eq!(
            describe(&filings[3]),
            "2026-10-01 SC 13G: large holder stake"
        );
    }

    #[test]
    fn truncation_respects_multibyte_characters() {
        assert_eq!(truncate("héllo wörld", 4), "héll");
        assert_eq!(truncate("日本語のテキスト", 3), "日本語");
        assert_eq!(truncate("short", 50), "short");
    }

    const FORM4: &str = r#"<?xml version="1.0"?><ownershipDocument>
        <issuer><issuerTradingSymbol>MAX</issuerTradingSymbol></issuer>
        <reportingOwner><reportingOwnerId><rptOwnerName>DOE JANE</rptOwnerName></reportingOwnerId>
          <reportingOwnerRelationship><isDirector>0</isDirector><isOfficer>1</isOfficer>
          <officerTitle>Chief Financial Officer</officerTitle></reportingOwnerRelationship></reportingOwner>
        <nonDerivativeTable>
          <nonDerivativeTransaction>
            <transactionDate><value>2026-11-03</value></transactionDate>
            <transactionCoding><transactionFormType>4</transactionFormType><transactionCode>S</transactionCode></transactionCoding>
            <transactionAmounts><transactionShares><value>10000</value></transactionShares>
              <transactionPricePerShare><value>12.5</value></transactionPricePerShare>
              <transactionAcquiredDisposedCode><value>D</value></transactionAcquiredDisposedCode></transactionAmounts>
          </nonDerivativeTransaction>
          <nonDerivativeTransaction>
            <transactionDate><value>2026-11-03</value></transactionDate>
            <transactionCoding><transactionCode>F</transactionCode></transactionCoding>
            <transactionAmounts><transactionShares><value>1234</value></transactionShares>
              <transactionPricePerShare><value>12.5</value></transactionPricePerShare></transactionAmounts>
          </nonDerivativeTransaction>
        </nonDerivativeTable></ownershipDocument>"#;

    #[test]
    fn a_form_4_is_read_for_direction_before_anyone_calls_it_buying() {
        let detail = parse_form4(FORM4).unwrap();
        assert_eq!(
            detail,
            "Chief Financial Officer DOE JANE: open-market sale of 10,000 shares at $12.50 on 2026-11-03; \
             shares withheld for tax of 1,234 shares at $12.50 on 2026-11-03"
        );
        assert!(!detail.contains("purchase"));
        let buy = FORM4.replace("<transactionCode>S<", "<transactionCode>P<");
        assert!(
            parse_form4(&buy)
                .unwrap()
                .contains("open-market purchase of 10,000 shares")
        );
        let grant = FORM4.replace("<transactionCode>S<", "<transactionCode>A<");
        assert!(
            parse_form4(&grant)
                .unwrap()
                .contains("grant or award (not a purchase)")
        );
        assert_eq!(parse_form4("<html>not a form</html>"), None);
        let empty = "<ownershipDocument><reportingOwner><rptOwnerName>X Y</rptOwnerName>\
            <isDirector>1</isDirector></reportingOwner></ownershipDocument>";
        assert_eq!(
            parse_form4(empty).unwrap(),
            "Director X Y: no share transaction reported (derivative grants or holdings only)"
        );
        // The dossier line carries the direction once it is known, and not before.
        let mut filing = Filing {
            form: "4".into(),
            date: "2026-11-04".into(),
            ..Filing::default()
        };
        assert!(describe(&filing).contains("direction not extracted"));
        filing.detail = detail;
        assert!(describe(&filing).starts_with("2026-11-04 4: Form 4: Chief Financial Officer"));
    }

    #[test]
    fn a_filing_is_read_from_its_substance_not_its_xbrl_header() {
        let html = r#"<html><body><ix:header><ix:hidden><ix:nonNumeric name="dei:EntityCentralIndexKey">0001818383</ix:nonNumeric>
            false --12-31 Q3 2026 0001818383</ix:hidden><ix:resources><xbrli:context id="c1">junk junk</xbrli:context></ix:resources></ix:header>
            <p>UNITED STATES SECURITIES AND EXCHANGE COMMISSION Washington, D.C. 20549 FORM 10-Q</p>
            <p>Table of contents: Item 2. Management's Discussion and Analysis of Financial Condition and Results of Operations 21</p>
            <p>Revenue <ix:nonFraction name="us-gaap:Revenues" contextRef="c1" scale="3" unitRef="usd">306,514</ix:nonFraction>
               prior <ix:nonFraction name="us-gaap:Revenues" contextRef="c0" scale="3">259,131</ix:nonFraction>
               Net loss <ix:nonFraction name="us-gaap:NetIncomeLoss" contextRef="c1" scale="3" sign="-">4,200</ix:nonFraction>
               EPS <ix:nonFraction name="us-gaap:EarningsPerShareDiluted" contextRef="c1" scale="0" sign="-">0.07</ix:nonFraction></p>
            <p>Item 2. Management's Discussion and Analysis of Financial Condition and Results of Operations.
               Revenue increased 18% on higher transaction value in the property and casualty vertical.</p>
            </body></html>"#;
        let text = readable(html, "10-Q", 600);
        assert!(
            text.starts_with("Figures as tagged in the filing (first value of each; the period is not checked): revenue $306.5M; net income -$4.2M; diluted EPS -0.07.\n"),
            "{text}"
        );
        assert!(text.contains("Revenue increased 18%"));
        assert!(!text.contains("junk"), "{text}");
        assert!(
            !text.contains("Washington"),
            "the cover page is skipped: {text}"
        );
        // An 8-K starts at its first item.
        let eight = "<html><body><p>UNITED STATES SECURITIES AND EXCHANGE COMMISSION FORM 8-K Check the appropriate box</p>\
            <p>Item 2.02 Results of Operations and Financial Condition. On November 5 the Company issued a press release.</p></body></html>";
        assert!(readable(eight, "8-K", 300).starts_with("Item 2.02 Results of Operations"));
        // Anything else is returned from the start, and the limit holds.
        assert_eq!(readable("<p>héllo wörld</p>", "SC 13G", 4), "héll");
    }

    #[test]
    fn the_sec_ticker_file_gives_the_registrant_name() {
        let json = r#"{"0":{"cik_str":1818383,"ticker":"max","title":"MediaAlpha, Inc."},"1":{"cik_str":1174610,"ticker":"SSG","title":"ProShares Trust"}}"#;
        let titles = parse_ticker_titles(json);
        assert_eq!(titles["MAX"], "MediaAlpha, Inc.");
        assert_eq!(titles["SSG"], "ProShares Trust");
        let edgar = Edgar {
            cik_by_ticker: parse_ticker_map(json),
            title_by_ticker: titles,
            agent: String::new(),
        };
        assert_eq!(
            edgar.registrant("max"),
            Some(("0001818383".to_string(), "MediaAlpha, Inc.".to_string()))
        );
        assert_eq!(edgar.registrant("NOPE"), None);
    }
}
