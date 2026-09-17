//! The replay checker: steps the oracle in lockstep with a recorded engine output and names
//! the rule behind the first divergence.

use std::collections::{HashMap, HashSet};

use crate::oracle::Oracle;
use crate::semantics::Rule;
use crate::types::*;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Divergence {
    pub event_index: usize,
    pub rule: Rule,
    pub detail: String,
    pub expected: Step,
    pub actual: Step,
}

#[derive(Clone, Debug)]
struct OrderRecord {
    account: Account,
    side: Side,
    display: Option<Qty>,
    submitted: Qty,
    size: Qty,
    filled: Qty,
    cancelled: Qty,
}

/// Per-order accounting built only from inbound events and the engine's own outputs.
#[derive(Clone, Debug, Default)]
pub struct Ledger {
    orders: HashMap<OrderId, OrderRecord>,
}

impl Ledger {
    pub fn account(&self, id: OrderId) -> Option<Account> {
        self.orders.get(&id).map(|o| o.account)
    }

    pub fn display(&self, id: OrderId) -> Option<Qty> {
        self.orders.get(&id).and_then(|o| o.display)
    }

    /// Applies one event and the engine's step; returns every order id the step touched.
    pub fn record(&mut self, event: &Event, before: &Snapshot, step: &Step) -> Vec<OrderId> {
        match event {
            Event::New(o) => {
                self.orders.insert(
                    o.id,
                    OrderRecord {
                        account: o.account,
                        side: o.side,
                        display: o.display,
                        submitted: o.qty,
                        size: o.qty,
                        filled: 0,
                        cancelled: 0,
                    },
                );
            }
            Event::Amend { id, qty, .. } => {
                if let (Some(r), Some(_)) = (self.orders.get_mut(id), before.find(*id)) {
                    if *qty > r.size {
                        r.submitted += qty - r.size;
                    }
                    r.size = *qty;
                }
            }
            Event::Cancel { .. } => {}
        }
        let mut touched = vec![event.id()];
        for f in &step.outcome.fills {
            for id in [f.taker, f.maker] {
                if let Some(r) = self.orders.get_mut(&id) {
                    r.filled += f.qty;
                }
                touched.push(id);
            }
        }
        for c in &step.outcome.cancels {
            if let Some(r) = self.orders.get_mut(&c.id) {
                r.cancelled += c.qty;
            }
            touched.push(c.id);
        }
        touched.extend(before.bids.iter().chain(&before.asks).map(|e| e.id));
        touched.extend(step.book.bids.iter().chain(&step.book.asks).map(|e| e.id));
        touched.sort_unstable();
        touched.dedup();
        touched
    }

    pub fn conservation_violation(&self, touched: &[OrderId], book: &Snapshot) -> Option<String> {
        for &id in touched {
            let Some(r) = self.orders.get(&id) else {
                return Some(format!(
                    "order {id} appears in the output but was never submitted"
                ));
            };
            let resting = book.find(id).map_or(0, |(_, e)| e.leaves());
            if r.filled + r.cancelled + resting != r.submitted {
                return Some(format!(
                    "order {id}: filled {} + cancelled {} + resting {} != submitted {}",
                    r.filled, r.cancelled, resting, r.submitted
                ));
            }
        }
        None
    }
}

/// Invariants that must hold after every event for any correct engine, independent of the
/// oracle. Returns the violated rule and a description.
pub fn invariant_violation(
    ledger: &Ledger,
    event: &Event,
    before: &Snapshot,
    step: &Step,
    touched: &[OrderId],
) -> Option<(Rule, String)> {
    if step.book.is_crossed() {
        return Some((Rule::CrossedBook, crossed_detail(&step.book)));
    }
    if let Some(d) = self_trade(ledger, step) {
        return Some((Rule::StpMode, d));
    }
    if let Some(d) = fok_violation(event, before, step) {
        return Some((Rule::FokAtomicity, d));
    }
    if let Some(d) = ledger.conservation_violation(touched, &step.book) {
        return Some((Rule::QtyConservation, d));
    }
    shape_violation(ledger, &step.book)
}

fn crossed_detail(book: &Snapshot) -> String {
    let bid = book.bids.iter().map(|e| e.price).max().unwrap_or_default();
    let ask = book.asks.iter().map(|e| e.price).min().unwrap_or_default();
    format!("best bid {bid} >= best ask {ask}")
}

fn self_trade(ledger: &Ledger, step: &Step) -> Option<String> {
    step.outcome
        .fills
        .iter()
        .find_map(|f| match (ledger.account(f.taker), ledger.account(f.maker)) {
            (Some(a), Some(b)) if a == b => Some(format!("{f} pairs two orders of account {a}")),
            _ => None,
        })
}

fn fok_violation(event: &Event, before: &Snapshot, step: &Step) -> Option<String> {
    let Event::New(o) = event else { return None };
    if o.tif != Tif::Fok {
        return None;
    }
    let filled: Qty = step
        .outcome
        .fills
        .iter()
        .filter(|f| f.taker == o.id)
        .map(|f| f.qty)
        .sum();
    if filled == o.qty {
        return None;
    }
    let killed = Cancel {
        id: o.id,
        qty: o.qty,
        reason: CancelReason::Fok,
    };
    if step.outcome.fills.is_empty() && step.outcome.cancels == [killed] && step.book == *before {
        return None;
    }
    Some(format!(
        "fill-or-kill order {} for {} filled {} with {} cancel record(s); it must fill completely or leave no trace",
        o.id,
        o.qty,
        filled,
        step.outcome.cancels.len()
    ))
}

fn shape_violation(ledger: &Ledger, book: &Snapshot) -> Option<(Rule, String)> {
    for e in book.bids.iter().chain(&book.asks) {
        match ledger.display(e.id) {
            Some(d) if e.visible > d => {
                return Some((
                    Rule::IcebergRefillPriority,
                    format!(
                        "iceberg {} shows {} above its display size {}",
                        e.id, e.visible, d
                    ),
                ))
            }
            None if e.reserve > 0 => {
                return Some((
                    Rule::QtyConservation,
                    format!("order {} is not an iceberg but has reserve {}", e.id, e.reserve),
                ))
            }
            _ => {}
        }
    }
    None
}

fn stp_cancels(outcome: &Outcome) -> Vec<Cancel> {
    let mut v: Vec<Cancel> = outcome
        .cancels
        .iter()
        .filter(|c| c.reason == CancelReason::Stp)
        .copied()
        .collect();
    v.sort();
    v
}

fn listed(cancels: &[Cancel]) -> String {
    cancels
        .iter()
        .map(Cancel::to_string)
        .collect::<Vec<_>>()
        .join(", ")
}

fn sorted_cancels(outcome: &Outcome) -> Vec<Cancel> {
    let mut v = outcome.cancels.clone();
    v.sort();
    v
}

pub struct Checker {
    oracle: Oracle,
    ledger: Ledger,
    before: Snapshot,
    index: usize,
}

impl Checker {
    pub fn new(stp: StpMode) -> Checker {
        Checker {
            oracle: Oracle::new(stp),
            ledger: Ledger::default(),
            before: Snapshot::default(),
            index: 0,
        }
    }

    /// Feeds one event and what the engine under test emitted for it.
    pub fn step(&mut self, event: &Event, actual: &Step) -> Result<(), Box<Divergence>> {
        let expected = self.oracle.step(event);
        let touched = self.ledger.record(event, &self.before, actual);
        let same = expected.outcome.fills == actual.outcome.fills
            && sorted_cancels(&expected.outcome) == sorted_cancels(&actual.outcome)
            && expected.book == actual.book;
        if !same {
            let (rule, detail) = self.classify(event, &expected, actual, &touched);
            return Err(Box::new(Divergence {
                event_index: self.index,
                rule,
                detail,
                expected,
                actual: actual.clone(),
            }));
        }
        self.before = expected.book;
        self.index += 1;
        Ok(())
    }

    fn classify(&self, event: &Event, expected: &Step, actual: &Step, touched: &[OrderId]) -> (Rule, String) {
        if let Some(v) = invariant_violation(&self.ledger, event, &self.before, actual, touched) {
            return v;
        }
        if let Some(d) = self.price_violation(event, actual) {
            return (Rule::PricePriority, d);
        }
        let refilled: HashSet<OrderId> = self.oracle.refilled_last_event().iter().copied().collect();

        for (k, (e, a)) in expected
            .outcome
            .fills
            .iter()
            .zip(&actual.outcome.fills)
            .enumerate()
        {
            if e == a {
                continue;
            }
            let at = format!("fill #{k}: expected {e}, got {a}");
            if e.price != a.price {
                return (Rule::PricePriority, at);
            }
            if e.maker != a.maker {
                if refilled.contains(&e.maker) || refilled.contains(&a.maker) {
                    return (
                        Rule::IcebergRefillPriority,
                        format!("{at}; a refilled iceberg must queue behind the level"),
                    );
                }
                return (Rule::TimePriority, at);
            }
            if self.ledger.display(e.maker).is_some() {
                return (Rule::IcebergRefillPriority, at);
            }
            return (Rule::QtyConservation, at);
        }

        // The engine kept trading against an iceberg the oracle had already sent to the back
        // of its level; the oracle's queue moved on (perhaps into a self-trade cancel).
        let common = expected.outcome.fills.len();
        if let Some(extra) = actual
            .outcome
            .fills
            .iter()
            .skip(common)
            .find(|f| refilled.contains(&f.maker))
        {
            return (
                Rule::IcebergRefillPriority,
                format!(
                    "unexpected {extra}: iceberg {} refilled and must queue behind its level",
                    extra.maker
                ),
            );
        }

        let (se, sa) = (stp_cancels(&expected.outcome), stp_cancels(&actual.outcome));
        if se != sa {
            let aggressor = self.aggressor(event).map(|a| a.0);
            let engine_stopped_aggressor = sa.iter().any(|c| Some(c.id) == aggressor);
            let refilled_prices: HashSet<Price> = refilled
                .iter()
                .filter_map(|id| self.before.find(*id))
                .map(|(_, e)| e.price)
                .collect();
            let behind_refill = se
                .iter()
                .filter(|c| !sa.contains(c))
                .filter_map(|c| self.before.find(c.id))
                .any(|(_, e)| refilled_prices.contains(&e.price));
            if !engine_stopped_aggressor && behind_refill {
                return (
                    Rule::IcebergRefillPriority,
                    format!("self-trade prevention cancels: expected [{}], got [{}]; the engine never reached an order queued ahead of a refilled iceberg", listed(&se), listed(&sa)),
                );
            }
            return (
                Rule::StpMode,
                format!(
                    "self-trade prevention cancels: expected [{}], got [{}]",
                    listed(&se),
                    listed(&sa)
                ),
            );
        }
        if expected.outcome.fills.len() != actual.outcome.fills.len() {
            return (
                Rule::QtyConservation,
                format!(
                    "expected {} fill(s), got {}",
                    expected.outcome.fills.len(),
                    actual.outcome.fills.len()
                ),
            );
        }

        for side in [Side::Buy, Side::Sell] {
            if let Some(v) =
                self.book_difference(side, expected.book.side(side), actual.book.side(side), &refilled)
            {
                return v;
            }
        }
        (
            Rule::QtyConservation,
            format!(
                "cancel records differ: expected [{}], got [{}]",
                listed(&sorted_cancels(&expected.outcome)),
                listed(&sorted_cancels(&actual.outcome))
            ),
        )
    }

    fn aggressor(&self, event: &Event) -> Option<(OrderId, Account, Side, Option<Price>)> {
        match event {
            Event::New(o) => Some((o.id, o.account, o.side, o.price)),
            Event::Amend { id, price, .. } => self
                .before
                .find(*id)
                .and_then(|_| self.ledger.orders.get(id))
                .map(|r| (*id, r.account, r.side, Some(*price))),
            Event::Cancel { .. } => None,
        }
    }

    fn price_violation(&self, event: &Event, actual: &Step) -> Option<String> {
        let fills = &actual.outcome.fills;
        let Some((id, account, side, limit)) = self.aggressor(event) else {
            return fills
                .first()
                .map(|f| format!("{f} on an event that cannot trade"));
        };
        let maker_side = side.opposite();
        let mut previous: Option<Price> = None;
        for f in fills {
            if f.taker != id {
                return Some(format!("{f}: the taker must be the incoming order {id}"));
            }
            if !crosses(side, limit, f.price) {
                return Some(format!(
                    "{f} trades beyond the limit {:?} of order {id}",
                    limit.unwrap_or_default()
                ));
            }
            if previous.is_some_and(|p| better_price(maker_side, f.price, p)) {
                return Some(format!(
                    "{f} is at a better price than an earlier fill of the same order"
                ));
            }
            previous = Some(f.price);
            let skipped = actual.book.side(maker_side).iter().find(|e| {
                better_price(maker_side, e.price, f.price) && self.ledger.account(e.id) != Some(account)
            });
            if let Some(e) = skipped {
                return Some(format!(
                    "{f} while order {} still rests at the better price {}",
                    e.id, e.price
                ));
            }
        }
        for c in actual
            .outcome
            .cancels
            .iter()
            .filter(|c| c.reason == CancelReason::Stp)
        {
            let same_account_within_limit =
                |e: &&BookEntry| self.ledger.account(e.id) == Some(account) && crosses(side, limit, e.price);
            if c.id == id {
                let opposite = self.before.side(maker_side);
                let any_self = opposite
                    .iter()
                    .any(|e| self.ledger.account(e.id) == Some(account));
                if any_self && !opposite.iter().any(|e| same_account_within_limit(&e)) {
                    return Some(format!(
                        "order {id} was cancelled by STP against an order beyond its limit"
                    ));
                }
            } else if let Some((_, e)) = self.before.find(c.id) {
                if !crosses(side, limit, e.price) {
                    return Some(format!(
                        "resting order {} at {} was reached beyond the limit of order {id}",
                        e.id, e.price
                    ));
                }
            }
        }
        None
    }

    fn book_difference(
        &self,
        side: Side,
        expected: &[BookEntry],
        actual: &[BookEntry],
        refilled: &HashSet<OrderId>,
    ) -> Option<(Rule, String)> {
        if expected == actual {
            return None;
        }
        let by_id = |v: &[BookEntry]| {
            let mut v = v.to_vec();
            v.sort_by_key(|e| e.id);
            v
        };
        let (se, sa) = (by_id(expected), by_id(actual));
        if se != sa {
            if se.len() == sa.len() {
                for (e, a) in se.iter().zip(&sa) {
                    if e != a
                        && e.id == a.id
                        && e.price == a.price
                        && e.leaves() == a.leaves()
                        && self.ledger.display(e.id).is_some()
                    {
                        return Some((
                            Rule::IcebergRefillPriority,
                            format!(
                                "iceberg {} shows {} with reserve {}, expected {} with reserve {}",
                                a.id, a.visible, a.reserve, e.visible, e.reserve
                            ),
                        ));
                    }
                }
            }
            return Some((
                Rule::QtyConservation,
                format!(
                    "resting {} orders differ: expected {:?}, got {:?}",
                    side.as_str(),
                    expected,
                    actual
                ),
            ));
        }
        if let Some(w) = actual
            .windows(2)
            .find(|w| better_price(side, w[1].price, w[0].price))
        {
            return Some((
                Rule::PricePriority,
                format!(
                    "book lists {} at {} ahead of {} at the better price {}",
                    w[0].id, w[0].price, w[1].id, w[1].price
                ),
            ));
        }
        let (e, a) = expected.iter().zip(actual).find(|(e, a)| e != a)?;
        let detail = format!(
            "{} level {}: expected order {} next in priority, got {}",
            side.as_str(),
            e.price,
            e.id,
            a.id
        );
        if refilled.contains(&e.id) || refilled.contains(&a.id) {
            Some((Rule::IcebergRefillPriority, detail))
        } else {
            Some((Rule::TimePriority, detail))
        }
    }
}

/// Replays a whole recorded run. On success returns the number of events checked.
pub fn replay(events: &[Event], steps: &[Step], stp: StpMode) -> Result<usize, Box<Divergence>> {
    let mut checker = Checker::new(stp);
    for (event, step) in events.iter().zip(steps) {
        checker.step(event, step)?;
    }
    Ok(events.len().min(steps.len()))
}
