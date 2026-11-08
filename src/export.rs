//! Static dashboard snapshot: JSON state plus full CSV logs.

use crate::data::SOURCE;
use crate::ledger::{self, report};
use anyhow::Result;
use rusqlite::types::ValueRef;
use serde_json::{Value, json};
use std::fs;
use std::path::Path;

fn csv_cell(value: ValueRef) -> String {
    let text = match value {
        ValueRef::Null | ValueRef::Blob(_) => String::new(),
        ValueRef::Integer(i) => i.to_string(),
        ValueRef::Real(f) => {
            // Always print a decimal point so the column reads back as a float.
            let s = format!("{f}");
            if s.contains('.') || s.contains('e') || s.contains("inf") || s.contains("NaN") {
                s
            } else {
                format!("{s}.0")
            }
        }
        ValueRef::Text(t) => String::from_utf8_lossy(t).into_owned(),
    };
    if text.contains([',', '"', '\n', '\r']) {
        format!("\"{}\"", text.replace('"', "\"\""))
    } else {
        text
    }
}

fn write_atomic(target: &Path, body: &str) -> Result<()> {
    let temporary = target.with_extension("tmp");
    fs::write(&temporary, body)?;
    fs::rename(temporary, target)?;
    Ok(())
}

/// Export compatible state JSON, full CSV logs and universe and coverage metadata.
pub fn export(path: &str, output: &str, strategy: Option<&Value>) -> Result<Value> {
    let root = Path::new(output);
    fs::create_dir_all(root.join("api"))?;
    fs::create_dir_all(root.join("export"))?;
    let mut report = report(path)?;
    let db = ledger::connect(path)?;
    let state = ledger::load(&db)?;
    report["coverage"] = json!(state.coverage);
    report["data_errors"] = json!(state.data_errors);
    report["source"] = json!(SOURCE);
    if let Some(strategy) = strategy {
        report["strategy"] = strategy.clone();
    }
    report["benchmark_label"] = json!("SPY price-only (dividends excluded)");
    report["simulation"] = json!(
        "Prior completed daily bars; latest completed session open after signal; \
         missed runs skip sessions; not real-time execution"
    );
    let rules = report["rules"].as_object_mut().unwrap();
    rules.insert("position_cap".into(), json!(0.20));
    rules.insert("fee".into(), json!(crate::account::FEE));
    rules.insert("slippage".into(), json!(crate::account::SLIP));
    rules.insert("cycle_seconds".into(), json!(86400));
    report["health"]["age_seconds"] = json!((ledger::now() - state.last_at).max(0.0));
    let body = serde_json::to_string_pretty(&report)?;
    for name in ["state", "state.json"] {
        write_atomic(&root.join("api").join(name), &body)?;
    }
    fs::write(
        root.join("api/universe.json"),
        serde_json::to_string(&state.universe)?,
    )?;
    for table in ["fills", "decisions", "equity", "events"] {
        let mut stmt = db.prepare(&format!("SELECT * FROM {table} ORDER BY id"))?;
        let names: Vec<String> = stmt
            .column_names()
            .map(std::string::ToString::to_string)
            .collect();
        let mut out = names.join(",") + "\n";
        let mut cursor = stmt.query([])?;
        while let Some(row) = cursor.next()? {
            let cells: Vec<String> = (0..names.len())
                .map(|i| row.get_ref(i).map(csv_cell))
                .collect::<Result<_, _>>()?;
            out.push_str(&(cells.join(",") + "\n"));
        }
        fs::write(root.join("export").join(format!("{table}.csv")), out)?;
    }
    Ok(report)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn csv_quotes_commas_and_quotes() {
        assert_eq!(csv_cell(ValueRef::Text(b"a,b")), "\"a,b\"");
        assert_eq!(
            csv_cell(ValueRef::Text(b"say \"hi\"")),
            "\"say \"\"hi\"\"\""
        );
        assert_eq!(csv_cell(ValueRef::Integer(7)), "7");
        assert_eq!(csv_cell(ValueRef::Real(2.0)), "2.0");
        assert_eq!(csv_cell(ValueRef::Null), "");
    }

    #[test]
    fn export_writes_state_and_csv_files() {
        let dir = std::env::temp_dir().join(format!("pt-export-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        let db = dir.join("account.sqlite");
        let out = dir.join("site");
        let report = export(db.to_str().unwrap(), out.to_str().unwrap(), None).unwrap();
        assert_eq!(report["account"]["equity"], 1000);
        assert!(out.join("api/state").exists());
        assert!(out.join("export/fills.csv").exists());
        let header = fs::read_to_string(out.join("export/equity.csv")).unwrap();
        assert!(header.starts_with("id,at,equity"));
    }
}
