//! Paper Trader: one trading day per run, decided by a desk of local language models.

use anyhow::Result;
use clap::Parser;
use paper_trader::desk::{self, Live};
use paper_trader::llm::{Brain, LocalBrain};
use paper_trader::models::{self, MODELS};
use paper_trader::store::{self, Store};
use paper_trader::{market, progress, site};
use serde_json::json;

#[derive(Parser)]
#[command(version, about)]
enum Cli {
    /// Run today's desk: settle the books, research, decide, queue orders, export the site
    Run {
        #[arg(long)]
        db: String,
        #[arg(long)]
        output: String,
        /// Run even if the latest session was already processed
        #[arg(long)]
        force: bool,
        /// Minutes the desk may spend before it stops arguing new candidates
        #[arg(long, default_value_t = 240.0)]
        minutes: f64,
        /// Run as of an earlier moment (RFC 3339 or epoch seconds): every source is cut there
        #[arg(long)]
        as_of: Option<String>,
    },
    /// Write the dashboard data from the database without running the desk
    Export {
        #[arg(long)]
        db: String,
        #[arg(long)]
        output: String,
    },
    /// Put the same desk questions to several models and compare speed and answers
    Bench {
        /// Comma-separated model ids; all registered models when empty
        #[arg(long, default_value = "")]
        models: String,
    },
}

fn main() -> Result<()> {
    match Cli::parse() {
        Cli::Run {
            db,
            output,
            force,
            minutes,
            as_of,
        } => run(&db, &output, force, minutes, as_of.as_deref()),
        Cli::Export { db, output } => {
            let store = Store::open(&db)?;
            println!("{}", site::export(&store, &output)?);
            Ok(())
        }
        Cli::Bench { models } => bench(&models),
    }
}

fn parse_time(text: &str) -> Result<f64> {
    if let Ok(seconds) = text.trim().parse::<f64>() {
        return Ok(seconds);
    }
    Ok(chrono::DateTime::parse_from_rfc3339(text.trim())?.timestamp() as f64)
}

fn run(db: &str, output: &str, force: bool, minutes: f64, as_of: Option<&str>) -> Result<()> {
    let store = Store::open(db)?;
    let cutoff = as_of.map(parse_time).transpose()?;
    let now = chrono::Utc::now();
    let settled = market::last_settled(now).timestamp() as f64;
    let state = store.load()?;
    if cutoff.is_none() && !force && state.engine == store::ENGINE && state.last_at >= settled {
        println!("The desk already ran after the latest session, so this run does nothing.");
        site::export(&store, output)?;
        return Ok(());
    }
    progress::enable(std::time::Duration::from_secs(60));
    let brain = match LocalBrain::from_env() {
        Ok(brain) => Some(brain),
        Err(error) => {
            eprintln!("No model runtime ({error}): the books are settled but nothing is decided");
            None
        }
    };
    let replay = cutoff.map(paper_trader::replay::AsOf::new);
    let feed: &dyn desk::Feed = match &replay {
        Some(feed) => feed,
        None => &Live,
    };
    let result = desk::run(
        &store,
        brain.as_ref().map(|b| b as &dyn Brain),
        feed,
        cutoff.unwrap_or_else(store::now),
        minutes,
    );
    drop(brain);
    let summary = site::export(&store, output)?;
    match result {
        Ok(report) => {
            println!(
                "{}",
                json!({
                    "day": report.day,
                    "focus": report.focus.sectors,
                    "decisions": report.decisions.iter().map(|d| format!("{} {}", d.kind, d.symbol)).collect::<Vec<_>>(),
                    "problems": report.problems,
                    "funnel": report.funnel,
                    "account": summary,
                })
            );
            Ok(())
        }
        Err(error) => {
            let _ = store.event(store::now(), "error", &error.to_string());
            Err(error)
        }
    }
}

fn bench(ids: &str) -> Result<()> {
    progress::enable(std::time::Duration::from_secs(60));
    let brain = LocalBrain::from_env()?;
    let chosen: Vec<&'static models::ModelSpec> = if ids.trim().is_empty() {
        MODELS.iter().collect()
    } else {
        ids.split(',')
            .filter_map(|id| models::by_id(id.trim()))
            .collect()
    };
    let questions = paper_trader::bench::questions();
    for spec in chosen {
        for (name, system, user, schema, check) in &questions {
            let started = std::time::Instant::now();
            let outcome = brain.ask_model(spec, system, user, schema);
            let seconds = started.elapsed().as_secs_f64();
            let line = match outcome {
                Ok((value, _, stats)) => json!({
                    "model": spec.id, "question": name, "seconds": seconds,
                    "prompt_tps": stats.prompt_per_second, "gen_tps": stats.tokens_per_second,
                    "tokens_out": stats.completion_tokens, "correct": check(&value), "answer": value,
                }),
                Err(error) => {
                    json!({"model": spec.id, "question": name, "error": error.to_string()})
                }
            };
            println!("{line}");
        }
        brain.stop();
    }
    Ok(())
}
