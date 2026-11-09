//! Regression coverage for parsing borrowed daily-history timestamp arrays.

use chrono::NaiveDate;
use paper_trader::data::parse_history;
use serde_json::{Value, json};

const START: i64 = 1_700_000_000;
const DAY: i64 = 86_400;

fn payload() -> Value {
    let timestamps: Vec<i64> = (0..50).rev().map(|i| START + i * DAY).collect();
    json!({"chart": {"error": null, "result": [{
        "meta": {"currency": "USD"},
        "timestamp": timestamps,
        "indicators": {"quote": [{
            "open": (0..50).map(|i| 100 + i).collect::<Vec<_>>(),
            "close": vec![110; 50],
            "volume": vec![1000; 50]
        }]},
        "events": {"dividends": {"event": {"date": START, "amount": 0.5}}}
    }]}})
}

fn today() -> NaiveDate {
    NaiveDate::from_ymd_opt(2030, 1, 1).unwrap()
}

#[test]
fn timestamps_preserve_order_alignment_and_payload() {
    let input = payload();
    let before = input.clone();
    let quote = parse_history(&input, today()).unwrap();
    assert_eq!(quote.bars.len(), 50);
    for (i, bar) in quote.bars.iter().enumerate() {
        assert_eq!(bar.at, (START + i as i64 * DAY) as f64);
        assert_eq!(bar.open, (149 - i) as f64);
        assert_eq!(bar.close, 110.0);
        assert_eq!(bar.volume, 1000.0);
    }
    assert_eq!(quote.actions.len(), 1);
    assert_eq!(quote.actions[0].kind, "dividends");
    assert_eq!(quote.actions[0].date, START as f64);
    assert_eq!(quote.actions[0].amount, Some(0.5));
    assert_eq!(input, before);
}

#[test]
fn missing_empty_and_non_array_timestamps_keep_the_same_error() {
    let mut input = payload();
    input["chart"]["result"][0]
        .as_object_mut()
        .unwrap()
        .remove("timestamp");
    assert_eq!(
        parse_history(&input, today()).unwrap_err().to_string(),
        "Fewer than 50 completed daily bars"
    );
    for stamps in [Value::Null, json!([]), json!(12), json!("bad"), json!({})] {
        input["chart"]["result"][0]["timestamp"] = stamps;
        assert_eq!(
            parse_history(&input, today()).unwrap_err().to_string(),
            "Fewer than 50 completed daily bars"
        );
    }
}

#[test]
fn nonnumeric_timestamps_skip_rows_without_shifting_prices() {
    let mut input = payload();
    let result = &mut input["chart"]["result"][0];
    result["timestamp"]
        .as_array_mut()
        .unwrap()
        .insert(0, json!("invalid"));
    result["indicators"]["quote"][0]["open"]
        .as_array_mut()
        .unwrap()
        .insert(0, json!(999));
    result["indicators"]["quote"][0]["close"] = json!(vec![110; 51]);
    result["indicators"]["quote"][0]["volume"] = json!(vec![1000; 51]);
    let quote = parse_history(&input, today()).unwrap();
    assert_eq!(quote.bars.len(), 50);
    assert_eq!(quote.bars.first().unwrap().open, 149.0);
    assert_eq!(quote.bars.last().unwrap().open, 100.0);
    input["chart"]["result"][0]["timestamp"][1] = json!(1e30);
    assert_eq!(
        parse_history(&input, today()).unwrap_err().to_string(),
        "Bad timestamp"
    );
}
