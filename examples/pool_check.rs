//! Loads every role's model in turn and asks it a small question under a JSON schema.
//! Prints load time, answer, speed and the determinism check, so a new machine or a new
//! model pin can be checked in a few minutes.
//!
//! ```text
//! PAPER_TRADER_LLAMA_SERVER=/path/to/llama-server \
//! PAPER_TRADER_MODELS_DIR=/path/to/models cargo run --release --example pool_check
//! ```
use paper_trader::models::{Role, model_for};
use paper_trader::pool::{Pool, Router};
use serde_json::json;
use std::path::PathBuf;
use std::time::Instant;

fn main() {
    let server = std::env::var("PAPER_TRADER_LLAMA_SERVER").expect("PAPER_TRADER_LLAMA_SERVER");
    let dir = std::env::var("PAPER_TRADER_MODELS_DIR").expect("PAPER_TRADER_MODELS_DIR");
    let pool = Pool::new(PathBuf::from(dir), PathBuf::from(server), 8092, false);
    let schema = json!({"type":"object","properties":{
        "answer":{"type":"string","maxLength":200},
        "score":{"type":"number"}},"required":["answer","score"]});
    for role in Role::ALL {
        let spec = model_for(role);
        let started = Instant::now();
        let reply = pool.complete(
            role,
            "You are a careful stock analyst. Reply in JSON.",
            "A company raised its annual guidance after beating earnings, and the stock is up 8% this month. In one sentence, is that a reason to buy? Give a score from 0 to 10.",
            Some(&schema),
        );
        let secs = started.elapsed().as_secs_f64();
        match reply {
            Ok(c) => println!(
                "{:<12} {:<20} {:<12} {:>6.1}s total  gen {:>5.1} tok/s  prompt {:>6.1} tok/s\n   {}",
                role.name(),
                spec.id,
                spec.company,
                secs,
                c.gen_tps.unwrap_or(0.0),
                c.prompt_tps.unwrap_or(0.0),
                c.text.replace('\n', " ")
            ),
            Err(error) => println!(
                "{:<12} {:<20} FAILED after {secs:.1}s: {error}",
                role.name(),
                spec.id
            ),
        }
        for e in pool.take_events() {
            println!("   [{}] {} {} ({} ms)", e.kind, e.subject, e.detail, e.ms);
        }
    }
}
