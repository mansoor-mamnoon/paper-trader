//! Rebuild an observation from saved provider files, so a day can be replayed exactly.
//!
//! A folder holds what the providers returned: `chart_<SYMBOL>.json` (Yahoo daily bars),
//! `rss_<SYMBOL>.xml` (the ticker news feed), the Nasdaq directory as header and row files
//! (`nasdaqlisted.header.txt`, `nasdaqlisted.rows.txt`, and the same for `otherlisted`) and
//! `screener.json` (market capitalisations). `tests/fixtures/incident-2026-11-10` is such a
//! folder, captured the evening the desk bought a 2x inverse fund.

use crate::data;
use crate::intel;
use crate::model::{Intel, Observation};
use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use std::collections::BTreeMap;
use std::path::Path;

/// Build the observation a run at `at` (epoch seconds) would have made from `dir`.
pub fn from_saved(dir: &Path, at: f64) -> Result<Observation> {
    let read = |name: &str| {
        std::fs::read_to_string(dir.join(name)).with_context(|| format!("reading {name}"))
    };
    let now = DateTime::<Utc>::from_timestamp(at as i64, 0).context("bad timestamp")?;
    let mut directory = BTreeMap::new();
    for (file, key) in [("nasdaqlisted", "Symbol"), ("otherlisted", "ACT Symbol")] {
        let text = format!(
            "{}{}",
            read(&format!("{file}.header.txt"))?,
            read(&format!("{file}.rows.txt"))?
        );
        data::parse_directory(&text, key, &mut directory)?;
    }
    let (market_caps, sectors) = data::parse_screener(&read("screener.json")?);
    let mut observation = Observation {
        at,
        universe: directory.values().cloned().collect(),
        intel: Intel {
            market_caps,
            sectors,
            ..Intel::default()
        },
        ..Observation::default()
    };
    let mut names: Vec<String> = std::fs::read_dir(dir)?
        .filter_map(|e| e.ok()?.file_name().into_string().ok())
        .collect();
    names.sort();
    for name in names {
        if let Some(symbol) = name
            .strip_prefix("chart_")
            .and_then(|n| n.strip_suffix(".json"))
        {
            let payload = serde_json::from_str(&read(&name)?)?;
            match data::parse_history(&payload, data::first_open_day(now)) {
                Ok(quote) => {
                    observation.quotes.insert(symbol.to_string(), quote);
                }
                Err(error) => {
                    observation
                        .errors
                        .insert(symbol.to_string(), error.to_string());
                }
            }
        } else if let Some(symbol) = name
            .strip_prefix("rss_")
            .and_then(|n| n.strip_suffix(".xml"))
        {
            let items = intel::parse_rss(&read(&name)?, 12);
            if !items.is_empty() {
                observation.intel.news.insert(symbol.to_string(), items);
            }
        }
    }
    Ok(observation)
}
