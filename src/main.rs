//! Paper Trader: a $1,000 simulated US-stock account that trades by fixed rules.

use anyhow::Result;
use clap::Parser;
use paper_trader::analyst::llm_from_env;
use paper_trader::edgar::Edgar;
use paper_trader::pool::{OneModel, Pool, Router};
use paper_trader::telemetry::{RunMeta, RunOutcome, Telemetry};
use paper_trader::{crypto, daily, export, ledger, progress};
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
        desk_minutes: std::env::var("PAPER_TRADER_DESK_MINUTES")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(120.0),
    };
    // Skip quietly when a redundant trigger fires after the day has already been done.
    if args.replay.is_none() {
        let hours: f64 = std::env::var("PAPER_TRADER_SKIP_WITHIN_HOURS")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(0.0);
        let last_at = ledger::connect(&args.db)
            .and_then(|db| ledger::load(&db))
            .map_or(0.0, |s| s.last_at);
        if hours > 0.0 && ledger::now() - last_at < hours * 3600.0 {
            println!(
                "The account already ran {:.1} hours ago, so this trigger does nothing.",
                (ledger::now() - last_at) / 3600.0
            );
            return Ok(());
        }
    }
    progress::enable(std::time::Duration::from_secs(60));
    progress::log(&format!(
        "stock run starting: desk budget {} min, batch {} tickers",
        options.desk_minutes, options.batch_size
    ));
    let single = llm_from_env();
    let one = single.as_deref().map(OneModel);
    let pool = std::env::var("PAPER_TRADER_LLAMA_SERVER")
        .ok()
        .map(|server| {
            let dir = std::env::var("PAPER_TRADER_MODELS_DIR").map_or_else(
                |_| {
                    std::path::PathBuf::from(std::env::var("HOME").unwrap_or_else(|_| ".".into()))
                        .join(".cache/paper-trader-llm/models")
                },
                std::path::PathBuf::from,
            );
            let port = std::env::var("PAPER_TRADER_LLM_PORT")
                .ok()
                .and_then(|p| p.parse().ok())
                .unwrap_or(8091);
            Pool::new(
                dir,
                std::path::PathBuf::from(server),
                port,
                std::env::var("PAPER_TRADER_PRUNE_MODELS").is_ok(),
            )
        });
    let router: Option<&dyn Router> = match (&pool, &one) {
        (Some(pool), _) => Some(pool),
        (None, Some(one)) => Some(one),
        (None, None) => None,
    };
    if router.is_none() {
        eprintln!(
            "No model configured (PAPER_TRADER_LLAMA_SERVER, PAPER_TRADER_LLM_URL or ANTHROPIC_API_KEY): trading by the quantitative ranking only"
        );
    }
    let edgar = match Edgar::load() {
        Ok(edgar) => Some(edgar),
        Err(error) => {
            eprintln!("SEC filings are off ({error}): the desk will work from headlines only");
            None
        }
    };
    let started = ledger::now();
    let meta = run_meta(options.desk_minutes);
    let run_id =
        std::env::var("GITHUB_RUN_ID").unwrap_or_else(|_| format!("local-{}", started as u64));
    let telemetry = Telemetry::open(
        &options
            .strategies
            .join("telemetry.sqlite")
            .to_string_lossy(),
        &run_id,
        &meta,
        started,
    )?;
    let summary = daily::run(&options, mode, router, edgar.as_ref(), &telemetry);
    let mut summary = match summary {
        Ok(summary) => summary,
        Err(error) => {
            telemetry.event("error", "run", &error.to_string());
            let _ = telemetry.finish(
                &RunOutcome {
                    status: "failed".into(),
                    notes: error.to_string(),
                    minutes_budget: options.desk_minutes,
                    ..RunOutcome::default()
                },
                ledger::now(),
            );
            let _ = ledger::failure(&args.db, &error.to_string());
            let _ = export::export(&args.db, &args.output, None);
            return Err(error);
        }
    };
    // "Nothing to decide" is normal when every slot is full, so it is not a degradation.
    let degraded: Vec<String> = summary["desk"]["degraded"]
        .as_array()
        .map(|a| {
            a.iter()
                .filter_map(|v| v.as_str().map(str::to_string))
                .filter(|d| !d.starts_with("nothing to decide"))
                .collect()
        })
        .unwrap_or_default();
    let used = (ledger::now() - started) / 60.0;
    telemetry.finish(
        &RunOutcome {
            status: if degraded.is_empty() {
                "ok".into()
            } else {
                "degraded".into()
            },
            minutes_used: used,
            minutes_budget: options.desk_minutes,
            degraded,
            notes: String::new(),
        },
        ledger::now(),
    )?;
    summary["telemetry"] = telemetry.summary_json();
    let report = export::export(&args.db, &args.output, Some(&summary))?;
    let _ = telemetry.export_csv(&std::path::Path::new(&args.output).join("export"));
    let markdown = telemetry.markdown_summary();
    match std::env::var("GITHUB_STEP_SUMMARY") {
        Ok(path) => {
            use std::io::Write;
            if let Ok(mut file) = std::fs::OpenOptions::new()
                .append(true)
                .create(true)
                .open(path)
            {
                let _ = writeln!(file, "{markdown}");
            }
        }
        Err(_) => eprintln!("{markdown}"),
    }
    println!(
        "{}",
        json!({
            "equity": report["account"]["equity"],
            "coverage": report["coverage"],
            "health": report["health"],
            "analyst": summary["analyst"],
            "minutes_used": used,
            "output": format!("{}/api/state", args.output),
        })
    );
    Ok(())
}

/// What machine this is and why it is running, for the telemetry store.
fn run_meta(desk_minutes: f64) -> RunMeta {
    let read = |path: &str| std::fs::read_to_string(path).unwrap_or_default();
    let cpu = read("/proc/cpuinfo")
        .lines()
        .find_map(|l| {
            l.strip_prefix("model name")
                .and_then(|r| r.split(':').nth(1))
                .map(|v| v.trim().to_string())
        })
        .or_else(|| {
            std::process::Command::new("sysctl")
                .args(["-n", "machdep.cpu.brand_string"])
                .output()
                .ok()
                .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        })
        .unwrap_or_default();
    let ram_mb = read("/proc/meminfo")
        .lines()
        .find_map(|l| l.strip_prefix("MemTotal:"))
        .and_then(|r| {
            r.split_whitespace()
                .next()
                .and_then(|n| n.parse::<u64>().ok())
        })
        .map_or_else(
            || {
                std::process::Command::new("sysctl")
                    .args(["-n", "hw.memsize"])
                    .output()
                    .ok()
                    .and_then(|o| {
                        String::from_utf8_lossy(&o.stdout)
                            .trim()
                            .parse::<u64>()
                            .ok()
                    })
                    .map_or(0, |bytes| bytes / 1024 / 1024)
            },
            |kb| kb / 1024,
        );
    let trigger = match std::env::var("GITHUB_EVENT_NAME").as_deref() {
        Ok("schedule") => "schedule",
        Ok("workflow_dispatch") => "manual",
        Ok("repository_dispatch") => "webhook",
        _ => "local",
    };
    RunMeta {
        trigger: trigger.into(),
        git_sha: std::env::var("GITHUB_SHA")
            .map_or_else(|_| "local".into(), |s| s.chars().take(7).collect()),
        cpu,
        cores: std::thread::available_parallelism().map_or(1, std::num::NonZero::get),
        ram_mb,
        desk_minutes,
    }
}
