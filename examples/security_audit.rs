//! Classify every record in saved Nasdaq directory files and print what the security
//! master makes of them. Run it after changing `security::parse_name`:
//!
//! ```text
//! cargo run --example security_audit -- nasdaqlisted.txt otherlisted.txt [SYMBOL ...]
//! ```

use paper_trader::data::parse_directory;
use paper_trader::security::{InstrumentType, resolve};
use std::collections::BTreeMap;

fn main() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let mut directory = BTreeMap::new();
    let mut wanted = Vec::new();
    for arg in &args {
        if std::path::Path::new(arg).is_file() {
            let key = if arg.contains("otherlisted") {
                "ACT Symbol"
            } else {
                "Symbol"
            };
            parse_directory(&std::fs::read_to_string(arg)?, key, &mut directory)?;
        } else {
            wanted.push(arg.to_uppercase());
        }
    }
    let mut counts: BTreeMap<String, (usize, usize)> = BTreeMap::new();
    let mut samples: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for (symbol, row) in &directory {
        let identity = resolve(symbol, Some(row), None, "audit");
        let key = format!("{:?}", identity.instrument_type);
        let entry = counts.entry(key.clone()).or_default();
        entry.0 += 1;
        entry.1 += usize::from(identity.verified);
        let list = samples.entry(key).or_default();
        if list.len() < 6
            || matches!(identity.instrument_type, InstrumentType::Other) && list.len() < 12
        {
            list.push(identity.describe());
        }
    }
    println!("{} directory records", directory.len());
    for (kind, (total, verified)) in &counts {
        println!("{kind:>22}: {total:>6} ({verified} verified from the name alone)");
        for line in &samples[kind] {
            println!("{:>24}{line}", "");
        }
    }
    for symbol in wanted {
        let identity = resolve(&symbol, directory.get(&symbol), None, "audit");
        println!("{}", serde_json::to_string_pretty(&identity)?);
    }
    Ok(())
}
