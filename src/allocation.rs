//! A standing SPY allocation invests capital left after stock orders at each session open.
use crate::{
    account::{FEE, SLIP},
    ledger,
    model::{Position, State},
};
use anyhow::Result;
use rusqlite::Connection;

pub const CORE: &str = "SPY";
pub fn stock_count(state: &State) -> usize {
    state.positions.len() - usize::from(state.fully_invested && state.positions.contains_key(CORE))
}

/// Raise only the cash required by a stock purchase, recording proportional cost basis.
pub fn fund(
    db: &Connection,
    state: &mut State,
    needed: f64,
    open: f64,
    at: f64,
    signal: f64,
) -> Result<()> {
    let Some(p) = state.positions.get_mut(CORE) else {
        return Ok(());
    };
    let price = open * (1.0 - SLIP);
    let quantity = ((needed - state.cash).max(0.0) / (price * (1.0 - FEE))).min(p.quantity);
    if quantity <= 0.0 {
        return Ok(());
    }
    let cost = p.cost * quantity / p.quantity;
    let fee = quantity * price * FEE;
    let proceeds = quantity * price - fee;
    p.quantity -= quantity;
    p.cost -= cost;
    p.market_value = p.quantity * price * (1.0 - FEE);
    let pnl = proceeds - cost;
    state.cash += proceeds;
    state.realized_pnl += pnl;
    state.fees += fee;
    if p.quantity < 1e-10 {
        state.positions.remove(CORE);
    }
    ledger::insert_fill(
        db,
        at,
        signal,
        CORE,
        "SELL",
        quantity,
        price,
        fee,
        pnl,
        "Standing allocation: fund stock orders",
    )?;
    Ok(())
}

pub fn sweep(db: &Connection, state: &mut State, open: f64, at: f64, signal: f64) -> Result<()> {
    if state.halted || state.cash < 0.000_001 {
        return Ok(());
    }
    let price = open * (1.0 + SLIP);
    let cost = state.cash;
    let quantity = cost / (price * (1.0 + FEE));
    let fee = quantity * price * FEE;
    let p = state
        .positions
        .entry(CORE.into())
        .or_insert_with(|| Position {
            symbol: CORE.into(),
            opened_at: at,
            industry: "Broad US equity allocation".into(),
            ..Position::default()
        });
    p.quantity += quantity;
    p.cost += cost;
    p.entry = p.cost / p.quantity / (1.0 + FEE);
    p.market_value = p.quantity * open * (1.0 - SLIP) * (1.0 - FEE);
    p.price_at = Some(at);
    state.cash = 0.0;
    state.fees += fee;
    ledger::insert_fill(
        db,
        at,
        signal,
        CORE,
        "BUY",
        quantity,
        price,
        fee,
        0.0,
        "Standing allocation: invest remaining capital",
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn all_cash_is_invested_and_stock_funding_preserves_cost_basis() {
        let db = ledger::connect(":memory:").unwrap();
        let mut state = State {
            fully_invested: true,
            ..State::default()
        };
        sweep(&db, &mut state, 100.0, 2.0, 1.0).unwrap();
        assert_eq!(state.cash, 0.0);
        assert_eq!(stock_count(&state), 0);
        let original = state.positions[CORE].quantity;
        fund(&db, &mut state, 200.0, 110.0, 3.0, 2.0).unwrap();
        assert!((state.cash - 200.0).abs() < 1e-8);
        let p = &state.positions[CORE];
        assert!((p.cost - 1000.0 * p.quantity / original).abs() < 1e-8);
        assert!(state.realized_pnl > 0.0);
        assert!(state.fees > 1.0);
        let count: i64 = db
            .query_row("SELECT COUNT(*) FROM fills", [], |r| r.get(0))
            .unwrap();
        assert_eq!(count, 2);
    }
    #[test]
    fn zero_cash_does_not_churn_and_halted_account_does_not_buy() {
        let db = ledger::connect(":memory:").unwrap();
        let mut state = State {
            fully_invested: true,
            ..State::default()
        };
        sweep(&db, &mut state, 100.0, 2.0, 1.0).unwrap();
        let fees = state.fees;
        sweep(&db, &mut state, 101.0, 3.0, 2.0).unwrap();
        assert_eq!(fees, state.fees);
        state.halted = true;
        state.cash = 100.0;
        sweep(&db, &mut state, 102.0, 4.0, 3.0).unwrap();
        assert_eq!(state.cash, 100.0);
    }
}
