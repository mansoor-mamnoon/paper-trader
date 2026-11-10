//! Data types shared by the stock and crypto accounts. The serialized form of `State`
//! is the JSON body stored in the ledger, and unknown fields round-trip untouched.

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

/// A news headline attached to a stock.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Headline {
    pub title: String,
    #[serde(default)]
    pub published: String,
}

/// One SEC filing, from the EDGAR submissions feed.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Filing {
    /// Form type, for example `8-K`, `10-Q`, `4`.
    pub form: String,
    /// Filing date, `YYYY-MM-DD`.
    pub date: String,
    /// Comma-separated 8-K item codes, for example `2.02,9.01`. Empty for other forms.
    #[serde(default)]
    pub items: String,
    #[serde(default)]
    pub description: String,
    pub accession: String,
    /// Primary document file name inside the accession folder.
    #[serde(default)]
    pub document: String,
}

/// Earnings dates and headlines gathered for one scan.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Intel {
    /// Symbol to the next report date, `YYYY-MM-DD`.
    #[serde(default)]
    pub earnings: BTreeMap<String, String>,
    #[serde(default)]
    pub news: BTreeMap<String, Vec<Headline>>,
    /// Recent SEC filings per symbol, newest first.
    #[serde(default)]
    pub filings: BTreeMap<String, Vec<Filing>>,
    /// Symbol to its most recent report date in the last seven days, `YYYY-MM-DD`.
    #[serde(default)]
    pub reported: BTreeMap<String, String>,
    /// Symbol to its SEC industry, `"7372 Services-Prepackaged Software"`.
    #[serde(default)]
    pub industries: BTreeMap<String, String>,
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
    #[serde(default)]
    pub intel: Intel,
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
    /// SEC industry at entry, for the industry cap. Empty when unknown.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub industry: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Order {
    pub side: String,
    pub at: f64,
    #[serde(default)]
    pub reason: String,
    /// Share of the position cap this buy may use, 0 to 1: the regime and the volatility
    /// and industry gates set it, never a model.
    #[serde(default = "full_size")]
    pub size_fraction: f64,
    /// SEC industry of a buy, for the industry cap. Empty when unknown.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub industry: String,
}

fn full_size() -> f64 {
    1.0
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
