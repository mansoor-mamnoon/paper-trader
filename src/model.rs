//! Data types shared by the stock and crypto accounts. The serialized form of `State`
//! matches the JSON body that the Python ledger stores, so both implementations can open
//! the same SQLite file.

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use std::collections::BTreeMap;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Bar {
    pub at: f64,
    pub open: f64,
    pub close: f64,
    pub volume: f64,
}

/// A split or cash distribution reported by the data provider.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Action {
    pub kind: String,
    pub date: f64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub amount: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub numerator: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub denominator: Option<f64>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Quote {
    pub bars: Vec<Bar>,
    #[serde(default)]
    pub actions: Vec<Action>,
}

/// One daily scan of the stock universe.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Observation {
    pub at: f64,
    #[serde(default)]
    pub universe: Vec<Value>,
    pub quotes: BTreeMap<String, Quote>,
    #[serde(default)]
    pub errors: BTreeMap<String, String>,
    #[serde(default)]
    pub next_cursor: u64,
    #[serde(default)]
    pub coverage: Map<String, Value>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Position {
    #[serde(default)]
    pub symbol: String,
    #[serde(default)]
    pub quantity: f64,
    #[serde(default)]
    pub entry: f64,
    #[serde(default)]
    pub cost: f64,
    #[serde(default)]
    pub stop: f64,
    #[serde(default)]
    pub target: f64,
    #[serde(default)]
    pub opened_at: f64,
    #[serde(default)]
    pub planned_risk: f64,
    #[serde(default)]
    pub market_value: f64,
    #[serde(default)]
    pub unrealized_pnl: f64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub price_at: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mark: Option<f64>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Order {
    pub side: String,
    pub at: f64,
    #[serde(default)]
    pub reason: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct State {
    pub mode: Option<String>,
    pub cash: f64,
    pub positions: BTreeMap<String, Position>,
    pub pending: BTreeMap<String, Order>,
    pub cooldown: BTreeMap<String, f64>,
    pub started_at: Option<f64>,
    pub last_at: f64,
    pub peak: f64,
    pub max_drawdown: f64,
    pub halted: bool,
    pub halt_reason: String,
    pub benchmark_quantity: f64,
    pub realized_pnl: f64,
    pub fees: f64,
    pub health: Value,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub benchmark_value: Option<f64>,
    #[serde(default)]
    pub scan_cursor: u64,
    #[serde(default)]
    pub corporate_actions: Vec<String>,
    #[serde(default)]
    pub scan_seen: BTreeMap<String, f64>,
    #[serde(default)]
    pub coverage: Map<String, Value>,
    #[serde(default)]
    pub universe: Vec<Value>,
    #[serde(default)]
    pub data_errors: BTreeMap<String, String>,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

impl Default for State {
    fn default() -> Self {
        State {
            mode: None,
            cash: 1000.0,
            positions: BTreeMap::new(),
            pending: BTreeMap::new(),
            cooldown: BTreeMap::new(),
            started_at: None,
            last_at: 0.0,
            peak: 1000.0,
            max_drawdown: 0.0,
            halted: false,
            halt_reason: String::new(),
            benchmark_quantity: 0.0,
            realized_pnl: 0.0,
            fees: 0.0,
            health: serde_json::json!({"status": "starting"}),
            benchmark_value: None,
            scan_cursor: 0,
            corporate_actions: Vec::new(),
            scan_seen: BTreeMap::new(),
            coverage: Map::new(),
            universe: Vec::new(),
            data_errors: BTreeMap::new(),
            extra: Map::new(),
        }
    }
}
