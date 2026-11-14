//! The account database: one SQLite file holding the account state, every fill, the sector
//! newsbook, every headline and its label, the desk's journal, closed trades and model
//! transcripts. This is also the desk's memory: agents read earlier days from here.

use crate::config::{HEADLINE_KEEP_DAYS, REPORT_DAYS, START_CASH, TRANSCRIPT_DAYS};
use crate::llm::Call;
use crate::news::Headline;
use anyhow::Result;
use rusqlite::{Connection, params};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::path::Path;

/// The name written into the state once this engine has taken the account over.
pub const ENGINE: &str = "newsbook-1";

pub fn now() -> f64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0.0, |d| d.as_secs_f64())
}

/// One holding.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Position {
    pub symbol: String,
    pub quantity: f64,
    /// Average price paid per share, before the fee.
    pub entry: f64,
    /// Dollars paid in total, fees included.
    pub cost: f64,
    pub opened_at: f64,
    /// What the holding would fetch if sold at the last close, after costs.
    pub market_value: f64,
    pub unrealized_pnl: f64,
    /// The last close.
    pub mark: Option<f64>,
    pub price_at: Option<f64>,
    /// Newsbook sector id.
    pub sector: String,
    /// The analyst's case when the stock was bought.
    pub thesis: String,
    /// The catalyst label the analyst gave when the stock was bought.
    pub catalyst: String,
}

fn one() -> f64 {
    1.0
}

/// An order waiting for the next session's open.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Order {
    /// `BUY` or `SELL`.
    pub side: String,
    pub at: f64,
    pub reason: String,
    /// A buy's share of the cash available at the open, relative to the other buys.
    #[serde(default = "one")]
    pub weight: f64,
    pub sector: String,
    pub thesis: String,
    pub catalyst: String,
}

/// The whole account, stored as one JSON document.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct State {
    /// Which engine last wrote the state. Empty for the desk this one replaced.
    pub engine: String,
    pub cash: f64,
    pub positions: BTreeMap<String, Position>,
    pub pending: BTreeMap<String, Order>,
    pub started_at: Option<f64>,
    pub last_at: f64,
    pub peak: f64,
    pub max_drawdown: f64,
    /// Shares of SPY the starting cash would have bought. A yardstick only: never held.
    pub benchmark_quantity: f64,
    pub benchmark_value: Option<f64>,
    pub realized_pnl: f64,
    pub fees: f64,
    pub health: Value,
    /// Splits and dividends already applied, so none is applied twice.
    pub corporate_actions: Vec<String>,
}

impl Default for State {
    fn default() -> Self {
        State {
            engine: String::new(),
            cash: START_CASH,
            positions: BTreeMap::new(),
            pending: BTreeMap::new(),
            started_at: None,
            last_at: 0.0,
            peak: START_CASH,
            max_drawdown: 0.0,
            benchmark_quantity: 0.0,
            benchmark_value: None,
            realized_pnl: 0.0,
            fees: 0.0,
            health: json!({"status": "starting"}),
            corporate_actions: Vec::new(),
        }
    }
}

impl State {
    pub fn equity(&self) -> f64 {
        self.cash + self.positions.values().map(|p| p.market_value).sum::<f64>()
    }
}

/// One day's newsbook entry for one sector, written by its scout.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct SectorNote {
    pub day: String,
    pub sector: String,
    /// How much is happening, 0 to 10.
    pub heat: i64,
    /// `bullish`, `neutral` or `bearish`.
    pub outlook: String,
    /// What happened, in the scout's words.
    pub entry: String,
    /// The single biggest driver.
    pub driver: String,
}

/// A stored headline with the classifier's label.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Tagged {
    pub id: i64,
    /// The day the desk first saw it.
    pub day: String,
    pub title: String,
    pub source: String,
    pub published: f64,
    pub link: String,
    /// Empty until classified.
    pub label: String,
    /// 0 to 3. Meaningless until classified.
    pub buzz: i64,
}

/// One line of the desk's journal.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Entry {
    pub day: String,
    pub at: f64,
    /// A ticker, a sector id, or empty for the whole book.
    pub symbol: String,
    /// `FOCUS`, `KEEP`, `SELL`, `BUY`, `TOP_UP` or `NOTE`.
    pub kind: String,
    pub text: String,
}

/// A closed trade.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Trade {
    pub symbol: String,
    pub sector: String,
    pub catalyst: String,
    pub opened_at: f64,
    pub closed_at: f64,
    pub entry: f64,
    pub exit: f64,
    pub pnl: f64,
    pub return_pct: f64,
    pub thesis: String,
    pub exit_reason: String,
}

/// One execution.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Fill {
    pub at: f64,
    pub signal_at: f64,
    pub symbol: String,
    pub side: String,
    pub quantity: f64,
    pub price: f64,
    pub fee: f64,
    pub realized_pnl: f64,
    pub reason: String,
}

/// One point of the equity curve.
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct EquityPoint {
    pub at: f64,
    pub equity: f64,
    pub cash: f64,
    pub benchmark: f64,
}

/// The open database.
#[derive(Debug)]
pub struct Store {
    db: Connection,
}

impl Store {
    pub fn open(path: &str) -> Result<Self> {
        if path != ":memory:"
            && let Some(parent) = Path::new(path).parent()
            && !parent.as_os_str().is_empty()
        {
            std::fs::create_dir_all(parent)?;
        }
        let db = Connection::open(path)?;
        db.busy_timeout(std::time::Duration::from_secs(30))?;
        db.execute_batch(
            "PRAGMA synchronous=FULL;
            CREATE TABLE IF NOT EXISTS state (id INTEGER PRIMARY KEY CHECK(id=1), body TEXT NOT NULL);
            CREATE TABLE IF NOT EXISTS fills (
                id INTEGER PRIMARY KEY, at REAL, signal_at REAL, symbol TEXT, side TEXT,
                quantity REAL, price REAL, notional REAL, fee REAL, realized_pnl REAL, reason TEXT);
            CREATE TABLE IF NOT EXISTS equity (
                id INTEGER PRIMARY KEY, at REAL UNIQUE, equity REAL, cash REAL,
                benchmark REAL, drawdown REAL, planned_risk REAL);
            CREATE TABLE IF NOT EXISTS events (
                id INTEGER PRIMARY KEY, at REAL, kind TEXT, message TEXT);
            CREATE TABLE IF NOT EXISTS headlines (
                id INTEGER PRIMARY KEY, day TEXT, scope TEXT, title TEXT, source TEXT,
                published REAL, link TEXT, label TEXT NOT NULL DEFAULT '', buzz INTEGER NOT NULL DEFAULT 0,
                UNIQUE(scope, title));
            CREATE TABLE IF NOT EXISTS newsbook (
                id INTEGER PRIMARY KEY, day TEXT, sector TEXT, heat INTEGER, outlook TEXT,
                entry TEXT, driver TEXT, UNIQUE(day, sector));
            CREATE TABLE IF NOT EXISTS journal (
                id INTEGER PRIMARY KEY, day TEXT, at REAL, symbol TEXT, kind TEXT, text TEXT);
            CREATE TABLE IF NOT EXISTS calls (
                id INTEGER PRIMARY KEY, day TEXT, at REAL, agent TEXT, model TEXT, subject TEXT,
                system TEXT, user TEXT, response TEXT, ok INTEGER, prompt_tokens INTEGER,
                completion_tokens INTEGER, seconds REAL);
            CREATE TABLE IF NOT EXISTS trades (
                id INTEGER PRIMARY KEY, symbol TEXT, sector TEXT, catalyst TEXT, opened_at REAL,
                closed_at REAL, entry REAL, exit REAL, pnl REAL, return_pct REAL, thesis TEXT,
                exit_reason TEXT);
            CREATE TABLE IF NOT EXISTS reports (day TEXT PRIMARY KEY, at REAL, body TEXT);",
        )?;
        let has_speed: bool = db
            .prepare("SELECT 1 FROM pragma_table_info('calls') WHERE name='tokens_per_second'")?
            .exists([])?;
        if !has_speed {
            db.execute_batch(
                "ALTER TABLE calls ADD COLUMN prompt_per_second REAL NOT NULL DEFAULT 0;
                 ALTER TABLE calls ADD COLUMN tokens_per_second REAL NOT NULL DEFAULT 0;",
            )?;
        }
        db.execute(
            "INSERT OR IGNORE INTO state VALUES (1, ?)",
            params![serde_json::to_string(&State::default())?],
        )?;
        Ok(Store { db })
    }

    pub fn begin(&self) -> Result<()> {
        self.db.execute_batch("BEGIN IMMEDIATE")?;
        Ok(())
    }

    pub fn commit(&self) -> Result<()> {
        self.db.execute_batch("COMMIT")?;
        Ok(())
    }

    pub fn load(&self) -> Result<State> {
        let body: String = self
            .db
            .query_row("SELECT body FROM state WHERE id=1", [], |r| r.get(0))?;
        Ok(serde_json::from_str(&body)?)
    }

    pub fn save(&self, state: &State) -> Result<()> {
        self.db.execute(
            "UPDATE state SET body=? WHERE id=1",
            params![serde_json::to_string(state)?],
        )?;
        Ok(())
    }

    pub fn event(&self, at: f64, kind: &str, message: &str) -> Result<()> {
        self.db.execute(
            "INSERT INTO events(at,kind,message) VALUES(?,?,?)",
            params![at, kind, message],
        )?;
        Ok(())
    }

    /// The most recent events, newest first, as `(at, kind, message)`.
    pub fn events(&self, limit: usize) -> Result<Vec<(f64, String, String)>> {
        let mut stmt = self
            .db
            .prepare("SELECT at, kind, message FROM events ORDER BY id DESC LIMIT ?")?;
        let rows = stmt.query_map([limit as i64], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    pub fn fill(&self, fill: &Fill) -> Result<()> {
        self.db.execute(
            "INSERT INTO fills(at,signal_at,symbol,side,quantity,price,notional,fee,realized_pnl,reason) \
             VALUES(?,?,?,?,?,?,?,?,?,?)",
            params![
                fill.at,
                fill.signal_at,
                fill.symbol,
                fill.side,
                fill.quantity,
                fill.price,
                fill.quantity * fill.price,
                fill.fee,
                fill.realized_pnl,
                fill.reason
            ],
        )?;
        Ok(())
    }

    /// The most recent fills, newest first.
    pub fn fills(&self, limit: usize) -> Result<Vec<Fill>> {
        let mut stmt = self.db.prepare(
            "SELECT at, signal_at, symbol, side, quantity, price, fee, realized_pnl, reason \
             FROM fills ORDER BY id DESC LIMIT ?",
        )?;
        let rows = stmt.query_map([limit as i64], |r| {
            Ok(Fill {
                at: r.get(0)?,
                signal_at: r.get::<_, Option<f64>>(1)?.unwrap_or(0.0),
                symbol: r.get(2)?,
                side: r.get(3)?,
                quantity: r.get(4)?,
                price: r.get(5)?,
                fee: r.get::<_, Option<f64>>(6)?.unwrap_or(0.0),
                realized_pnl: r.get::<_, Option<f64>>(7)?.unwrap_or(0.0),
                reason: r.get::<_, Option<String>>(8)?.unwrap_or_default(),
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    pub fn equity_point(&self, point: &EquityPoint, drawdown: f64) -> Result<()> {
        self.db.execute(
            "INSERT OR REPLACE INTO equity(at,equity,cash,benchmark,drawdown,planned_risk) VALUES(?,?,?,?,?,0)",
            params![point.at, point.equity, point.cash, point.benchmark, drawdown],
        )?;
        Ok(())
    }

    /// The whole equity curve, oldest first.
    pub fn equity_curve(&self) -> Result<Vec<EquityPoint>> {
        let mut stmt = self
            .db
            .prepare("SELECT at, equity, cash, benchmark FROM equity ORDER BY at")?;
        let rows = stmt.query_map([], |r| {
            Ok(EquityPoint {
                at: r.get(0)?,
                equity: r.get(1)?,
                cash: r.get::<_, Option<f64>>(2)?.unwrap_or(0.0),
                benchmark: r.get::<_, Option<f64>>(3)?.unwrap_or(START_CASH),
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// Store headlines for a ticker or `sector:<id>`. One already stored keeps its first
    /// day and its label.
    pub fn remember_headlines(&self, day: &str, scope: &str, items: &[Headline]) -> Result<()> {
        for item in items {
            self.db.execute(
                "INSERT OR IGNORE INTO headlines(day,scope,title,source,published,link) VALUES(?,?,?,?,?,?)",
                params![day, scope, item.title, item.source, item.published, item.link],
            )?;
        }
        Ok(())
    }

    /// Headlines for `scope` first seen on or after `since_day`, newest first.
    pub fn headlines(&self, scope: &str, since_day: &str, limit: usize) -> Result<Vec<Tagged>> {
        let mut stmt = self.db.prepare(
            "SELECT id, day, title, source, published, link, label, buzz FROM headlines \
             WHERE scope=? AND day>=? ORDER BY day DESC, published DESC, id DESC LIMIT ?",
        )?;
        let rows = stmt.query_map(params![scope, since_day, limit as i64], |r| {
            Ok(Tagged {
                id: r.get(0)?,
                day: r.get(1)?,
                title: r.get(2)?,
                source: r.get::<_, Option<String>>(3)?.unwrap_or_default(),
                published: r.get::<_, Option<f64>>(4)?.unwrap_or(0.0),
                link: r.get::<_, Option<String>>(5)?.unwrap_or_default(),
                label: r.get(6)?,
                buzz: r.get(7)?,
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    pub fn label_headline(&self, id: i64, label: &str, buzz: i64) -> Result<()> {
        self.db.execute(
            "UPDATE headlines SET label=?, buzz=? WHERE id=?",
            params![label, buzz, id],
        )?;
        Ok(())
    }

    pub fn write_note(&self, note: &SectorNote) -> Result<()> {
        self.db.execute(
            "INSERT OR REPLACE INTO newsbook(day,sector,heat,outlook,entry,driver) VALUES(?,?,?,?,?,?)",
            params![note.day, note.sector, note.heat, note.outlook, note.entry, note.driver],
        )?;
        Ok(())
    }

    fn notes(&self, sql: &str, args: &[&dyn rusqlite::ToSql]) -> Result<Vec<SectorNote>> {
        let mut stmt = self.db.prepare(sql)?;
        let rows = stmt.query_map(args, |r| {
            Ok(SectorNote {
                day: r.get(0)?,
                sector: r.get(1)?,
                heat: r.get(2)?,
                outlook: r.get(3)?,
                entry: r.get(4)?,
                driver: r.get::<_, Option<String>>(5)?.unwrap_or_default(),
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// A sector's entries from before `day`, newest first.
    pub fn sector_history(&self, sector: &str, day: &str, limit: usize) -> Result<Vec<SectorNote>> {
        self.notes(
            "SELECT day, sector, heat, outlook, entry, driver FROM newsbook \
             WHERE sector=? AND day<? ORDER BY day DESC LIMIT ?",
            &[&sector, &day, &(limit as i64)],
        )
    }

    /// Every entry on or after `since_day`, newest day first.
    pub fn newsbook(&self, since_day: &str) -> Result<Vec<SectorNote>> {
        self.notes(
            "SELECT day, sector, heat, outlook, entry, driver FROM newsbook \
             WHERE day>=? ORDER BY day DESC, heat DESC, sector",
            &[&since_day],
        )
    }

    pub fn journal(&self, entry: &Entry) -> Result<()> {
        self.db.execute(
            "INSERT INTO journal(day,at,symbol,kind,text) VALUES(?,?,?,?,?)",
            params![entry.day, entry.at, entry.symbol, entry.kind, entry.text],
        )?;
        Ok(())
    }

    /// Forget a day's journal, so running the same day again does not repeat its lines.
    pub fn clear_journal(&self, day: &str) -> Result<()> {
        self.db
            .execute("DELETE FROM journal WHERE day=?", params![day])?;
        Ok(())
    }

    fn entries(&self, sql: &str, args: &[&dyn rusqlite::ToSql]) -> Result<Vec<Entry>> {
        let mut stmt = self.db.prepare(sql)?;
        let rows = stmt.query_map(args, |r| {
            Ok(Entry {
                day: r.get(0)?,
                at: r.get(1)?,
                symbol: r.get(2)?,
                kind: r.get(3)?,
                text: r.get(4)?,
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// The journal lines about one symbol from before `day`, newest first.
    pub fn journal_for(&self, symbol: &str, day: &str, limit: usize) -> Result<Vec<Entry>> {
        self.entries(
            "SELECT day, at, symbol, kind, text FROM journal WHERE symbol=? AND day<? \
             ORDER BY id DESC LIMIT ?",
            &[&symbol, &day, &(limit as i64)],
        )
    }

    /// The journal lines of one kind from before `day`, newest first.
    pub fn journal_kind(&self, kind: &str, day: &str, limit: usize) -> Result<Vec<Entry>> {
        self.entries(
            "SELECT day, at, symbol, kind, text FROM journal WHERE kind=? AND day<? \
             ORDER BY id DESC LIMIT ?",
            &[&kind, &day, &(limit as i64)],
        )
    }

    /// Every journal line on or after `since_day`, newest first.
    pub fn journal_since(&self, since_day: &str) -> Result<Vec<Entry>> {
        self.entries(
            "SELECT day, at, symbol, kind, text FROM journal WHERE day>=? ORDER BY id DESC",
            &[&since_day],
        )
    }

    pub fn record_calls(&self, day: &str, calls: &[Call]) -> Result<()> {
        for call in calls {
            self.db.execute(
                "INSERT INTO calls(day,at,agent,model,subject,system,user,response,ok,prompt_tokens,completion_tokens,seconds,prompt_per_second,tokens_per_second) \
                 VALUES(?,?,?,?,?,?,?,?,?,?,?,?,?,?)",
                params![
                    day,
                    call.at,
                    call.agent,
                    call.model,
                    call.subject,
                    call.system,
                    call.user,
                    call.response,
                    call.ok,
                    call.prompt_tokens as i64,
                    call.completion_tokens as i64,
                    call.seconds,
                    call.prompt_per_second,
                    call.tokens_per_second
                ],
            )?;
        }
        Ok(())
    }

    /// One day's model calls, in order.
    pub fn calls(&self, day: &str) -> Result<Vec<Call>> {
        let mut stmt = self.db.prepare(
            "SELECT at, agent, model, subject, system, user, response, ok, prompt_tokens, \
             completion_tokens, seconds, prompt_per_second, tokens_per_second FROM calls WHERE day=? ORDER BY id",
        )?;
        let rows = stmt.query_map([day], |r| {
            Ok(Call {
                at: r.get(0)?,
                agent: r.get(1)?,
                model: r.get(2)?,
                subject: r.get(3)?,
                system: r.get(4)?,
                user: r.get(5)?,
                response: r.get(6)?,
                ok: r.get(7)?,
                prompt_tokens: r.get::<_, i64>(8)? as u64,
                completion_tokens: r.get::<_, i64>(9)? as u64,
                seconds: r.get(10)?,
                prompt_per_second: r.get(11)?,
                tokens_per_second: r.get(12)?,
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    pub fn trade(&self, trade: &Trade) -> Result<()> {
        self.db.execute(
            "INSERT INTO trades(symbol,sector,catalyst,opened_at,closed_at,entry,exit,pnl,return_pct,thesis,exit_reason) \
             VALUES(?,?,?,?,?,?,?,?,?,?,?)",
            params![
                trade.symbol,
                trade.sector,
                trade.catalyst,
                trade.opened_at,
                trade.closed_at,
                trade.entry,
                trade.exit,
                trade.pnl,
                trade.return_pct,
                trade.thesis,
                trade.exit_reason
            ],
        )?;
        Ok(())
    }

    /// Closed trades, newest first.
    pub fn trades(&self, limit: usize) -> Result<Vec<Trade>> {
        let mut stmt = self.db.prepare(
            "SELECT symbol, sector, catalyst, opened_at, closed_at, entry, exit, pnl, return_pct, \
             thesis, exit_reason FROM trades ORDER BY id DESC LIMIT ?",
        )?;
        let rows = stmt.query_map([limit as i64], |r| {
            Ok(Trade {
                symbol: r.get(0)?,
                sector: r.get(1)?,
                catalyst: r.get(2)?,
                opened_at: r.get(3)?,
                closed_at: r.get(4)?,
                entry: r.get(5)?,
                exit: r.get(6)?,
                pnl: r.get(7)?,
                return_pct: r.get(8)?,
                thesis: r.get(9)?,
                exit_reason: r.get(10)?,
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// The account's own record by catalyst: `(label, trades, average return in percent)`.
    pub fn catalyst_record(&self) -> Result<Vec<(String, i64, f64)>> {
        let mut stmt = self.db.prepare(
            "SELECT catalyst, COUNT(*), AVG(return_pct) FROM trades WHERE catalyst<>'' \
             GROUP BY catalyst ORDER BY COUNT(*) DESC, catalyst",
        )?;
        let rows = stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    pub fn put_report(&self, day: &str, at: f64, body: &str) -> Result<()> {
        self.db.execute(
            "INSERT OR REPLACE INTO reports(day,at,body) VALUES(?,?,?)",
            params![day, at, body],
        )?;
        Ok(())
    }

    /// Stored desk reports, newest first, as `(day, body)`.
    pub fn reports(&self, limit: usize) -> Result<Vec<(String, String)>> {
        let mut stmt = self
            .db
            .prepare("SELECT day, body FROM reports ORDER BY day DESC LIMIT ?")?;
        let rows = stmt.query_map([limit as i64], |r| Ok((r.get(0)?, r.get(1)?)))?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// Drop transcripts, headlines and reports older than their keep windows, counted back
    /// from `day`, and compact the file.
    pub fn prune(&self, day: &str) -> Result<()> {
        let Ok(today) = chrono::NaiveDate::parse_from_str(day, "%Y-%m-%d") else {
            return Ok(());
        };
        let before = |days: i64| (today - chrono::Duration::days(days)).to_string();
        self.db
            .execute("DELETE FROM calls WHERE day<?", [before(TRANSCRIPT_DAYS)])?;
        self.db.execute(
            "DELETE FROM headlines WHERE day<?",
            [before(HEADLINE_KEEP_DAYS)],
        )?;
        self.db
            .execute("DELETE FROM reports WHERE day<?", [before(REPORT_DAYS)])?;
        self.db.execute_batch("VACUUM")?;
        Ok(())
    }

    /// Tables the previous desk kept that this one does not read.
    pub fn drop_legacy_tables(&self) -> Result<()> {
        self.db.execute_batch(
            "DROP TABLE IF EXISTS decisions; DROP TABLE IF EXISTS picks; DROP TABLE IF EXISTS forward;",
        )?;
        Ok(())
    }
}

/// The day `days` before `day`, both `YYYY-MM-DD`.
pub fn days_before(day: &str, days: i64) -> String {
    chrono::NaiveDate::parse_from_str(day, "%Y-%m-%d").map_or_else(
        |_| day.to_string(),
        |d| (d - chrono::Duration::days(days)).to_string(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_old_desks_state_loads_and_keeps_the_account() {
        let old = r#"{"fully_invested":true,"mode":"stocks-daily","cash":12.5,
            "positions":{"POOL":{"symbol":"POOL","quantity":0.5,"entry":216.4,"cost":113.1,"stop":207.0,
              "target":233.0,"opened_at":1794000000.0,"planned_risk":4.0,"market_value":112.4,
              "unrealized_pnl":-0.7,"industry":"5090 Wholesale"}},
            "pending":{"ROP":{"side":"SELL","at":1794605856.0,"reason":"doctor","size_fraction":1.0}},
            "cooldown":{},"started_at":1794077065.0,"last_at":1794605856.0,"peak":1003.0,
            "max_drawdown":0.02,"halted":false,"halt_reason":"","benchmark_quantity":1.5,
            "realized_pnl":5.09,"fees":1.78,"health":{"status":"ok"},"universe":[{"symbol":"A"}],
            "scan_cursor":7,"corporate_actions":["X"]}"#;
        let state: State = serde_json::from_str(old).unwrap();
        assert_eq!(state.engine, "");
        assert!((state.cash - 12.5).abs() < 1e-9);
        assert!((state.positions["POOL"].quantity - 0.5).abs() < 1e-9);
        assert_eq!(state.pending["ROP"].side, "SELL");
        assert!((state.pending["ROP"].weight - 1.0).abs() < 1e-9);
        assert!((state.benchmark_quantity - 1.5).abs() < 1e-9);
        assert!((state.equity() - 124.9).abs() < 1e-9);
        // Saving drops what the old desk kept and this one does not use.
        let saved = serde_json::to_string(&state).unwrap();
        assert!(!saved.contains("universe") && !saved.contains("scan_cursor"));
    }

    #[test]
    fn state_round_trips_through_the_database() {
        let store = Store::open(":memory:").unwrap();
        let mut state = store.load().unwrap();
        assert!((state.cash - START_CASH).abs() < 1e-9);
        state.cash = 3.0;
        state.engine = ENGINE.into();
        store.save(&state).unwrap();
        assert_eq!(store.load().unwrap(), state);
    }

    #[test]
    fn headlines_keep_their_first_day_and_label() {
        let store = Store::open(":memory:").unwrap();
        let item = Headline {
            title: "Acme wins contract".into(),
            source: "Reuters".into(),
            published: 10.0,
            link: String::new(),
        };
        store
            .remember_headlines("2026-11-12", "ACME", std::slice::from_ref(&item))
            .unwrap();
        let first = store.headlines("ACME", "2026-11-01", 10).unwrap();
        store.label_headline(first[0].id, "deal", 3).unwrap();
        store
            .remember_headlines("2026-11-13", "ACME", &[item])
            .unwrap();
        let again = store.headlines("ACME", "2026-11-01", 10).unwrap();
        assert_eq!(again.len(), 1);
        assert_eq!(again[0].day, "2026-11-12");
        assert_eq!((again[0].label.as_str(), again[0].buzz), ("deal", 3));
        assert!(
            store
                .headlines("ACME", "2026-11-13", 10)
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn the_newsbook_remembers_earlier_days() {
        let store = Store::open(":memory:").unwrap();
        for (day, heat) in [("2026-11-11", 3), ("2026-11-12", 8), ("2026-11-13", 5)] {
            store
                .write_note(&SectorNote {
                    day: day.into(),
                    sector: "clean".into(),
                    heat,
                    outlook: "bullish".into(),
                    entry: format!("entry {day}"),
                    driver: "bill".into(),
                })
                .unwrap();
        }
        let history = store.sector_history("clean", "2026-11-13", 5).unwrap();
        assert_eq!(history.len(), 2);
        assert_eq!(history[0].day, "2026-11-12");
        assert_eq!(store.newsbook("2026-11-12").unwrap().len(), 2);
    }

    #[test]
    fn the_journal_and_trades_are_read_back_newest_first() {
        let store = Store::open(":memory:").unwrap();
        for (day, kind) in [("2026-11-11", "KEEP"), ("2026-11-12", "SELL")] {
            store
                .journal(&Entry {
                    day: day.into(),
                    at: 1.0,
                    symbol: "ACME".into(),
                    kind: kind.into(),
                    text: "because".into(),
                })
                .unwrap();
        }
        let lines = store.journal_for("ACME", "2026-11-13", 5).unwrap();
        assert_eq!(lines[0].kind, "SELL");
        assert_eq!(store.journal_for("ACME", "2026-11-12", 5).unwrap().len(), 1);
        assert_eq!(
            store.journal_kind("KEEP", "2026-11-13", 5).unwrap().len(),
            1
        );
        store.clear_journal("2026-11-12").unwrap();
        assert_eq!(store.journal_since("2026-11-01").unwrap().len(), 1);
        for (catalyst, ret) in [("deal", 4.0), ("deal", -2.0), ("policy", 1.0)] {
            store
                .trade(&Trade {
                    symbol: "ACME".into(),
                    catalyst: catalyst.into(),
                    return_pct: ret,
                    ..Trade::default()
                })
                .unwrap();
        }
        let record = store.catalyst_record().unwrap();
        assert_eq!(record[0], ("deal".to_string(), 2, 1.0));
        assert_eq!(store.trades(2).unwrap().len(), 2);
    }

    #[test]
    fn pruning_keeps_recent_rows() {
        let store = Store::open(":memory:").unwrap();
        let call = Call {
            agent: "scout".into(),
            ok: true,
            ..Call::default()
        };
        store
            .record_calls("2026-10-01", std::slice::from_ref(&call))
            .unwrap();
        store.record_calls("2026-11-12", &[call]).unwrap();
        store.put_report("2026-11-12", 1.0, "{}").unwrap();
        store.prune("2026-11-13").unwrap();
        assert!(store.calls("2026-10-01").unwrap().is_empty());
        assert_eq!(store.calls("2026-11-12").unwrap().len(), 1);
        assert_eq!(store.reports(5).unwrap().len(), 1);
        assert_eq!(days_before("2026-11-13", 4), "2026-11-09");
    }
}
