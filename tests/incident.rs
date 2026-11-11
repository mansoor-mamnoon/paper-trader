//! The 10 November 2026 incident, replayed from the provider files saved that evening.
//!
//! That night the desk queued a buy of RAMZ, a 2x inverse fund, at a stated conviction of
//! 0.90, calling it "not a speculative or leveraged play" with "a tangible, recent
//! catalyst". These tests run the same inputs through the repaired pipeline, from the
//! scan to the order, and show that the decision can no longer be produced.

use paper_trader::account::{self, cycle_with, screen};
use paper_trader::analyst::{Llm, Pick, Review};
use paper_trader::desk::{self, Budget, Desk};
use paper_trader::evidence::MatchStatus;
use paper_trader::gates::code;
use paper_trader::ledger;
use paper_trader::model::Observation;
use paper_trader::pool::OneModel;
use paper_trader::security::{InstrumentType, Master};
use paper_trader::strategy::{Strategy, Universe};
use paper_trader::{diagnostics, replay};
use serde_json::Value;
use std::path::Path;

/// 10 November 2026, 22:33:07 UTC: the run that queued the buy.
const AT: f64 = 1_794_349_987.0;
/// The eight finalists the judge was shown that night.
const FINALISTS: [&str; 8] = ["OMH", "SOXS", "MAX", "SNDQ", "NVDS", "SSG", "RAMZ", "POOL"];

fn observation() -> Observation {
    replay::from_saved(Path::new("tests/fixtures/incident-2026-11-10"), AT).unwrap()
}

fn temp_db(name: &str) -> String {
    let path =
        std::env::temp_dir().join(format!("pt-incident-{name}-{}.sqlite", std::process::id()));
    for suffix in ["", "-wal", "-shm"] {
        let _ = std::fs::remove_file(format!("{}{suffix}", path.display()));
    }
    path.to_string_lossy().into_owned()
}

#[test]
fn the_saved_inputs_are_the_ones_the_desk_saw() {
    let obs = observation();
    let ramz = obs.quotes["RAMZ"].bars.last().unwrap();
    assert!((ramz.close - 24.11).abs() < 0.005, "{}", ramz.close);
    let rows = screen(&Strategy::default(), &obs, AT);
    let row = rows.iter().find(|r| r.candidate.symbol == "RAMZ").unwrap();
    // The dossier that night read "20d +35.8% | 5d +21.6% | volatility 11.4%/day".
    assert!((row.candidate.momentum_20d - 0.358).abs() < 0.001);
    assert!((row.candidate.momentum_5d - 0.216).abs() < 0.001);
    assert!((row.candidate.volatility_20d - 0.114).abs() < 0.001);
    assert_eq!(obs.quotes["RAMZ"].instrument_type.as_deref(), Some("ETF"));
}

#[test]
fn every_instrument_in_the_incident_is_identified_for_what_it_is() {
    let obs = observation();
    let master = Master::new(&obs);
    for (symbol, multiple, underlying) in [
        ("RAMZ", 2.0, Some("DRAM")),
        ("SSG", 2.0, None),
        ("SNDQ", 2.0, Some("SNDK")),
        ("SOXS", 3.0, None),
        ("NVDS", 1.5, Some("NVDA")),
        ("AAOZ", 2.0, Some("AAOI")),
    ] {
        let id = master.identify(symbol);
        assert_eq!(
            id.instrument_type,
            InstrumentType::LeveragedInverseEtf,
            "{symbol}"
        );
        assert_eq!(id.leverage_multiple, Some(multiple), "{symbol}");
        assert_eq!(id.underlying_symbol.as_deref(), underlying, "{symbol}");
        assert!(id.inverse && id.daily_reset && id.verified, "{symbol}");
    }
    for symbol in ["MAX", "POOL", "OMH", "REAL", "REPL"] {
        let id = master.identify(symbol);
        assert_eq!(id.instrument_type, InstrumentType::CommonStock, "{symbol}");
        assert!(id.verified, "{symbol}");
    }
}

#[test]
fn seven_of_the_eight_finalists_are_refused_before_any_model_is_asked() {
    let obs = observation();
    let rows = screen(&Strategy::default(), &obs, AT);
    let gate = |symbol: &str| {
        rows.iter()
            .find(|r| r.candidate.symbol == symbol)
            .unwrap()
            .gate
            .as_ref()
            .map(|g| g.code)
    };
    for symbol in ["RAMZ", "SOXS", "SNDQ", "NVDS", "SSG"] {
        assert_eq!(gate(symbol), Some(code::LEVERAGED_INVERSE_ETF), "{symbol}");
    }
    // A $32M company up 432% in a day, and an $851M one: small caps.
    assert_eq!(gate("OMH"), Some(code::SMALL_CAP));
    assert_eq!(gate("MAX"), Some(code::SMALL_CAP));
    assert_eq!(gate("POOL"), None);
    let brief = account::brief(
        &Strategy::default(),
        &obs,
        &paper_trader::model::State::default(),
    );
    for c in &brief.candidates {
        assert!(
            !FINALISTS.contains(&c.symbol.as_str()) || c.symbol == "POOL",
            "{}",
            c.symbol
        );
        assert_eq!(c.identity.instrument_type, InstrumentType::CommonStock);
        assert!(obs.intel.market_caps[&c.symbol] >= 2e9);
    }
    // Even the ETF-inclusive universe admits none of the geared funds.
    let with_etfs = Strategy {
        universe: Universe::EquitiesAndEtfs,
        ..Strategy::default()
    };
    let offered = account::brief(&with_etfs, &obs, &paper_trader::model::State::default());
    for symbol in ["RAMZ", "SOXS", "SNDQ", "NVDS", "SSG"] {
        assert!(
            offered.candidates.iter().all(|c| c.symbol != symbol),
            "{symbol}"
        );
    }
}

#[test]
fn the_australian_retailers_news_is_not_evidence_for_the_us_fund() {
    let obs = observation();
    let master = Master::new(&obs);
    let ssg = master.identify("SSG");
    let today = "2026-11-10".parse().unwrap();
    let records: Vec<_> = obs.intel.news["SSG"]
        .iter()
        .map(|h| paper_trader::evidence::news_record(&ssg, h, today))
        .collect();
    assert_eq!(records.len(), 2);
    let shaver = records
        .iter()
        .find(|r| r.excerpt.contains("Shaver Shop"))
        .unwrap();
    assert_eq!(shaver.status, MatchStatus::Rejected);
    assert!(shaver.note.contains("ASX:SSG"));
    assert!(records.iter().all(|r| !r.shown() && !r.verified_event()));
    // RAMZ had no feed items at all: nothing known, which is not "positive news".
    assert!(!obs.intel.news.contains_key("RAMZ"));
}

/// The judge's own answer that night, word for word where it matters.
fn original_review() -> Review {
    Review {
        picks: vec![Pick {
            symbol: "RAMZ".into(),
            conviction: 0.9,
            thesis: "RAMZ has shown strong momentum with a 20-day gain of +35.8% and a 5-day gain of +21.6%, driven by recent volume spikes (2.7x average) and a significant spike to $29.05. The news of strong market interest and recent gains supports a short-term bullish thesis.".into(),
            key_risk: "The absence of earnings or a clear fundamental catalyst makes the momentum unsustainable in a risk-off regime.".into(),
            ..Pick::default()
        }],
        source: "llm".into(),
        regime: Some("risk_off".into()),
        ..Review::default()
    }
}

const ORIGINAL_REASONING: &str = "In a risk-off regime, we prioritize stability and clear, recent catalysts. While several stocks show strong momentum, only RAMZ has a tangible, recent catalyst (strong price action and volume spikes) without being a speculative or leveraged play. The other candidates either have extreme volatility, no earnings, or are inverse/leveraged products.";

#[test]
fn the_original_decision_cannot_become_an_order() {
    let obs = observation();
    // 1. Handed straight to the account, as the old pipeline did: no order.
    let path = temp_db("order");
    cycle_with(
        &path,
        &obs,
        "stocks-replay",
        &Strategy::default(),
        Some(&original_review()),
    )
    .unwrap();
    let db = ledger::connect(&path).unwrap();
    let state = ledger::load(&db).unwrap();
    assert!(!state.pending.contains_key("RAMZ") && state.positions.is_empty());
    let rows = ledger::rows(
        &db,
        "SELECT action, reason FROM decisions WHERE symbol='RAMZ'",
    )
    .unwrap();
    assert_eq!(rows.len(), 1, "{rows:?}");
    assert_eq!(rows[0]["action"], "PASS");
    assert!(
        rows[0]["reason"]
            .as_str()
            .unwrap()
            .starts_with("Hard gate [LEVERAGED_INVERSE_ETF]")
    );
    assert!(
        rows[0]["reason"]
            .as_str()
            .unwrap()
            .contains("-2x the daily return of DRAM")
    );
    // 2. And if a case for RAMZ had reached the judge, the evidence check refuses it on
    //    three independent grounds: the instrument, the contradiction, the invented news.
    let rows = screen(&Strategy::default(), &obs, AT);
    let ramz = &rows
        .iter()
        .find(|r| r.candidate.symbol == "RAMZ")
        .unwrap()
        .candidate;
    let case = desk::build_case(&obs, ramz, &[]);
    assert_eq!(case.evidence.verified_events().len(), 0);
    let claims = paper_trader::evidence::audit(ORIGINAL_REASONING, &[], &case.context(), false);
    let notes: Vec<&str> = claims.iter().map(|c| c.note.as_str()).collect();
    assert!(
        notes
            .iter()
            .any(|n| n.contains("without being a speculative or leveraged")),
        "{notes:?}"
    );
    assert!(
        notes
            .iter()
            .any(|n| n.contains("technical observations, not a verified event")),
        "{notes:?}"
    );
    let mut review = original_review();
    let cases = std::collections::BTreeMap::from([("RAMZ".to_string(), case)]);
    desk::enforce(
        &mut review,
        ORIGINAL_REASONING,
        &Strategy::default(),
        &cases,
    );
    assert_eq!(review.picks.len(), 0);
    assert_eq!(review.rejected[0].code, code::LEVERAGED_INVERSE_ETF);
    assert!(
        review
            .no_trade_reason
            .unwrap()
            .starts_with("Insufficient evidence")
    );
}

/// A model that does everything it can to buy RAMZ, and failing that buys anything on an
/// invented catalyst at conviction 0.90.
#[derive(Debug)]
struct Reckless;
impl Llm for Reckless {
    fn name(&self) -> String {
        "reckless".into()
    }
    fn complete(&self, system: &str, user: &str, _: Option<&Value>) -> anyhow::Result<String> {
        let symbols: Vec<String> = user
            .lines()
            .filter_map(|l| {
                l.split(" = ")
                    .next()
                    .filter(|s| s.len() <= 5 && l.contains(" = "))
            })
            .map(str::to_string)
            .collect();
        Ok(if system.contains("You screen stocks") {
            let scores: Vec<String> = symbols
                .iter()
                .map(|s| format!(r#"{{"symbol":"{s}","score":8,"basis":"verified_event","evidence_ids":[],"why":"High 20-day performance and positive news."}}"#))
                .chain(std::iter::once(r#"{"symbol":"RAMZ","score":10,"basis":"verified_event","evidence_ids":[],"why":"positive news"}"#.to_string()))
                .collect();
            format!(r#"{{"scores":[{}]}}"#, scores.join(","))
        } else if system.contains("research analyst") {
            r#"{"thought":"enough","action":"finish","arg":0}"#.into()
        } else if system.contains("BULL analyst") {
            r#"{"hypothesis":"It rises.","why_forward":"Strong market interest is a catalyst.","evidence_ids":[],"assumptions":[],"falsifier":"It falls."}"#.into()
        } else if system.contains("BEAR analyst") {
            r#"{"challenge":"Volatile.","alternative":"Reversal.","downside_scenario":"Stop.","structural_risks":[],"contradicting_evidence_ids":[],"hypothesis_survives":"yes"}"#.into()
        } else if system.contains("portfolio manager") {
            let first = user
                .split("\"symbol\":\"")
                .nth(1)
                .and_then(|s| s.split('"').next())
                .unwrap_or("POOL");
            format!(
                r#"{{"reasoning":"Only RAMZ and {first} have a tangible, recent catalyst.","regime":"risk_off","market_view":"x","decision":"trade","picks":[{{"symbol":"RAMZ","basis":"event_driven","evidence_ids":[],"conviction":0.9,"thesis":"Strong momentum and positive news.","why_over_alternatives":"","why_not_cash":"","key_risk":"","invalidation":""}},{{"symbol":"{first}","basis":"event_driven","evidence_ids":[],"conviction":0.9,"thesis":"The news of strong market interest and recent gains supports a short-term bullish thesis.","why_over_alternatives":"","why_not_cash":"","key_risk":"","invalidation":""}}],"avoid":[],"no_trade_reason":""}}"#
            )
        } else {
            anyhow::bail!("unexpected stage")
        })
    }
}

#[test]
fn a_model_that_tries_to_repeat_the_incident_ends_in_no_trade() {
    let obs = observation();
    let strategy = Strategy::default();
    let brief = account::brief(&strategy, &obs, &paper_trader::model::State::default());
    assert!(!brief.candidates.is_empty());
    let llm = Reckless;
    let router = OneModel(&llm);
    let budget = Budget::new(30.0);
    let mut desk = Desk::new(&router, None, &obs, &budget);
    let (review, transcript) = desk.review(&strategy, &brief, None, &[]);
    // RAMZ was never offered, so the judge's pick of it is dropped as not on the list;
    // the other pick is refused by the evidence check.
    assert_eq!(review.picks.len(), 0, "{:?}", review.picks);
    assert_eq!(review.rejected.len(), 1);
    // Refused either for the invented catalyst, or, when the stock does have a real
    // announcement that the pick failed to cite, for being technical only in risk-off.
    assert!(
        ["UNSUPPORTED_CLAIM", "INSUFFICIENT_EVIDENCE"].contains(&review.rejected[0].code.as_str()),
        "{:?}",
        review.rejected
    );
    assert_eq!(transcript.judge["decision"], "no_trade");
    assert!(transcript.finalists.iter().all(|f| f["symbol"] != "RAMZ"));
    assert!(transcript.triage.iter().all(|t| t["symbol"] != "RAMZ"));
    for t in &transcript.triage {
        assert!(
            t["score"].as_f64().unwrap() <= desk::UNSUPPORTED_SCORE_CAP,
            "{t}"
        );
    }
    // The account records the refusal and the decision not to trade, and buys nothing.
    let path = temp_db("reckless");
    cycle_with(&path, &obs, "stocks-replay", &strategy, Some(&review)).unwrap();
    let db = ledger::connect(&path).unwrap();
    let state = ledger::load(&db).unwrap();
    assert_eq!(state.pending.len() + state.positions.len(), 0);
    let actions: Vec<String> = ledger::rows(
        &db,
        "SELECT action FROM decisions WHERE action IN ('NO_TRADE','DESK_REJECT','BUY_SIGNAL')",
    )
    .unwrap()
    .iter()
    .map(|r| r["action"].as_str().unwrap().to_string())
    .collect();
    assert_eq!(actions, ["DESK_REJECT", "NO_TRADE"]);
    // The diagnostic shows where each of the original finalists stopped.
    let trace = diagnostics::trace(&obs, &strategy, &transcript, &path);
    let text = diagnostics::render(&trace);
    let ramz = trace.iter().find(|r| r["symbol"] == "RAMZ").unwrap();
    assert_eq!(ramz["identity"]["status"], "pass");
    assert_eq!(ramz["gates"]["status"], "fail");
    assert_eq!(ramz["judge"]["status"], "not_reached");
    assert!(
        text.contains("[LEVERAGED_INVERSE_ETF]") && text.contains("[SMALL_CAP]"),
        "{text}"
    );
}
