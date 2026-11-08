//! SEC EDGAR filings: the recent 8-Ks, quarterly reports, insider forms and large-holder
//! stakes of a company, and the text of one filing on request. No key is needed, but the
//! SEC asks every client to identify itself and to stay under ten requests a second.

use crate::http;
use crate::model::Filing;
use anyhow::{Result, anyhow};
use chrono::{Duration as Days, NaiveDate};
use serde_json::Value;
use std::collections::{BTreeMap, VecDeque};
use std::sync::Mutex;
use std::time::Duration;

pub const USER_AGENT: &str =
    "paper-trader research (https://github.com/mansoor-mamnoon/paper-trader)";

const TICKER_URL: &str = "https://www.sec.gov/files/company_tickers.json";
const FORMS: [&str; 11] = [
    "8-K", "10-Q", "10-K", "4", "SC 13D", "SC 13G", "SC 13D/A", "SC 13G/A", "S-3", "424B5",
    "DEF 14A",
];

/// Ticker to CIK lookup, loaded once per run.
#[derive(Debug)]
pub struct Edgar {
    cik_by_ticker: BTreeMap<String, u64>,
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
        })
        .collect()
}

impl Edgar {
    pub fn load() -> Result<Self> {
        let body = http::get(TICKER_URL, USER_AGENT, Duration::from_secs(30))?;
        let cik_by_ticker = parse_ticker_map(&body);
        if cik_by_ticker.is_empty() {
            return Err(anyhow!("the SEC ticker file was empty or unreadable"));
        }
        Ok(Edgar { cik_by_ticker })
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
        let Some(cik) = self.cik(symbol) else {
            return Vec::new();
        };
        let url = format!("https://data.sec.gov/submissions/CIK{cik:010}.json");
        let Ok(body) = http::get(&url, USER_AGENT, Duration::from_secs(20)) else {
            return Vec::new();
        };
        parse_submissions(&body, usize::MAX)
            .into_iter()
            .filter(|f| f.date.parse::<NaiveDate>().is_ok_and(|d| d >= since))
            .take(limit)
            .collect()
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
        let html = http::get(&url, USER_AGENT, Duration::from_secs(30))?;
        Ok(truncate(&strip_html(&html), max_chars))
    }
}

/// Recent filings for many symbols, politely: four workers, a pause after each request.
pub fn gather_filings(
    edgar: &Edgar,
    symbols: &[String],
    today: NaiveDate,
    days: i64,
    per_symbol: usize,
) -> BTreeMap<String, Vec<Filing>> {
    let since = today - Days::days(days);
    let queue = Mutex::new(symbols.iter().cloned().collect::<VecDeque<_>>());
    let found = Mutex::new(BTreeMap::new());
    std::thread::scope(|scope| {
        for _ in 0..4 {
            scope.spawn(|| {
                while let Some(symbol) = queue.lock().unwrap().pop_front() {
                    let filings = edgar.recent_filings(&symbol, since, per_symbol);
                    std::thread::sleep(Duration::from_millis(120));
                    if !filings.is_empty() {
                        found.lock().unwrap().insert(symbol, filings);
                    }
                }
            });
        }
    });
    found.into_inner().unwrap()
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
        "4" => "insider transaction (Form 4)".to_string(),
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
            "2026-11-03 4: insider transaction (Form 4)"
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
}
