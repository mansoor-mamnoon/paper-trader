//! Paper Trader: a $1,000 simulated account run by a desk of local language models.
//!
//! Rust gathers the market, the news and the attention each stock is getting. Models read
//! it, keep a sector newsbook, argue each stock, and make every trading decision. Rust
//! then keeps the books: it fills orders at the next open and keeps the account invested.

pub mod agents;
pub mod bench;
pub mod broker;
pub mod buzz;
pub mod config;
pub mod desk;
pub mod http;
pub mod llm;
pub mod market;
pub mod models;
pub mod news;
pub mod progress;
pub mod sectors;
pub mod site;
pub mod store;
