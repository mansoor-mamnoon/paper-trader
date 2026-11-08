//! Paper Trader: a $1,000 simulated US-stock account that trades by fixed rules.

use anyhow::Result;
use clap::Parser;
use paper_trader::analyst::{Analyst, LlmAnalyst, llm_from_env};
use paper_trader::lab::{LlmProposer, Proposer};
use paper_trader::{crypto, daily, export, ledger};
use serde_json::json;
use std::path::PathBuf;

/// Paper-only $1,000 experiments: a daily US-stock account and an hourly crypto account.
#[derive(Parser)]
#[command(version, about)]
enum Cli {
    /// Run one US-stock cycle and export the static dashboard snapshot
    Stocks(Args),
    /// Run the separate crypto account (public Kraken data)
    #[command(subcommand)]
    Crypto(Crypto),
}

#[derive(clap::Subcommand)]
enum Crypto {
    /// Observe Kraken once and apply one live cycle
    Once {
        #[arg(long)]
        db: String,
    },
    /// Replay a JSON array of snapshots into a separate database
    Replay {
        #[arg(long)]
        db: String,
        #[arg(long)]
        input: PathBuf,
    },
    /// Print the account report as JSON
    Status {
        #[arg(long)]
        db: String,
    },
}

#[derive(clap::Args)]
struct Args {
    /// Persistent SQLite STOCK account; never a crypto DB
    #[arg(long)]
    db: String,
    /// Static output directory
    #[arg(long)]
    output: String,
    /// Rotating universe slice; use 20000 for the entire directory (many requests)
    #[arg(long, default_value_t = 800)]
    batch_size: usize,
    /// Explicit observation JSON; separate replay account
    #[arg(long)]
    replay: Option<PathBuf>,
    /// Folder holding the strategy registry and shadow accounts (default: next to --db)
    #[arg(long)]
    strategies: Option<PathBuf>,
}

fn crypto_main(command: Crypto) -> Result<()> {
    match command {
        Crypto::Once { db } => {
            ledger::reserve(
                &db,
                "live",
                &["live", "replay"],
                "Live and replay data must use separate databases",
            )?;
            let result =
                crypto::observe().and_then(|snapshot| crypto::cycle(&db, &snapshot, "live"));
            if let Err(error) = result {
                let _ = ledger::failure(&db, &error.to_string());
                return Err(error);
            }
            println!("{}", ledger::report(&db)?);
        }
        Crypto::Replay { db, input } => {
            let snapshots: Vec<crypto::Snapshot> =
                serde_json::from_str(&std::fs::read_to_string(input)?)?;
            ledger::reserve(
                &db,
                "replay",
                &["live", "replay"],
                "Live and replay data must use separate databases",
            )?;
            for snapshot in snapshots {
                if let Err(error) = crypto::cycle(&db, &snapshot, "replay") {
                    ledger::failure(&db, &error.to_string())?;
                }
            }
            println!("{}", ledger::report(&db)?);
        }
        Crypto::Status { db } => println!("{}", ledger::report(&db)?),
    }
    Ok(())
}

fn main() -> Result<()> {
    let args = match Cli::parse() {
        Cli::Stocks(args) => args,
        Cli::Crypto(command) => return crypto_main(command),
    };
    if args.batch_size < 1 {
        anyhow::bail!("--batch-size must be positive");
    }
    let mode = if args.replay.is_some() {
        "stocks-replay"
    } else {
        "stocks-daily"
    };
    let options = daily::Options {
        strategies: args.strategies.clone().unwrap_or_else(|| {
            std::path::Path::new(&args.db)
                .parent()
                .unwrap_or(std::path::Path::new("."))
                .join("strategies")
        }),
        db: args.db.clone(),
        batch_size: args.batch_size,
        replay: args.replay.clone(),
    };
    let llm = llm_from_env();
    let analyst = llm.as_deref().map(|llm| LlmAnalyst { llm });
    let proposer = llm.as_deref().map(|llm| LlmProposer { llm });
    if llm.is_none() {
        eprintln!(
            "No model configured (PAPER_TRADER_LLM_URL or ANTHROPIC_API_KEY): trading by the quantitative ranking only"
        );
    }
    let summary = daily::run(
        &options,
        mode,
        analyst.as_ref().map(|a| a as &dyn Analyst),
        proposer.as_ref().map(|p| p as &dyn Proposer),
    );
    let summary = match summary {
        Ok(summary) => summary,
        Err(error) => {
            let _ = ledger::failure(&args.db, &error.to_string());
            let _ = export::export(&args.db, &args.output, None);
            return Err(error);
        }
    };
    let report = export::export(&args.db, &args.output, Some(&summary))?;
    println!(
        "{}",
        json!({
            "equity": report["account"]["equity"],
            "coverage": report["coverage"],
            "health": report["health"],
            "analyst": summary["analyst"],
            "output": format!("{}/api/state", args.output),
        })
    );
    Ok(())
}
