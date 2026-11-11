//! A per-security trace of the pipeline: for each stock that mattered today, whether it
//! passed identity validation, data validation, the hard gates, the evidence check, the
//! judge and the check before the fill, and why not where it did not.
//!
//! The point is to make a wrong decision visible as a failed stage with a reason code,
//! instead of leaving it inside a plausible paragraph.

use crate::account;
use crate::desk::Transcript;
use crate::gates;
use crate::ledger;
use crate::model::Observation;
use crate::security::Master;
use crate::strategy::Strategy;
use serde_json::{Value, json};
use std::collections::BTreeSet;
use std::fmt::Write as _;

/// How many gate-refused stocks, ranked by 20-day gain, are traced beside the finalists.
/// These are the ones a pure momentum ranking would have reached for first.
pub const REFUSED_SHOWN: usize = 20;

fn stage(pass: Option<bool>, detail: impl Into<String>) -> Value {
    json!({"status": match pass {
        Some(true) => "pass",
        Some(false) => "fail",
        None => "not_reached",
    }, "detail": detail.into()})
}

/// Trace one symbol through the deterministic stages. `transcript` and `ledger_rows` add
/// what the desk and the account did with it.
fn trace_symbol(
    symbol: &str,
    observation: &Observation,
    master: &Master,
    strategy: &Strategy,
    transcript: &Transcript,
    ledger_rows: &[Value],
) -> Value {
    let identity = master.identify(symbol);
    let instrument = gates::instrument(&identity, strategy.universe);
    let identity_stage = if identity.verified {
        stage(Some(true), identity.describe())
    } else {
        stage(
            Some(false),
            format!(
                "[{}] {}",
                gates::code::UNVERIFIED_SECURITY_IDENTITY,
                identity
                    .notes
                    .first()
                    .map_or("not resolved", String::as_str)
            ),
        )
    };
    let quote = observation.quotes.get(symbol);
    let reference = account::reference_bars(observation);
    let data = match quote {
        None => stage(Some(false), "no price history in this observation"),
        Some(q) => match gates::data_quality(&q.bars, &q.actions, reference) {
            Some(block) => stage(Some(false), block.text()),
            None => stage(
                Some(true),
                format!(
                    "{} bars, latest session present, no unexplained split-sized step",
                    q.bars.len()
                ),
            ),
        },
    };
    let gate = quote.and_then(|q| {
        account::entry_gate(
            symbol,
            q,
            f64::INFINITY,
            observation,
            master,
            strategy,
            observation.at,
        )
    });
    let gates_stage = match (&instrument, &gate) {
        (Some(block), _) | (None, Some(block)) => stage(Some(false), block.text()),
        (None, None) if quote.is_some() => stage(
            Some(true),
            "instrument, size, data, price, liquidity and event gates",
        ),
        _ => stage(None, ""),
    };
    let finalist = transcript.finalists.iter().find(|f| f["symbol"] == symbol);
    let evidence = match finalist {
        Some(f) => {
            let counts = &f["evidence"]["counts"];
            let count = |v: &Value| v.as_array().map_or(0, Vec::len);
            let flagged = count(&f["research"]["audit"]["problems"])
                + count(&f["research"]["audit"]["dropped_events"])
                + count(&f["bull_problems"])
                + count(&f["bear_problems"]);
            json!({"status": if flagged == 0 { "pass" } else { "flagged" },
                   "detail": format!(
                       "{} verified events, {} withheld by the news policy, {} unmatched or rejected; {flagged} model claims flagged",
                       counts["verified_events"], counts["withheld_by_news_policy"],
                       counts["quarantined"].as_u64().unwrap_or(0) + counts["rejected"].as_u64().unwrap_or(0))})
        }
        None => stage(None, "not a finalist"),
    };
    let in_list = |key: &str| {
        transcript.judge[key]
            .as_array()
            .into_iter()
            .flatten()
            .find(|x| x["symbol"] == symbol)
            .cloned()
    };
    let judge = if let Some(refused) = in_list("rejected") {
        stage(
            Some(false),
            format!(
                "proposed, then refused by the evidence check: [{}] {}",
                refused["code"].as_str().unwrap_or(""),
                refused["reason"].as_str().unwrap_or("")
            ),
        )
    } else if let Some(pick) = in_list("picks") {
        stage(
            Some(true),
            format!(
                "picked ({}; uncalibrated model score {:.2})",
                pick["basis"].as_str().unwrap_or("basis not stated"),
                pick["conviction"].as_f64().unwrap_or(0.0)
            ),
        )
    } else if let Some(veto) = in_list("avoid") {
        stage(
            Some(false),
            format!("vetoed: {}", veto["reason"].as_str().unwrap_or("")),
        )
    } else if finalist.is_some() {
        stage(Some(false), "passed over")
    } else {
        stage(None, "")
    };
    let mine: Vec<&Value> = ledger_rows
        .iter()
        .filter(|r| r["symbol"] == symbol)
        .collect();
    let find = |action: &str| mine.iter().find(|r| r["action"] == action);
    let prefill = if let Some(row) = find("CANCELLED") {
        stage(
            Some(false),
            format!(
                "queued buy cancelled: {}",
                row["reason"].as_str().unwrap_or("")
            ),
        )
    } else if find("BUY_FILLED").is_some() {
        stage(
            Some(true),
            "re-checked against every gate, then filled at the open",
        )
    } else if find("BUY_SIGNAL").is_some() {
        stage(
            Some(true),
            "buy queued; it is re-checked against every gate before it fills",
        )
    } else {
        stage(None, "no order")
    };
    json!({
        "symbol": symbol,
        "security": identity.describe(),
        "instrument_type": identity.instrument_type,
        "identity": identity_stage,
        "data": data,
        "gates": gates_stage,
        "evidence": evidence,
        "judge": judge,
        "prefill": prefill,
    })
}

/// The trace for one strategy's day: its finalists, anything it proposed, queued or
/// cancelled, and the strongest risers the gates refused.
pub fn trace(
    observation: &Observation,
    strategy: &Strategy,
    transcript: &Transcript,
    db_path: &str,
) -> Vec<Value> {
    let master = Master::new(observation);
    let ledger_rows = ledger::connect(db_path)
        .and_then(|db| {
            ledger::rows(
                &db,
                &format!(
                    "SELECT symbol, action, reason FROM decisions WHERE at = {} AND action IN \
                     ('CANCELLED','BUY_FILLED','BUY_SIGNAL','DESK_REJECT')",
                    observation.at
                ),
            )
        })
        .unwrap_or_default();
    let mut symbols: Vec<String> = Vec::new();
    let mut seen = BTreeSet::new();
    let mut add = |symbol: &str| {
        if !symbol.is_empty() && symbol != "ACCOUNT" && seen.insert(symbol.to_string()) {
            symbols.push(symbol.to_string());
        }
    };
    for f in &transcript.finalists {
        add(f["symbol"].as_str().unwrap_or(""));
    }
    for key in ["proposed_picks", "rejected"] {
        for x in transcript.judge[key].as_array().into_iter().flatten() {
            add(x["symbol"].as_str().unwrap_or(""));
        }
    }
    for row in &ledger_rows {
        add(row["symbol"].as_str().unwrap_or(""));
    }
    let mut refused: Vec<account::Screened> =
        account::screen(strategy, observation, observation.at)
            .into_iter()
            .filter(|r| r.gate.is_some() && r.candidate.momentum_20d > strategy.min_momentum)
            .collect();
    refused.sort_by(|a, b| {
        b.candidate
            .momentum_20d
            .partial_cmp(&a.candidate.momentum_20d)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    for row in refused.iter().take(REFUSED_SHOWN) {
        add(&row.candidate.symbol);
    }
    symbols
        .iter()
        .map(|s| trace_symbol(s, observation, &master, strategy, transcript, &ledger_rows))
        .collect()
}

/// The trace as plain text, for a terminal or a job log.
pub fn render(rows: &[Value]) -> String {
    let mut out = String::new();
    for row in rows {
        let _ = writeln!(out, "{}", row["security"].as_str().unwrap_or(""));
        for key in ["identity", "data", "gates", "evidence", "judge", "prefill"] {
            let status = row[key]["status"].as_str().unwrap_or("");
            if status == "not_reached" {
                continue;
            }
            let _ = writeln!(
                out,
                "  {key:<9}{:<8}{}",
                status.to_uppercase(),
                row[key]["detail"].as_str().unwrap_or("")
            );
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Bar, Intel, Quote};

    fn observation() -> Observation {
        let bars = |step: f64| -> Vec<Bar> {
            (0..60)
                .map(|i| Bar {
                    at: 1_700_000_000.0 + 86400.0 * f64::from(i),
                    open: 20.0 + step * f64::from(i),
                    close: 20.0 + step * f64::from(i),
                    volume: 1e6,
                })
                .collect()
        };
        let listed = |symbol: &str, name: &str, etf: bool| json!({"symbol": symbol, "name": name, "exchange": "Q", "etf": etf});
        Observation {
            at: 1_700_000_000.0 + 86400.0 * 59.5,
            universe: vec![
                listed("RAMZ", "T-REX 2X Inverse DRAM Daily Target ETF", true),
                listed("GOOD", "Good Corp. - Common Stock", false),
                listed("TINY", "Tiny Corp. - Common Stock", false),
            ],
            quotes: [("RAMZ", 0.5), ("GOOD", 0.3), ("TINY", 0.4), ("MYST", 0.2)]
                .iter()
                .map(|(s, step)| {
                    (
                        (*s).to_string(),
                        Quote {
                            bars: bars(*step),
                            ..Quote::default()
                        },
                    )
                })
                .collect(),
            intel: Intel {
                market_caps: [("GOOD", 9e9), ("TINY", 3e8)]
                    .iter()
                    .map(|(s, c)| ((*s).to_string(), *c))
                    .collect(),
                ..Intel::default()
            },
            ..Observation::default()
        }
    }

    #[test]
    fn every_refused_riser_is_traced_with_the_stage_and_code_that_stopped_it() {
        let obs = observation();
        let transcript = Transcript {
            finalists: vec![
                json!({"symbol": "GOOD", "evidence": {"counts": {"verified_events": 0,
                "withheld_by_news_policy": 2, "quarantined": 1, "rejected": 0}},
                "research": {"audit": {"problems": ["UNSUPPORTED: x"], "dropped_events": []}}}),
            ],
            judge: json!({"picks": [], "avoid": [], "proposed_picks": [{"symbol": "GOOD"}],
                "rejected": [{"symbol": "GOOD", "code": "UNSUPPORTED_CLAIM", "reason": "no event"}]}),
            ..Transcript::default()
        };
        let rows = trace(&obs, &Strategy::default(), &transcript, ":memory:");
        let row = |s: &str| rows.iter().find(|r| r["symbol"] == s).unwrap();
        assert_eq!(rows[0]["symbol"], "GOOD");
        assert_eq!(row("GOOD")["identity"]["status"], "pass");
        assert_eq!(row("GOOD")["gates"]["status"], "pass");
        assert_eq!(row("GOOD")["evidence"]["status"], "flagged");
        assert_eq!(row("GOOD")["judge"]["status"], "fail");
        assert!(
            row("GOOD")["judge"]["detail"]
                .as_str()
                .unwrap()
                .contains("[UNSUPPORTED_CLAIM]")
        );
        assert_eq!(
            row("RAMZ")["identity"]["status"],
            "pass",
            "it is identified, as what it is"
        );
        assert!(
            row("RAMZ")["gates"]["detail"]
                .as_str()
                .unwrap()
                .starts_with("[LEVERAGED_INVERSE_ETF]")
        );
        assert_eq!(row("RAMZ")["judge"]["status"], "not_reached");
        assert!(
            row("TINY")["gates"]["detail"]
                .as_str()
                .unwrap()
                .starts_with("[SMALL_CAP]")
        );
        assert_eq!(row("MYST")["identity"]["status"], "fail");
        assert!(
            row("MYST")["gates"]["detail"]
                .as_str()
                .unwrap()
                .starts_with("[UNVERIFIED_SECURITY_IDENTITY]")
        );
        let text = render(&rows);
        assert!(
            text.contains("RAMZ = T-REX 2X Inverse DRAM Daily Target ETF [leveraged inverse ETF]")
        );
        assert!(
            text.contains("  gates    FAIL    [LEVERAGED_INVERSE_ETF]"),
            "{text}"
        );
    }
}
