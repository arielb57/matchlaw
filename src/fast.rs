//! The fast engine: a `BTreeMap` of price levels per side, each level a FIFO queue.
//!
//! Queue entries are `(order id, epoch)`. Cancels and priority changes do not search the
//! queue; they bump or drop the order's epoch, and stale entries are skipped when they reach
//! the front (and compacted when they dominate a level).

use std::collections::{BTreeMap, HashMap, VecDeque};

use crate::mutants::Mutation;
use crate::types::*;

#[derive(Debug)]
struct Order {
    account: Account,
    side: Side,
    price: Price,
    visible: Qty,
    reserve: Qty,
    display: Option<Qty>,
    size: Qty,
    epoch: u64,
}

#[derive(Debug, Default)]
struct Level {
    queue: VecDeque<(OrderId, u64)>,
    live: usize,
}

type Orders = HashMap<OrderId, Order>;

fn is_live(orders: &Orders, entry: &(OrderId, u64)) -> bool {
    orders.get(&entry.0).is_some_and(|o| o.epoch == entry.1)
}

#[derive(Debug)]
pub struct FastEngine {
    stp: StpMode,
    mutation: Option<Mutation>,
    bids: BTreeMap<Price, Level>,
    asks: BTreeMap<Price, Level>,
    orders: Orders,
    epoch: u64,
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

impl FastEngine {
    pub fn new(stp: StpMode) -> FastEngine {
        FastEngine::with_mutation(stp, None)
    }

    pub fn with_mutation(stp: StpMode, mutation: Option<Mutation>) -> FastEngine {
        FastEngine {
            stp,
            mutation,
            bids: BTreeMap::new(),
            asks: BTreeMap::new(),
            orders: HashMap::new(),
            epoch: 0,
        }
    }

    pub fn resting_orders(&self) -> usize {
        self.orders.len()
    }

    fn mutated(&self, m: Mutation) -> bool {
        self.mutation == Some(m)
    }

    fn effective_stp(&self) -> StpMode {
        if self.mutated(Mutation::StpCancelsAggressor) && self.stp == StpMode::CancelOldest {
            StpMode::CancelNewest
        } else {
            self.stp
        }
    }

    fn may_trade(&self, side: Side, limit: Option<Price>, price: Price) -> bool {
        (self.mutated(Mutation::TradeThrough)) || crosses(side, limit, price)
    }

    fn next_epoch(&mut self) -> u64 {
        self.epoch += 1;
        self.epoch
    }

    fn book_mut(&mut self, side: Side) -> (&mut BTreeMap<Price, Level>, &mut Orders) {
        match side {
            Side::Buy => (&mut self.bids, &mut self.orders),
            Side::Sell => (&mut self.asks, &mut self.orders),
        }
    }

    fn best_price(&self, side: Side) -> Option<Price> {
        match side {
            Side::Buy => self.bids.keys().next_back().copied(),
            Side::Sell => self.asks.keys().next().copied(),
        }
    }

    /// Removes an order from the map and its level. The queue entry becomes stale.
    fn unlink(&mut self, id: OrderId) -> Option<Order> {
        let order = self.orders.remove(&id)?;
        let (book, orders) = self.book_mut(order.side);
        let level = book.get_mut(&order.price).expect("resting order has a level");
        level.live -= 1;
        if level.live == 0 {
            book.remove(&order.price);
        } else if level.queue.len() > 32 && level.queue.len() > 4 * level.live {
            level.queue.retain(|e| is_live(orders, e));
        }
        Some(order)
    }

    fn rest(&mut self, id: OrderId, mut order: Order) {
        order.epoch = self.next_epoch();
        let (side, price, epoch) = (order.side, order.price, order.epoch);
        self.orders.insert(id, order);
        let (book, _) = self.book_mut(side);
        let level = book.entry(price).or_default();
        level.queue.push_back((id, epoch));
        level.live += 1;
    }

    /// Moves a live order to the back of its level with a new epoch.
    fn requeue(&mut self, id: OrderId) {
        let epoch = self.next_epoch();
        let (book, orders) = match self.orders.get(&id).map(|o| o.side) {
            Some(side) => self.book_mut(side),
            None => return,
        };
        let order = orders.get_mut(&id).expect("checked above");
        order.epoch = epoch;
        book.get_mut(&order.price)
            .expect("resting order has a level")
            .queue
            .push_back((id, epoch));
    }

    fn front(&mut self, side: Side, price: Price) -> OrderId {
        let (book, orders) = self.book_mut(side);
        let level = book.get_mut(&price).expect("best level exists");
        loop {
            let entry = *level.queue.front().expect("a live level has a live entry");
            if is_live(orders, &entry) {
                return entry.0;
            }
            level.queue.pop_front();
        }
    }

    fn execute(&mut self, a: Aggressor, out: &mut Outcome) {
        let opposite = a.side.opposite();
        let stp = self.effective_stp();
        let mut remaining = a.qty;
        while remaining > 0 {
            let Some(price) = self.best_price(opposite) else {
                break;
            };
            if !self.may_trade(a.side, a.limit, price) {
                break;
            }
            let maker_id = self.front(opposite, price);
            let maker = &self.orders[&maker_id];
            let maker_leaves = maker.visible + maker.reserve;

            if maker.account == a.account {
                match stp {
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
                            id: maker_id,
                            qty: maker_leaves,
                            reason: CancelReason::Stp,
                        });
                        self.unlink(maker_id);
                    }
                    StpMode::CancelBoth => {
                        out.cancels.push(Cancel {
                            id: maker_id,
                            qty: maker_leaves,
                            reason: CancelReason::Stp,
                        });
                        self.unlink(maker_id);
                        out.cancels.push(Cancel {
                            id: a.id,
                            qty: remaining,
                            reason: CancelReason::Stp,
                        });
                        remaining = 0;
                    }
                    StpMode::DecrementAndCancel => {
                        let q = remaining.min(maker_leaves);
                        if !self.mutated(Mutation::SilentStpDecrement) {
                            out.cancels.push(Cancel {
                                id: maker_id,
                                qty: q,
                                reason: CancelReason::Stp,
                            });
                        }
                        if q == maker_leaves {
                            self.unlink(maker_id);
                        } else {
                            let m = self.orders.get_mut(&maker_id).expect("maker is live");
                            let from_reserve = q.min(m.reserve);
                            m.reserve -= from_reserve;
                            m.visible -= q - from_reserve;
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
                maker: maker_id,
                price,
                qty: q,
            });
            remaining -= q;
            let m = self.orders.get_mut(&maker_id).expect("maker is live");
            m.visible -= q;
            if m.visible == 0 {
                if m.reserve > 0 {
                    let refill = m.display.unwrap_or(m.reserve).min(m.reserve);
                    m.visible = refill;
                    m.reserve -= refill;
                    if !self.mutated(Mutation::IcebergKeepsPriority) {
                        self.requeue(maker_id);
                    }
                } else {
                    self.unlink(maker_id);
                }
            }
            if remaining > 0
                && self.mutated(Mutation::RestAfterFirstFill)
                && a.tif == Tif::Gtc
                && a.limit.is_some()
            {
                break;
            }
        }

        if remaining == 0 {
            return;
        }
        match (a.limit, a.tif) {
            (Some(price), Tif::Gtc) => {
                let visible = a.display.unwrap_or(remaining).min(remaining);
                let order = Order {
                    account: a.account,
                    side: a.side,
                    price,
                    visible,
                    reserve: remaining - visible,
                    display: a.display,
                    size: a.size,
                    epoch: 0,
                };
                self.rest(a.id, order);
            }
            _ => out.cancels.push(Cancel {
                id: a.id,
                qty: remaining,
                reason: CancelReason::Unfilled,
            }),
        }
    }

    /// Walks the opposite side without mutating it and decides whether a fill-or-kill order
    /// would fill completely. Levels are copied one at a time because iceberg refills and
    /// self-trade encounters depend on the order in which the queue is consumed.
    fn fok_fillable(&self, o: &NewOrder) -> bool {
        let stp = self.effective_stp();
        let mut remaining = o.qty;
        let levels: Box<dyn Iterator<Item = (&Price, &Level)>> = match o.side {
            Side::Buy => Box::new(self.asks.iter()),
            Side::Sell => Box::new(self.bids.iter().rev()),
        };
        for (&price, level) in levels {
            if !self.may_trade(o.side, o.price, price) {
                break;
            }
            let mut queue: VecDeque<(Account, Qty, Qty, Qty)> = level
                .queue
                .iter()
                .filter(|e| is_live(&self.orders, e))
                .map(|(id, _)| {
                    let m = &self.orders[id];
                    (m.account, m.visible, m.reserve, m.display.unwrap_or(Qty::MAX))
                })
                .collect();
            while let Some((account, visible, reserve, display)) = queue.pop_front() {
                if account == o.account {
                    if stp == StpMode::CancelOldest {
                        continue;
                    }
                    return false;
                }
                let q = remaining.min(visible);
                remaining -= q;
                if remaining == 0 {
                    return true;
                }
                if reserve > 0 {
                    let refill = display.min(reserve);
                    let entry = (account, refill, reserve - refill, display);
                    if self.mutated(Mutation::IcebergKeepsPriority) {
                        queue.push_front(entry);
                    } else {
                        queue.push_back(entry);
                    }
                }
            }
        }
        false
    }

    fn amend(&mut self, id: OrderId, price: Price, qty: Qty, out: &mut Outcome) {
        let Some(o) = self.orders.get(&id) else { return };
        if price == o.price && qty == o.size {
            return;
        }
        let leaves = o.visible + o.reserve;
        let new_leaves = if qty >= o.size {
            leaves + (qty - o.size)
        } else {
            leaves.saturating_sub(o.size - qty)
        };
        if new_leaves < leaves {
            out.cancels.push(Cancel {
                id,
                qty: leaves - new_leaves,
                reason: CancelReason::Amend,
            });
        }
        if price == o.price && qty < o.size {
            if new_leaves == 0 {
                self.unlink(id);
                return;
            }
            let m = self.orders.get_mut(&id).expect("checked above");
            m.size = qty;
            m.visible = m.visible.min(new_leaves);
            m.reserve = new_leaves - m.visible;
            if self.mutated(Mutation::AmendDownResetsPriority) {
                self.requeue(id);
            }
            return;
        }
        let o = self.unlink(id).expect("checked above");
        if new_leaves > 0 {
            let aggressor = Aggressor {
                id,
                account: o.account,
                side: o.side,
                limit: Some(price),
                qty: new_leaves,
                display: o.display,
                tif: Tif::Gtc,
                size: qty,
            };
            self.execute(aggressor, out);
        }
    }
}

impl Engine for FastEngine {
    fn process(&mut self, event: &Event) -> Outcome {
        let mut out = Outcome::default();
        match event {
            Event::New(o) => {
                if o.tif == Tif::Fok && !self.mutated(Mutation::FokPartialFill) && !self.fok_fillable(o) {
                    out.cancels.push(Cancel {
                        id: o.id,
                        qty: o.qty,
                        reason: CancelReason::Fok,
                    });
                } else {
                    let tif = if o.tif == Tif::Fok { Tif::Ioc } else { o.tif };
                    let aggressor = Aggressor {
                        id: o.id,
                        account: o.account,
                        side: o.side,
                        limit: o.price,
                        qty: o.qty,
                        display: o.display,
                        tif,
                        size: o.qty,
                    };
                    self.execute(aggressor, &mut out);
                }
            }
            Event::Cancel { id } => {
                if let Some(o) = self.unlink(*id) {
                    out.cancels.push(Cancel {
                        id: *id,
                        qty: o.visible + o.reserve,
                        reason: CancelReason::User,
                    });
                }
            }
            Event::Amend { id, price, qty } => self.amend(*id, *price, *qty, &mut out),
        }
        out
    }

    fn snapshot(&self) -> Snapshot {
        let entries = |levels: &mut dyn Iterator<Item = &Level>| {
            let mut v = Vec::new();
            for level in levels {
                for e in level.queue.iter().filter(|e| is_live(&self.orders, e)) {
                    let o = &self.orders[&e.0];
                    v.push(BookEntry {
                        id: e.0,
                        price: o.price,
                        visible: o.visible,
                        reserve: o.reserve,
                    });
                }
            }
            v
        };
        Snapshot {
            bids: entries(&mut self.bids.values().rev()),
            asks: entries(&mut self.asks.values()),
        }
    }
}
