//! Write the observation a run would have made from a folder of saved provider files, as
//! JSON for `paper-trader stocks --replay`.
//!
//! ```text
//! cargo run --example freeze_observation -- tests/fixtures/incident-2026-11-10 1794349987 out.json
//! ```

use paper_trader::replay;
use std::path::Path;

fn main() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let [dir, at, out] = args.as_slice() else {
        anyhow::bail!("usage: freeze_observation <folder> <epoch seconds> <output.json>");
    };
    let observation = replay::from_saved(Path::new(dir), at.parse()?)?;
    std::fs::write(out, serde_json::to_string(&observation)?)?;
    println!(
        "{} quotes, {} symbols with news, {} directory rows",
        observation.quotes.len(),
        observation.intel.news.len(),
        observation.universe.len()
    );
    Ok(())
}
