//! The books. Orders fill at the next session's open with a fee and slippage, sales fill
//! before purchases, and purchases spend all the cash there is. Nothing here chooses a
//! stock: it carries out what the manager decided.

use crate::config::{FEE, MAX_POSITIONS, MAX_WEIGHT, MIN_ORDER, SLIP, START_CASH};
use crate::market::Quote;
use crate::store::{EquityPoint, Fill, Order, Position, State, Store, Trade};
use anyhow::Result;
use std::collections::BTreeMap;

/// A queued buy is dropped after this many days without a session to fill it.
const STALE_ORDER_DAYS: f64 = 6.0;

/// What a holding would fetch if sold at `price`, after slippage and the fee.
pub fn liquidation(quantity: f64, price: f64) -> f64 {
    quantity * price * (1.0 - SLIP) * (1.0 - FEE)
}

/// Apply splits and dividends dated after a holding was opened, once each.
fn corporate_actions(
    store: &Store,
    state: &mut State,
    symbol: &str,
    quote: &Quote,
    at: f64,
) -> Result<()> {
    let Some(opened_at) = state.positions.get(symbol).map(|p| p.opened_at) else {
        return Ok(());
    };
    for action in &quote.actions {
        let key = format!("{symbol}:{}:{}", action.kind, action.date);
        if action.date <= opened_at || action.date > at || state.corporate_actions.contains(&key) {
            continue;
        }
        let position = state.positions.get_mut(symbol).expect("held");
        match (
            action.kind.as_str(),
            action.numerator,
            action.denominator,
            action.amount,
        ) {
            ("splits", Some(numerator), Some(denominator), _)
                if numerator > 0.0 && denominator > 0.0 =>
            {
                let ratio = numerator / denominator;
                position.quantity *= ratio;
                position.entry /= ratio;
                store.event(
                    at,
                    "split",
                    &format!("{symbol} split {numerator}-for-{denominator}"),
                )?;
            }
            ("dividends", _, _, Some(amount)) if amount > 0.0 => {
                let cash = position.quantity * amount;
                state.cash += cash;
                state.realized_pnl += cash;
                store.event(at, "dividend", &format!("{symbol} paid ${cash:.2}"))?;
            }
            _ => continue,
        }
        state.corporate_actions.push(key);
    }
    Ok(())
}

/// The first session that opened after `signal_at`.
fn opening_after(quote: &Quote, signal_at: f64) -> Option<&crate::market::Bar> {
    quote.bars.iter().find(|bar| bar.at > signal_at)
}

fn sell(
    store: &Store,
    state: &mut State,
    symbol: &str,
    order: &Order,
    bar_at: f64,
    open: f64,
) -> Result<()> {
    let Some(position) = state.positions.remove(symbol) else {
        return Ok(());
    };
    let price = open * (1.0 - SLIP);
    let fee = position.quantity * price * FEE;
    let proceeds = position.quantity * price - fee;
    let pnl = proceeds - position.cost;
    state.cash += proceeds;
    state.realized_pnl += pnl;
    state.fees += fee;
    store.fill(&Fill {
        at: bar_at,
        signal_at: order.at,
        symbol: symbol.into(),
        side: "SELL".into(),
        quantity: position.quantity,
        price,
        fee,
        realized_pnl: pnl,
        reason: order.reason.clone(),
    })?;
    store.trade(&Trade {
        symbol: symbol.into(),
        sector: position.sector,
        catalyst: position.catalyst,
        opened_at: position.opened_at,
        closed_at: bar_at,
        entry: position.entry,
        exit: price,
        pnl,
        return_pct: if position.cost > 0.0 {
            pnl / position.cost * 100.0
        } else {
            0.0
        },
        thesis: position.thesis,
        exit_reason: order.reason.clone(),
    })
}

fn buy(
    store: &Store,
    state: &mut State,
    symbol: &str,
    order: &Order,
    bar_at: f64,
    open: f64,
    budget: f64,
) -> Result<()> {
    let price = open * (1.0 + SLIP);
    let quantity = budget / (price * (1.0 + FEE));
    let fee = quantity * price * FEE;
    state.cash -= budget;
    state.fees += fee;
    let position = state
        .positions
        .entry(symbol.to_string())
        .or_insert_with(|| Position {
            symbol: symbol.into(),
            opened_at: bar_at,
            sector: order.sector.clone(),
            thesis: order.thesis.clone(),
            catalyst: order.catalyst.clone(),
            ..Position::default()
        });
    position.quantity += quantity;
    position.cost += budget;
    position.entry = position.cost / position.quantity / (1.0 + FEE);
    store.fill(&Fill {
        at: bar_at,
        signal_at: order.at,
        symbol: symbol.into(),
        side: "BUY".into(),
        quantity,
        price,
        fee,
        realized_pnl: 0.0,
        reason: order.reason.clone(),
    })
}

/// Bring the account up to date with the market: apply corporate actions, fill queued orders
/// at the first open after they were placed, mark every holding at its last close and add a
/// point to the equity curve. `quotes` must cover holdings, queued orders and SPY.
pub fn settle(
    store: &Store,
    state: &mut State,
    quotes: &BTreeMap<String, Quote>,
    at: f64,
) -> Result<()> {
    let held: Vec<String> = state.positions.keys().cloned().collect();
    for symbol in &held {
        if let Some(quote) = quotes.get(symbol) {
            corporate_actions(store, state, symbol, quote, at)?;
        }
    }
    let pending = std::mem::take(&mut state.pending);
    let mut waiting = BTreeMap::new();
    let mut buys = Vec::new();
    for (symbol, order) in pending {
        let opening = quotes
            .get(&symbol)
            .and_then(|q| opening_after(q, order.at))
            .map(|bar| (bar.at, bar.open));
        match (order.side.as_str(), opening) {
            ("SELL", Some((bar_at, open))) => sell(store, state, &symbol, &order, bar_at, open)?,
            ("BUY", Some((bar_at, open))) => buys.push((symbol, order, bar_at, open)),
            ("BUY", None) if at - order.at > STALE_ORDER_DAYS * 86400.0 => {
                store.event(
                    at,
                    "cancelled",
                    &format!("Buy of {symbol} found no session to fill in and was dropped"),
                )?;
            }
            _ => {
                if order.side == "BUY" || state.positions.contains_key(&symbol) {
                    waiting.insert(symbol, order);
                }
            }
        }
    }
    let equity = state.equity();
    let weights: Vec<f64> = buys.iter().map(|(_, o, _, _)| o.weight.max(0.0)).collect();
    let rooms: Vec<f64> = buys
        .iter()
        .map(|(symbol, _, _, _)| {
            let held = state.positions.get(symbol).map_or(0.0, |p| p.market_value);
            (MAX_WEIGHT * equity - held).max(0.0)
        })
        .collect();
    let budgets = allocate(state.cash, &weights, &rooms);
    for ((symbol, order, bar_at, open), budget) in buys.into_iter().zip(budgets) {
        if budget >= MIN_ORDER {
            buy(store, state, &symbol, &order, bar_at, open, budget)?;
        } else {
            waiting.insert(symbol, order);
        }
    }
    state.pending = waiting;
    if state.cash.abs() < 1e-6 {
        state.cash = 0.0;
    }
    for (symbol, position) in &mut state.positions {
        if let Some(last) = quotes.get(symbol).and_then(Quote::last) {
            position.mark = Some(last.close);
            position.price_at = Some(last.at);
            position.market_value = liquidation(position.quantity, last.close);
        }
        position.unrealized_pnl = position.market_value - position.cost;
    }
    if let Some(spy) = quotes.get("SPY").and_then(Quote::last) {
        if state.benchmark_quantity == 0.0 {
            state.benchmark_quantity = START_CASH / spy.close;
        }
        state.benchmark_value = Some(state.benchmark_quantity * spy.close);
    }
    let equity = state.equity();
    state.peak = state.peak.max(equity);
    let drawdown = 1.0 - equity / state.peak;
    state.max_drawdown = state.max_drawdown.max(drawdown);
    state.started_at.get_or_insert(at);
    state.last_at = at;
    store.equity_point(
        &EquityPoint {
            at,
            equity,
            cash: state.cash,
            benchmark: state.benchmark_value.unwrap_or(START_CASH),
        },
        drawdown,
    )
}

/// Split `cash` across purchases by weight, without any purchase going over its room
/// (the most it may add before the stock passes [`MAX_WEIGHT`] of the account). What one
/// capped purchase cannot take goes to the others, so the cash is all spent unless every
/// purchase is full.
pub fn allocate(cash: f64, weights: &[f64], rooms: &[f64]) -> Vec<f64> {
    let mut budgets = vec![0.0; weights.len()];
    let mut open: Vec<usize> = (0..weights.len())
        .filter(|&i| weights[i] > 0.0 && rooms[i] > 0.0)
        .collect();
    let mut remaining = cash.max(0.0);
    while !open.is_empty() && remaining > 1e-9 {
        let total: f64 = open.iter().map(|&i| weights[i]).sum();
        let capped: Vec<usize> = open
            .iter()
            .copied()
            .filter(|&i| remaining * weights[i] / total >= rooms[i] - budgets[i])
            .collect();
        if capped.is_empty() {
            for &i in &open {
                budgets[i] += remaining * weights[i] / total;
            }
            break;
        }
        for &i in &capped {
            remaining -= rooms[i] - budgets[i];
            budgets[i] = rooms[i];
        }
        open.retain(|i| !capped.contains(i));
    }
    budgets
}

/// How many new stocks the manager must pick. Money is on its way whenever something is
/// sold or cash is idle, and it fills the book up to [`MAX_POSITIONS`].
pub fn purchases_needed(state: &State, sells: usize) -> usize {
    let kept = state.positions.len().saturating_sub(sells);
    if sells > 0 || state.cash >= MIN_ORDER {
        MAX_POSITIONS.saturating_sub(kept)
    } else {
        0
    }
}

/// How many holdings the manager must sell at the least: a book above [`MAX_POSITIONS`]
/// is cut back to it.
pub fn sales_required(state: &State) -> usize {
    state.positions.len().saturating_sub(MAX_POSITIONS)
}

/// Replace the queue with today's decisions. When money is freed and the book is already
/// full, it is spread evenly over the stocks that were kept, so no cash is left idle.
/// Returns the symbols topped up that way.
pub fn queue(
    state: &mut State,
    sells: &BTreeMap<String, String>,
    buys: &BTreeMap<String, Order>,
    at: f64,
) -> Vec<String> {
    state.pending.clear();
    for (symbol, reason) in sells {
        if state.positions.contains_key(symbol) {
            state.pending.insert(
                symbol.clone(),
                Order {
                    side: "SELL".into(),
                    at,
                    reason: reason.clone(),
                    weight: 1.0,
                    ..Order::default()
                },
            );
        }
    }
    for (symbol, order) in buys {
        if !sells.contains_key(symbol) {
            state.pending.insert(
                symbol.clone(),
                Order {
                    side: "BUY".into(),
                    at,
                    ..order.clone()
                },
            );
        }
    }
    let money_coming = !sells.is_empty() || state.cash >= MIN_ORDER;
    let mut topped = Vec::new();
    if money_coming && buys.is_empty() {
        for symbol in state.positions.keys() {
            if !sells.contains_key(symbol) {
                topped.push(symbol.clone());
            }
        }
        for symbol in &topped {
            state.pending.insert(
                symbol.clone(),
                Order {
                    side: "BUY".into(),
                    at,
                    reason:
                        "The book is full, so freed cash is added to the stocks the manager kept"
                            .into(),
                    weight: 1.0,
                    ..Order::default()
                },
            );
        }
    }
    topped
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::market::{Action, Bar};

    fn quote(bars: &[(f64, f64, f64)]) -> Quote {
        Quote {
            bars: bars
                .iter()
                .map(|(at, open, close)| Bar {
                    at: *at,
                    open: *open,
                    close: *close,
                    volume: 1.0,
                })
                .collect(),
            actions: Vec::new(),
            instrument_type: Some("EQUITY".into()),
        }
    }

    fn held(symbol: &str, quantity: f64, cost: f64) -> Position {
        Position {
            symbol: symbol.into(),
            quantity,
            entry: cost / quantity / (1.0 + FEE),
            cost,
            opened_at: 50.0,
            market_value: cost,
            catalyst: "deal".into(),
            ..Position::default()
        }
    }

    fn buy_order(at: f64, weight: f64) -> Order {
        Order {
            side: "BUY".into(),
            at,
            reason: "pick".into(),
            weight,
            sector: "chips".into(),
            thesis: "thesis".into(),
            catalyst: "deal".into(),
        }
    }

    #[test]
    fn a_sale_funds_the_purchases_and_no_cash_is_left() {
        let store = Store::open(":memory:").unwrap();
        let mut state = State {
            cash: 0.0,
            ..State::default()
        };
        state
            .positions
            .insert("OLD".into(), held("OLD", 10.0, 500.0));
        state
            .positions
            .insert("STAY".into(), held("STAY", 50.0, 5000.0));
        let sells: BTreeMap<String, String> =
            [("OLD".to_string(), "played out".to_string())].into();
        let buys: BTreeMap<String, Order> = [
            ("NEW".to_string(), buy_order(0.0, 2.0)),
            ("ALSO".to_string(), buy_order(0.0, 1.0)),
        ]
        .into();
        assert!(queue(&mut state, &sells, &buys, 100.0).is_empty());
        assert_eq!(state.pending.len(), 3);
        let quotes: BTreeMap<String, Quote> = [
            (
                "OLD".to_string(),
                quote(&[(90.0, 50.0, 50.0), (200.0, 60.0, 61.0)]),
            ),
            (
                "STAY".to_string(),
                quote(&[(90.0, 100.0, 100.0), (200.0, 100.0, 110.0)]),
            ),
            (
                "NEW".to_string(),
                quote(&[(90.0, 20.0, 20.0), (200.0, 20.0, 21.0)]),
            ),
            (
                "ALSO".to_string(),
                quote(&[(90.0, 40.0, 40.0), (200.0, 40.0, 40.0)]),
            ),
            (
                "SPY".to_string(),
                quote(&[(90.0, 500.0, 500.0), (200.0, 500.0, 505.0)]),
            ),
        ]
        .into();
        settle(&store, &mut state, &quotes, 300.0).unwrap();
        let proceeds = 10.0 * 60.0 * (1.0 - SLIP) * (1.0 - FEE);
        assert!(state.cash.abs() < 1e-9, "cash {}", state.cash);
        assert!(state.pending.is_empty());
        assert!(!state.positions.contains_key("OLD"));
        assert!((state.positions["NEW"].cost - proceeds * 2.0 / 3.0).abs() < 1e-9);
        assert!((state.positions["ALSO"].cost - proceeds / 3.0).abs() < 1e-9);
        assert!((state.positions["NEW"].entry - 20.0 * (1.0 + SLIP)).abs() < 1e-9);
        assert_eq!(state.positions["NEW"].thesis, "thesis");
        assert!((state.realized_pnl - (proceeds - 500.0)).abs() < 1e-9);
        assert!((state.positions["STAY"].market_value - liquidation(50.0, 110.0)).abs() < 1e-9);
        let trades = store.trades(5).unwrap();
        assert_eq!(trades.len(), 1);
        assert_eq!(trades[0].exit_reason, "played out");
        assert_eq!(trades[0].catalyst, "deal");
        assert_eq!(store.fills(10).unwrap().len(), 3);
        assert_eq!(store.equity_curve().unwrap().len(), 1);
        assert!((state.benchmark_value.unwrap() - START_CASH).abs() < 1e-9);
    }

    #[test]
    fn no_purchase_passes_the_cap_and_the_rest_goes_elsewhere() {
        let budgets = allocate(100.0, &[3.0, 1.0], &[40.0, 100.0]);
        assert!((budgets[0] - 40.0).abs() < 1e-9);
        assert!((budgets[1] - 60.0).abs() < 1e-9);
        let even = allocate(90.0, &[1.0, 2.0], &[100.0, 100.0]);
        assert!((even[0] - 30.0).abs() < 1e-9 && (even[1] - 60.0).abs() < 1e-9);
        let full = allocate(100.0, &[1.0, 1.0], &[10.0, 20.0]);
        assert!((full.iter().sum::<f64>() - 30.0).abs() < 1e-9);
        assert_eq!(allocate(50.0, &[1.0], &[0.0]), [0.0]);
    }

    #[test]
    fn orders_wait_for_a_session_that_opens_after_them() {
        let store = Store::open(":memory:").unwrap();
        let mut state = State::default();
        state.pending.insert("NEW".into(), buy_order(100.0, 1.0));
        let quotes: BTreeMap<String, Quote> =
            [("NEW".to_string(), quote(&[(90.0, 20.0, 20.0)]))].into();
        settle(&store, &mut state, &quotes, 150.0).unwrap();
        assert!(state.positions.is_empty());
        assert_eq!(state.pending.len(), 1);
        assert!((state.cash - START_CASH).abs() < 1e-9);
        // With no session for a week the buy is dropped, so the manager must redeploy.
        settle(&store, &mut state, &quotes, 100.0 + 7.0 * 86400.0).unwrap();
        assert!(state.pending.is_empty());
    }

    #[test]
    fn the_book_is_filled_and_trimmed_to_its_size() {
        let mut state = State {
            cash: 0.0,
            ..State::default()
        };
        for i in 0..MAX_POSITIONS + 3 {
            state
                .positions
                .insert(format!("S{i}"), held("S", 1.0, 100.0));
        }
        assert_eq!(sales_required(&state), 3);
        assert_eq!(purchases_needed(&state, 3), 0);
        assert_eq!(purchases_needed(&state, 5), 2);
        assert_eq!(purchases_needed(&state, 0), 0);
        let empty = State::default();
        assert_eq!(sales_required(&empty), 0);
        assert_eq!(purchases_needed(&empty, 0), MAX_POSITIONS);
    }

    #[test]
    fn freed_cash_tops_up_the_kept_stocks_when_the_book_is_full() {
        let mut state = State {
            cash: 0.0,
            ..State::default()
        };
        for symbol in ["A", "B", "C"] {
            state
                .positions
                .insert(symbol.into(), held(symbol, 1.0, 100.0));
        }
        let sells: BTreeMap<String, String> = [("A".to_string(), "weak".to_string())].into();
        let topped = queue(&mut state, &sells, &BTreeMap::new(), 10.0);
        assert_eq!(topped, ["B", "C"]);
        assert_eq!(state.pending["A"].side, "SELL");
        assert_eq!(state.pending["B"].side, "BUY");
        // With nothing sold and no cash there is nothing to place.
        let mut quiet = state.clone();
        assert!(queue(&mut quiet, &BTreeMap::new(), &BTreeMap::new(), 10.0).is_empty());
        assert!(quiet.pending.is_empty());
    }

    #[test]
    fn splits_and_dividends_apply_once() {
        let store = Store::open(":memory:").unwrap();
        let mut state = State {
            cash: 0.0,
            ..State::default()
        };
        state
            .positions
            .insert("SPL".into(), held("SPL", 2.0, 200.0));
        let mut q = quote(&[(90.0, 100.0, 100.0), (200.0, 50.0, 50.0)]);
        q.actions = vec![
            Action {
                kind: "splits".into(),
                date: 200.0,
                amount: None,
                numerator: Some(2.0),
                denominator: Some(1.0),
            },
            Action {
                kind: "dividends".into(),
                date: 200.0,
                amount: Some(0.5),
                numerator: None,
                denominator: None,
            },
            Action {
                kind: "splits".into(),
                date: 10.0,
                amount: None,
                numerator: Some(3.0),
                denominator: Some(1.0),
            },
        ];
        let quotes: BTreeMap<String, Quote> = [("SPL".to_string(), q)].into();
        settle(&store, &mut state, &quotes, 300.0).unwrap();
        settle(&store, &mut state, &quotes, 301.0).unwrap();
        assert!((state.positions["SPL"].quantity - 4.0).abs() < 1e-9);
        assert!((state.cash - 2.0).abs() < 1e-9);
        assert_eq!(state.corporate_actions.len(), 2);
    }
}
