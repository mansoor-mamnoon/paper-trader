use paper_trader::edgar::{Edgar, describe};
fn main() {
    let edgar = Edgar::load().expect("ticker map");
    let today = chrono::Utc::now().date_naive();
    for sym in ["AAPL", "ACN", "BRK.B"] {
        let f = edgar.recent_filings(sym, today - chrono::Duration::days(45), 6);
        println!("{sym}: {} filings", f.len());
        for x in &f {
            println!("  {}", describe(x));
        }
        if let Some(x) = f.iter().find(|x| x.form == "8-K") {
            let t = edgar
                .filing_text(sym, x, 400)
                .unwrap_or_else(|e| format!("ERR {e}"));
            println!("  text: {t}");
        }
    }
}
