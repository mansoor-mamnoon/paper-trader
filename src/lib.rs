//! Paper Trader: a $1,000 simulated account for US stocks and a second one for crypto.
//!
//! Both accounts share one SQLite ledger format. A cycle takes an observation of the
//! market, records every decision with its evidence, and fills queued orders at a later
//! price with fees and slippage applied against the account.

pub mod account;
pub mod analyst;
pub mod crypto;
pub mod daily;
pub mod data;
pub mod desk;
pub mod edgar;
pub mod evidence;
pub mod export;
pub mod gates;
pub mod http;
pub mod intel;
pub mod lab;
pub mod ledger;
pub mod model;
pub mod models;
pub mod pool;
pub mod progress;
pub mod scorecard;
pub mod security;
pub mod strategy;
pub mod telemetry;
