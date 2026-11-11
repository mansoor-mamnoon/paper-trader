//! The editable part of the system: a bounded, versioned strategy.
//!
//! Hard limits (position count, position size, costs, the 60-day end) live in code and
//! are not part of a strategy. A strategy only moves values inside the bounds below,
//! and any proposal outside them is rejected by [`Strategy::validate`].

use anyhow::{Result, anyhow, bail};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

/// Five equal slots of 20% each, so a full book is 100% invested.
pub const MAX_POSITIONS: usize = 5;
pub const POSITION_CAP: f64 = 0.20;
pub const MAX_PLAYBOOK_CHARS: usize = 4000;

pub const DEFAULT_PLAYBOOK: &str = "You are the portfolio analyst for a $1,000 long-only US-stock paper account. \
Each day you receive stocks that already passed a quantitative screen, with recent headlines and \
upcoming earnings dates. Pick the stocks most likely to rise over the next one to three weeks. \
Prefer a clear, recent, specific catalyst (guidance raised, contract won, upgrade, product launch) \
and a trend that the news supports. Avoid stocks with a pending earnings report unless the setup \
is unusually strong, stocks whose news is mostly negative, legal trouble, dilution or a recent \
spike on thin news. Be selective: choosing no stock is a valid answer when nothing is convincing.";

/// Which instruments a strategy may buy. This is part of the experiment's definition and is
/// stated in the strategy, never left to a model's taste.
///
/// Leveraged ETFs, inverse ETFs, exchange-traded notes, unsupported classes and symbols
/// whose identity is not verified are refused by the hard gates under both settings; a
/// strategy for those products would need its own risk rules and does not exist.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Universe {
    /// Verified common stocks and ADRs. The default.
    #[default]
    Equities,
    /// The above plus unleveraged, long-only ETFs.
    EquitiesAndEtfs,
}

impl Universe {
    pub fn label(self) -> &'static str {
        match self {
            Universe::Equities => "equities",
            Universe::EquitiesAndEtfs => "equities_and_etfs",
        }
    }

    pub fn describe(self) -> &'static str {
        match self {
            Universe::Equities => "US-listed common stocks and ADRs with a verified identity",
            Universe::EquitiesAndEtfs => {
                "US-listed common stocks, ADRs and unleveraged ETFs with a verified identity"
            }
        }
    }

    /// Parse a setting such as `PAPER_TRADER_UNIVERSE=equities_and_etfs`.
    pub fn parse(text: &str) -> Option<Self> {
        match text.trim().to_lowercase().as_str() {
            "equities" => Some(Universe::Equities),
            "equities_and_etfs" => Some(Universe::EquitiesAndEtfs),
            _ => None,
        }
    }
}

/// The universe rules every strategy ran under before [`RULES_VERSION`] 2: any symbol in
/// the exchange directory, including leveraged and inverse ETFs. Kept as a label so old
/// ledger rows can be described accurately; no strategy can select it.
pub const LEGACY_UNIVERSE: &str = "all_listed_v1";
/// Version of the fixed trading rules a strategy runs under. 1: any listed symbol could be
/// bought. 2: the instrument and data gates, and the desk's evidence contract.
pub const RULES_VERSION: u32 = 2;

/// A named, bounded set of rules the account trades by.
#[allow(clippy::struct_excessive_bools)]
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Strategy {
    pub id: String,
    #[serde(default)]
    pub parent: Option<String>,
    #[serde(default)]
    pub created_at: f64,
    #[serde(default)]
    pub rationale: String,
    pub min_price: f64,
    pub min_dollar_volume: f64,
    pub min_momentum: f64,
    pub require_trend: bool,
    pub stop_pct: f64,
    pub target_pct: f64,
    pub earnings_blackout_days: u32,
    pub analyst_enabled: bool,
    pub shortlist_size: usize,
    pub playbook: String,
    /// How much of the investment desk runs: 1 single-shot analyst, 2 committee
    /// (triage, bull and bear, judge), 3 full desk (adds research agents with tools).
    #[serde(default = "default_depth")]
    pub desk_depth: u8,
    /// Stocks that reach the research and debate stages.
    #[serde(default = "default_finalists")]
    pub finalists: usize,
    /// The position doctor re-checks every holding each day.
    #[serde(default = "yes")]
    pub use_doctor: bool,
    /// Lessons from closed trades and the pick scorecard are shown to the judge.
    #[serde(default = "yes")]
    pub use_memory: bool,
    /// Which instruments may be bought. Not a tunable: the lab cannot change it.
    #[serde(default)]
    pub universe: Universe,
    /// The fixed-rules version this strategy last traded under. A stored strategy without
    /// the field traded under version 1.
    #[serde(default = "first_rules")]
    pub rules_version: u32,
}

fn default_depth() -> u8 {
    3
}
/// Every shortlisted stock the desk may research is a finalist: the upper bound.
pub const DEFAULT_FINALISTS: usize = 8;
fn default_finalists() -> usize {
    DEFAULT_FINALISTS
}
fn yes() -> bool {
    true
}
fn first_rules() -> u32 {
    1
}

impl Strategy {
    /// The depth that actually runs: 0 when the analyst is off.
    pub fn effective_depth(&self) -> u8 {
        if self.analyst_enabled {
            self.desk_depth.min(3)
        } else {
            0
        }
    }
}

impl Default for Strategy {
    fn default() -> Self {
        Strategy {
            id: "baseline".into(),
            parent: None,
            created_at: 0.0,
            rationale: "Original fixed rules: 20/50-day trend, 20-day momentum, liquidity floor."
                .into(),
            min_price: 5.0,
            min_dollar_volume: 1_000_000.0,
            min_momentum: 0.02,
            require_trend: true,
            stop_pct: 0.04,
            target_pct: 0.08,
            earnings_blackout_days: 0,
            analyst_enabled: true,
            shortlist_size: 25,
            playbook: DEFAULT_PLAYBOOK.into(),
            desk_depth: default_depth(),
            finalists: default_finalists(),
            use_doctor: true,
            use_memory: true,
            universe: Universe::Equities,
            rules_version: RULES_VERSION,
        }
    }
}

/// Parameter names a proposal may change, with their inclusive bounds.
pub const BOUNDS: &[(&str, f64, f64)] = &[
    ("min_price", 1.0, 50.0),
    ("min_dollar_volume", 200_000.0, 50_000_000.0),
    ("min_momentum", -0.05, 0.20),
    ("stop_pct", 0.02, 0.10),
    ("target_pct", 0.03, 0.30),
    ("earnings_blackout_days", 0.0, 10.0),
    ("shortlist_size", 5.0, 40.0),
    ("desk_depth", 0.0, 3.0),
    ("finalists", 1.0, 8.0),
];

impl Strategy {
    pub fn validate(&self) -> Result<()> {
        let values = [
            ("min_price", self.min_price),
            ("min_dollar_volume", self.min_dollar_volume),
            ("min_momentum", self.min_momentum),
            ("stop_pct", self.stop_pct),
            ("target_pct", self.target_pct),
            (
                "earnings_blackout_days",
                f64::from(self.earnings_blackout_days),
            ),
            ("shortlist_size", self.shortlist_size as f64),
            ("desk_depth", f64::from(self.desk_depth)),
            ("finalists", self.finalists as f64),
        ];
        for (name, value) in values {
            let (_, lo, hi) = BOUNDS.iter().find(|b| b.0 == name).copied().unwrap();
            if !value.is_finite() || value < lo || value > hi {
                bail!("{name} = {value} is outside [{lo}, {hi}]");
            }
        }
        if self.target_pct <= self.stop_pct {
            bail!("target_pct must exceed stop_pct");
        }
        if self.playbook.trim().is_empty() || self.playbook.chars().count() > MAX_PLAYBOOK_CHARS {
            bail!("playbook must be 1 to {MAX_PLAYBOOK_CHARS} characters");
        }
        if self.id.is_empty()
            || self.id.len() > 40
            || !self
                .id
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
        {
            bail!("invalid strategy id");
        }
        Ok(())
    }

    /// Return a copy with the named parameters changed, or an error if any is unknown,
    /// mistyped or out of bounds. Nothing is clamped silently.
    pub fn with_changes(&self, changes: &Map<String, Value>) -> Result<Strategy> {
        fn float(key: &str, value: &Value) -> Result<f64> {
            value
                .as_f64()
                .ok_or_else(|| anyhow!("{key} must be a number"))
        }
        fn whole(key: &str, value: &Value) -> Result<u32> {
            value
                .as_u64()
                .and_then(|v| u32::try_from(v).ok())
                .ok_or_else(|| anyhow!("{key} must be a whole number"))
        }
        fn flag(key: &str, value: &Value) -> Result<bool> {
            value
                .as_bool()
                .ok_or_else(|| anyhow!("{key} must be true or false"))
        }
        let mut next = self.clone();
        for (key, value) in changes {
            match key.as_str() {
                "min_price" => next.min_price = float(key, value)?,
                "min_dollar_volume" => next.min_dollar_volume = float(key, value)?,
                "min_momentum" => next.min_momentum = float(key, value)?,
                "stop_pct" => next.stop_pct = float(key, value)?,
                "target_pct" => next.target_pct = float(key, value)?,
                "earnings_blackout_days" => next.earnings_blackout_days = whole(key, value)?,
                "shortlist_size" => next.shortlist_size = whole(key, value)? as usize,
                "finalists" => next.finalists = whole(key, value)? as usize,
                "desk_depth" => {
                    next.desk_depth = u8::try_from(whole(key, value)?)
                        .map_err(|_| anyhow!("{key} is too large"))?;
                }
                "use_doctor" => next.use_doctor = flag(key, value)?,
                "use_memory" => next.use_memory = flag(key, value)?,
                "require_trend" => next.require_trend = flag(key, value)?,
                "analyst_enabled" => next.analyst_enabled = flag(key, value)?,
                other => bail!("unknown parameter {other}"),
            }
        }
        next.validate()?;
        Ok(next)
    }

    /// Fields that change what the account does, for diffing two strategies.
    pub fn diff(&self, other: &Strategy) -> Vec<String> {
        let a = serde_json::to_value(self).unwrap_or_default();
        let b = serde_json::to_value(other).unwrap_or_default();
        [
            "min_price",
            "min_dollar_volume",
            "min_momentum",
            "require_trend",
            "stop_pct",
            "target_pct",
            "earnings_blackout_days",
            "analyst_enabled",
            "shortlist_size",
            "playbook",
            "universe",
        ]
        .iter()
        .filter(|k| a[**k] != b[**k])
        .map(|k| (*k).to_string())
        .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn default_is_valid() {
        Strategy::default().validate().unwrap();
    }

    #[test]
    fn changes_inside_bounds_apply() {
        let changes = json!({"stop_pct": 0.05, "earnings_blackout_days": 3})
            .as_object()
            .unwrap()
            .clone();
        let next = Strategy::default().with_changes(&changes).unwrap();
        assert_eq!(next.earnings_blackout_days, 3);
        assert!((next.stop_pct - 0.05).abs() < 1e-12);
    }

    #[test]
    fn out_of_bounds_unknown_and_inverted_changes_are_rejected() {
        let s = Strategy::default();
        for bad in [
            json!({"stop_pct": 0.5}),
            json!({"leverage": 2}),
            json!({"target_pct": 0.03, "stop_pct": 0.05}),
            json!({"min_price": "ten"}),
        ] {
            assert!(s.with_changes(bad.as_object().unwrap()).is_err(), "{bad}");
        }
    }

    #[test]
    fn diff_lists_changed_fields() {
        let s = Strategy::default();
        let t = s
            .with_changes(json!({"stop_pct": 0.05}).as_object().unwrap())
            .unwrap();
        assert_eq!(s.diff(&t), vec!["stop_pct"]);
    }

    #[test]
    fn the_universe_is_explicit_and_no_proposal_can_change_it() {
        let s = Strategy::default();
        assert_eq!(s.universe, Universe::Equities);
        assert_eq!(s.rules_version, RULES_VERSION);
        for bad in [
            json!({"universe": "equities_and_etfs"}),
            json!({"rules_version": 1}),
        ] {
            assert!(s.with_changes(bad.as_object().unwrap()).is_err(), "{bad}");
        }
        assert_eq!(
            Universe::parse(" Equities_And_ETFs "),
            Some(Universe::EquitiesAndEtfs)
        );
        assert_eq!(Universe::parse("all"), None);
        assert_eq!(Universe::parse(LEGACY_UNIVERSE), None);
    }

    #[test]
    fn a_strategy_stored_before_the_universe_rule_reads_as_rules_version_one() {
        let mut stored = serde_json::to_value(Strategy::default()).unwrap();
        stored.as_object_mut().unwrap().remove("universe");
        stored.as_object_mut().unwrap().remove("rules_version");
        let old: Strategy = serde_json::from_value(stored).unwrap();
        assert_eq!(old.rules_version, 1);
        assert_eq!(old.universe, Universe::Equities);
    }
}
