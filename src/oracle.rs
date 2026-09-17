//! The reference oracle: one flat `Vec` of resting orders, re-sorted by
//! (side, price, priority timestamp) and scanned linearly before every single match.
//! It is deliberately slow; every line should be checkable against SEMANTICS.md by eye.

use crate::types::*;

#[derive(Clone, Debug)]
struct Resting {
    id: OrderId,
    account: Account,
    side: Side,
    price: Price,
    visible: Qty,
    reserve: Qty,
    display: Option<Qty>,
    size: Qty,
    priority: u64,
}

impl Resting {
    fn leaves(&self) -> Qty {
        self.visible + self.reserve
    }
}

struct Aggressor {
    id: OrderId,
    account: Account,
    side: Side,
    limit: Option<Price>,
    qty: Qty,
    display: Option<Qty>,
    tif: Tif,
    size: Qty,
}

#[derive(Clone, Debug)]
pub struct Oracle {
    stp: StpMode,
    orders: Vec<Resting>,
    clock: u64,
    refilled: Vec<OrderId>,
}

impl Oracle {
    pub fn new(stp: StpMode) -> Oracle {
        Oracle {
            stp,
            orders: Vec::new(),
            clock: 0,
            refilled: Vec::new(),
        }
    }

    /// Orders whose iceberg reserve refilled (and so lost priority) during the last event.
    pub fn refilled_last_event(&self) -> &[OrderId] {
        &self.refilled
    }

    fn next_timestamp(&mut self) -> u64 {
        self.clock += 1;
        self.clock
    }

    fn sort(&mut self) {
        self.orders.sort_by_key(|o| {
            let price_key = match o.side {
                Side::Buy => -o.price,
                Side::Sell => o.price,
            };
            (o.side, price_key, o.priority)
        });
    }

    fn position(&self, id: OrderId) -> Option<usize> {
        self.orders.iter().position(|o| o.id == id)
    }

    fn execute(&mut self, a: Aggressor, out: &mut Outcome) {
        let mut remaining = a.qty;
        while remaining > 0 {
            self.sort();
            let Some(best) = self.orders.iter().position(|o| o.side == a.side.opposite()) else {
                break;
            };
            if !crosses(a.side, a.limit, self.orders[best].price) {
                break;
            }
            let maker = self.orders[best].clone();

            if maker.account == a.account {
                match self.stp {
                    StpMode::CancelNewest => {
                        out.cancels.push(Cancel {
                            id: a.id,
                            qty: remaining,
                            reason: CancelReason::Stp,
                        });
                        remaining = 0;
                    }
                    StpMode::CancelOldest => {
                        out.cancels.push(Cancel {
                            id: maker.id,
                            qty: maker.leaves(),
                            reason: CancelReason::Stp,
                        });
                        self.orders.remove(best);
                    }
                    StpMode::CancelBoth => {
                        out.cancels.push(Cancel {
                            id: maker.id,
                            qty: maker.leaves(),
                            reason: CancelReason::Stp,
                        });
                        self.orders.remove(best);
                        out.cancels.push(Cancel {
                            id: a.id,
                            qty: remaining,
                            reason: CancelReason::Stp,
                        });
                        remaining = 0;
                    }
                    StpMode::DecrementAndCancel => {
                        let q = remaining.min(maker.leaves());
                        out.cancels.push(Cancel {
                            id: maker.id,
                            qty: q,
                            reason: CancelReason::Stp,
                        });
                        let m = &mut self.orders[best];
                        let from_reserve = q.min(m.reserve);
                        m.reserve -= from_reserve;
                        m.visible -= q - from_reserve;
                        if m.leaves() == 0 {
                            self.orders.remove(best);
                        }
                        out.cancels.push(Cancel {
                            id: a.id,
                            qty: q,
                            reason: CancelReason::Stp,
                        });
                        remaining -= q;
                    }
                }
                continue;
            }

            let q = remaining.min(maker.visible);
            out.fills.push(Fill {
                taker: a.id,
                maker: maker.id,
                price: maker.price,
                qty: q,
            });
            remaining -= q;
            let m = &mut self.orders[best];
            m.visible -= q;
            if m.visible == 0 {
                if m.reserve > 0 {
                    let display = m.display.unwrap_or(m.reserve);
                    let refill = display.min(m.reserve);
                    let ts = self.next_timestamp();
                    let m = &mut self.orders[best];
                    m.visible = refill;
                    m.reserve -= refill;
                    m.priority = ts;
                    self.refilled.push(maker.id);
                } else {
                    self.orders.remove(best);
                }
            }
        }

        if remaining == 0 {
            return;
        }
        match (a.limit, a.tif) {
            (Some(price), Tif::Gtc) => {
                let visible = a.display.unwrap_or(remaining).min(remaining);
                let priority = self.next_timestamp();
                self.orders.push(Resting {
                    id: a.id,
                    account: a.account,
                    side: a.side,
                    price,
                    visible,
                    reserve: remaining - visible,
                    display: a.display,
                    size: a.size,
                    priority,
                });
            }
            _ => out.cancels.push(Cancel {
                id: a.id,
                qty: remaining,
                reason: CancelReason::Unfilled,
            }),
        }
    }

    fn new_order(&mut self, o: &NewOrder, out: &mut Outcome) {
        let aggressor = |o: &NewOrder| Aggressor {
            id: o.id,
            account: o.account,
            side: o.side,
            limit: o.price,
            qty: o.qty,
            display: o.display,
            tif: o.tif,
            size: o.qty,
        };
        if o.tif == Tif::Fok {
            let mut trial = self.clone();
            let mut trial_out = Outcome::default();
            trial.execute(aggressor(o), &mut trial_out);
            let filled: Qty = trial_out.fills.iter().map(|f| f.qty).sum();
            if filled == o.qty {
                *self = trial;
                *out = trial_out;
            } else {
                out.cancels.push(Cancel {
                    id: o.id,
                    qty: o.qty,
                    reason: CancelReason::Fok,
                });
            }
            return;
        }
        self.execute(aggressor(o), out);
    }

    fn amend(&mut self, id: OrderId, price: Price, qty: Qty, out: &mut Outcome) {
        let Some(idx) = self.position(id) else { return };
        let o = self.orders[idx].clone();
        if price == o.price && qty == o.size {
            return;
        }
        let new_leaves = if qty >= o.size {
            o.leaves() + (qty - o.size)
        } else {
            o.leaves().saturating_sub(o.size - qty)
        };
        if new_leaves < o.leaves() {
            out.cancels.push(Cancel {
                id,
                qty: o.leaves() - new_leaves,
                reason: CancelReason::Amend,
            });
        }
        if price == o.price && qty < o.size {
            if new_leaves == 0 {
                self.orders.remove(idx);
            } else {
                let m = &mut self.orders[idx];
                m.size = qty;
                m.visible = m.visible.min(new_leaves);
                m.reserve = new_leaves - m.visible;
            }
            return;
        }
        self.orders.remove(idx);
        if new_leaves > 0 {
            self.execute(
                Aggressor {
                    id,
                    account: o.account,
                    side: o.side,
                    limit: Some(price),
                    qty: new_leaves,
                    display: o.display,
                    tif: Tif::Gtc,
                    size: qty,
                },
                out,
            );
        }
    }
}

impl Engine for Oracle {
    fn process(&mut self, event: &Event) -> Outcome {
        self.refilled.clear();
        let mut out = Outcome::default();
        match event {
            Event::New(o) => self.new_order(o, &mut out),
            Event::Cancel { id } => {
                if let Some(idx) = self.position(*id) {
                    let o = self.orders.remove(idx);
                    out.cancels.push(Cancel {
                        id: *id,
                        qty: o.leaves(),
                        reason: CancelReason::User,
                    });
                }
            }
            Event::Amend { id, price, qty } => self.amend(*id, *price, *qty, &mut out),
        }
        out
    }

    fn snapshot(&self) -> Snapshot {
        let mut sorted = self.clone();
        sorted.sort();
        let entry = |o: &Resting| BookEntry {
            id: o.id,
            price: o.price,
            visible: o.visible,
            reserve: o.reserve,
        };
        Snapshot {
            bids: sorted
                .orders
                .iter()
                .filter(|o| o.side == Side::Buy)
                .map(entry)
                .collect(),
            asks: sorted
                .orders
                .iter()
                .filter(|o| o.side == Side::Sell)
                .map(entry)
                .collect(),
        }
    }
}
