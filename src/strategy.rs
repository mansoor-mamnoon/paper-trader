//! The editable part of the system: a bounded, versioned strategy.
//!
//! Hard limits (position count, position size, drawdown halt, costs) live in code and
//! are not part of a strategy. A strategy only moves values inside the bounds below,
//! and any proposal outside them is rejected by [`Strategy::validate`].

use anyhow::{Result, anyhow, bail};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

pub const MAX_POSITIONS: usize = 3;
pub const POSITION_CAP: f64 = 0.20;
pub const MAX_PLAYBOOK_CHARS: usize = 4000;

pub const DEFAULT_PLAYBOOK: &str = "You are the portfolio analyst for a $1,000 long-only US-stock paper account. \
Each day you receive stocks that already passed a quantitative screen, with recent headlines and \
upcoming earnings dates. Pick the stocks most likely to rise over the next one to three weeks. \
Prefer a clear, recent, specific catalyst (guidance raised, contract won, upgrade, product launch) \
and a trend that the news supports. Avoid stocks with a pending earnings report unless the setup \
is unusually strong, stocks whose news is mostly negative, legal trouble, dilution or a recent \
spike on thin news. Be selective: choosing no stock is a valid answer when nothing is convincing.";

/// A named, bounded set of rules the account trades by.
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
}
