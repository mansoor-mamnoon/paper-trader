//! Fixed questions for comparing models on the desk's own work. Each has a known right
//! answer, so `paper-trader bench` reports accuracy beside speed.

use serde_json::{Value, json};

/// `(name, system, user, schema, check)`. The check returns the share of the answer that
/// is right, 0 to 1.
pub type Question = (&'static str, String, String, Value, fn(&Value) -> f64);

const HEADLINES: [(&str, &str); 8] = [
    ("Acme names Jane Smith chief executive officer", "noise"),
    (
        "Acme wins $4 billion Pentagon contract for AI targeting systems",
        "big_deal",
    ),
    (
        "Acme quarterly revenue jumps 60%, beats estimates and raises full-year outlook",
        "earnings_beat",
    ),
    ("FDA approves Acme's obesity pill", "approval"),
    (
        "Acme to present at the Barclays Global Technology Conference",
        "noise",
    ),
    (
        "Acme shares slump after SEC opens accounting probe",
        "bad_news",
    ),
    (
        "Retail traders pile into Acme as Reddit mentions triple and short sellers get squeezed",
        "retail_frenzy",
    ),
    (
        "Senate passes bill doubling tax credits for the products Acme makes",
        "policy",
    ),
];

fn classify_check(value: &Value) -> f64 {
    let labels = value["labels"].as_array().cloned().unwrap_or_default();
    let right = HEADLINES
        .iter()
        .enumerate()
        .filter(|(i, (_, want))| {
            labels
                .iter()
                .any(|l| l["n"].as_u64() == Some(*i as u64 + 1) && l["label"] == *want)
        })
        .count();
    right as f64 / HEADLINES.len() as f64
}

fn manager_check(value: &Value) -> f64 {
    let keep_good = value["GOOD"]["action"] == "KEEP";
    let sell_ceo = value["CEOX"]["action"] == "SELL";
    let sell_stale = value["STALE"]["action"] == "SELL";
    f64::from(u8::from(keep_good) + u8::from(sell_ceo) + u8::from(sell_stale)) / 3.0
}

fn pick_check(value: &Value) -> f64 {
    let picked: Vec<&str> = value["buys"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|b| b["symbol"].as_str())
        .collect();
    f64::from(u8::from(picked.contains(&"HYPE"))) * 0.5
        + f64::from(u8::from(picked.contains(&"LAW"))) * 0.5
}

/// The benchmark questions.
pub fn questions() -> Vec<Question> {
    let labels: Vec<&str> = crate::agents::CATALYSTS.iter().map(|(l, _)| *l).collect();
    let mut classify_user = String::from("COMPANY: Acme Corp (ticker ACME)\n\nLABELS:\n");
    for (label, meaning) in crate::agents::CATALYSTS {
        classify_user.push_str(&format!("  {label}: {meaning}\n"));
    }
    classify_user.push_str("\nHEADLINES:\n");
    for (i, (title, _)) in HEADLINES.iter().enumerate() {
        classify_user.push_str(&format!("  {}. {title}\n", i + 1));
    }
    classify_user.push_str("\nLabel all 8 headlines in order.");
    let item = json!({"type": "object", "properties": {
        "n": {"type": "integer", "minimum": 1, "maximum": 8},
        "label": {"type": "string", "enum": labels},
        "buzz": {"type": "integer", "minimum": 0, "maximum": 3}},
        "required": ["n", "label", "buzz"], "additionalProperties": false});
    let classify_schema = json!({"type": "object", "properties": {"labels": {
        "type": "array", "minItems": 8, "maxItems": 8, "items": item}},
        "required": ["labels"], "additionalProperties": false});
    let decision = json!({"type": "object", "properties": {
        "reason": {"type": "string", "minLength": 1, "maxLength": 260},
        "action": {"type": "string", "enum": ["KEEP", "SELL"]}},
        "required": ["reason", "action"], "additionalProperties": false});
    let review_schema = json!({"type": "object",
        "properties": {"GOOD": decision, "CEOX": decision, "STALE": decision},
        "required": ["GOOD", "CEOX", "STALE"], "additionalProperties": false});
    let review_user = "HOLDINGS:\n\
GOOD: owned 2 days, +3.1%. Bought because a $3B data-centre order was announced. Today: a second hyperscaler confirmed a similar order (big_deal, buzz 3). Reddit mentions 240 (day before 90). Volume 2.8x normal.\n\
CEOX: owned 6 days, -0.4%. Bought by the previous desk on a momentum ranking. Only news: the company named a new chief financial officer (noise). No Reddit chatter. Volume 0.9x normal.\n\
STALE: owned 9 days, +1.0%. Bought on a product launch that is now two weeks old. No headlines since. Reddit mentions 3 (day before 15). Volume 0.7x normal.\n\
CANDIDATES: HYPE (policy, buzz 3: Senate passed a bill funding its products; Reddit mentions up 4x).\n\
Decide every holding now: GOOD, CEOX, STALE. For each give the reason first, then KEEP or SELL.";
    let pick_schema = json!({"type": "object", "properties": {"buys": {
        "type": "array", "minItems": 2, "maxItems": 2, "items": {"type": "object", "properties": {
            "symbol": {"type": "string", "enum": ["HYPE", "CEO", "LAW", "POOL"]},
            "reason": {"type": "string", "minLength": 1, "maxLength": 240},
            "weight": {"type": "integer", "minimum": 1, "maximum": 3}},
            "required": ["symbol", "reason", "weight"], "additionalProperties": false}}},
        "required": ["buys"], "additionalProperties": false});
    let pick_user = "CANDIDATES YOU COULD BUY:\n\
HYPE: Analyst (upside 8/10, retail_frenzy): Reddit mentions tripled to 900 after a viral product demo; volume 4x normal; up 6% today. Skeptic (case stands).\n\
CEO: Analyst (upside 3/10, none): The only news is a new chief executive. Skeptic (case broken).\n\
LAW: Analyst (upside 7/10, policy): The House passed the grid bill that funds its transformers; the Senate votes next week. Skeptic (case stands).\n\
POOL: Analyst (upside 2/10, none): Opened a new pool-supply distribution centre. Skeptic (case broken).\n\
Money is available and it must all be invested. Choose exactly 2 different stocks to buy.";
    vec![
        (
            "classify",
            "You label news headlines for a trading desk that buys stocks on fresh buzz. Choose the one label that fits each headline and score its buzz from 0 to 3.".into(),
            classify_user,
            classify_schema,
            classify_check as fn(&Value) -> f64,
        ),
        (
            "review",
            "You manage a stock account that trades news buzz and is always fully invested. Keep a stock only if its catalyst is still alive. A stock with no fresh news and no buzz is a stock to sell. Do not keep a stock because of an executive change.".into(),
            review_user.into(),
            review_schema,
            manager_check,
        ),
        (
            "pick",
            "You manage a stock account that trades news buzz and is always fully invested. Buy the stocks with a fresh catalyst and growing excitement.".into(),
            pick_user.into(),
            pick_schema,
            pick_check,
        ),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn perfect_answers_score_one() {
        let labels: Vec<Value> = HEADLINES
            .iter()
            .enumerate()
            .map(|(i, (_, l))| json!({"n": i + 1, "label": l, "buzz": 1}))
            .collect();
        assert!((classify_check(&json!({"labels": labels})) - 1.0).abs() < 1e-9);
        let review = json!({"GOOD": {"action": "KEEP"}, "CEOX": {"action": "SELL"}, "STALE": {"action": "SELL"}});
        assert!((manager_check(&review) - 1.0).abs() < 1e-9);
        let pick = json!({"buys": [{"symbol": "LAW"}, {"symbol": "HYPE"}]});
        assert!((pick_check(&pick) - 1.0).abs() < 1e-9);
        assert_eq!(questions().len(), 3);
    }
}
