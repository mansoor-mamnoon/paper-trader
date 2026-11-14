//! Every number the account runs on, in one place.

/// The account started with this much simulated cash.
pub const START_CASH: f64 = 1000.0;
/// Fee charged on each side of a trade, as a share of its value.
pub const FEE: f64 = 0.001;
/// Price moved against the account on each side of a trade.
pub const SLIP: f64 = 0.002;
/// Orders smaller than this many dollars are not placed.
pub const MIN_ORDER: f64 = 1.0;
/// The book holds this many stocks. A sale is always paired with a purchase.
pub const MAX_POSITIONS: usize = 5;

/// A stock must be at least this large to be bought.
pub const MIN_MARKET_CAP: f64 = 1.0e9;
/// A stock must cost at least this much per share.
pub const MIN_PRICE: f64 = 3.0;
/// A stock must have traded at least this many dollars in the latest session.
pub const MIN_DOLLAR_VOLUME: f64 = 25.0e6;

/// Stocks kept by the first, price-and-attention pass of the buzz screen.
pub const PRESCREEN: usize = 60;
/// Most-buzzed stocks shown to each sector scout.
pub const SCOUT_NAMES: usize = 6;
/// Headlines shown to each sector scout.
pub const SCOUT_HEADLINES: usize = 16;
/// Sectors the strategist may focus the book on.
pub const MAX_FOCUS: usize = 3;
/// New stocks researched each day, drawn from the focus sectors.
pub const CANDIDATES: usize = 10;
/// Headlines kept per stock, newest first.
pub const HEADLINES_PER_STOCK: usize = 8;
/// A headline older than this many days is not shown to an agent.
pub const HEADLINE_DAYS: i64 = 4;
/// Headlines classified in one model call.
pub const CLASSIFY_BATCH: usize = 12;

/// Earlier newsbook entries a scout reads for its own sector.
pub const NEWSBOOK_MEMORY: usize = 5;
/// Earlier daily notes and decisions the manager reads.
pub const MANAGER_MEMORY: usize = 5;
/// Closed trades the manager reads.
pub const TRADE_MEMORY: usize = 8;

/// Days of model transcripts kept in the database. Each day is also written to a file.
pub const TRANSCRIPT_DAYS: i64 = 10;
/// Days of headlines kept in the database.
pub const HEADLINE_KEEP_DAYS: i64 = 30;
/// Days of full desk reports kept in the database.
pub const REPORT_DAYS: i64 = 60;
