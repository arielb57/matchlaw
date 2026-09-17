//! Event, output and book-snapshot types shared by every engine and the checker.

use std::fmt;

pub type OrderId = u64;
pub type Account = u32;
pub type Price = i64;
pub type Qty = u64;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Side {
    Buy,
    Sell,
}

impl Side {
    pub fn opposite(self) -> Side {
        match self {
            Side::Buy => Side::Sell,
            Side::Sell => Side::Buy,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Side::Buy => "buy",
            Side::Sell => "sell",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Tif {
    Gtc,
    Ioc,
    Fok,
}

impl Tif {
    pub fn as_str(self) -> &'static str {
        match self {
            Tif::Gtc => "gtc",
            Tif::Ioc => "ioc",
            Tif::Fok => "fok",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum StpMode {
    CancelNewest,
    CancelOldest,
    CancelBoth,
    DecrementAndCancel,
}

impl StpMode {
    pub const ALL: [StpMode; 4] = [
        StpMode::CancelNewest,
        StpMode::CancelOldest,
        StpMode::CancelBoth,
        StpMode::DecrementAndCancel,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            StpMode::CancelNewest => "cancel-newest",
            StpMode::CancelOldest => "cancel-oldest",
            StpMode::CancelBoth => "cancel-both",
            StpMode::DecrementAndCancel => "decrement-and-cancel",
        }
    }

    pub fn parse(s: &str) -> Option<StpMode> {
        StpMode::ALL.into_iter().find(|m| m.as_str() == s)
    }
}

/// A new order. `price == None` is a market order; `display == Some(d)` makes it an iceberg.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NewOrder {
    pub id: OrderId,
    pub account: Account,
    pub side: Side,
    pub price: Option<Price>,
    pub qty: Qty,
    pub display: Option<Qty>,
    pub tif: Tif,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Event {
    New(NewOrder),
    Cancel {
        id: OrderId,
    },
    /// `qty` is the new total order size, including anything already filled.
    Amend {
        id: OrderId,
        price: Price,
        qty: Qty,
    },
}

impl Event {
    pub fn id(&self) -> OrderId {
        match self {
            Event::New(o) => o.id,
            Event::Cancel { id } | Event::Amend { id, .. } => *id,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Fill {
    pub taker: OrderId,
    pub maker: OrderId,
    pub price: Price,
    pub qty: Qty,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum CancelReason {
    /// Explicit cancel request.
    User,
    /// Unfilled remainder of an IOC or market order.
    Unfilled,
    /// Fill-or-kill order that could not be filled completely.
    Fok,
    /// Self-trade prevention.
    Stp,
    /// Quantity removed by an amend that lowered the order size.
    Amend,
}

impl CancelReason {
    pub fn as_str(self) -> &'static str {
        match self {
            CancelReason::User => "user",
            CancelReason::Unfilled => "unfilled",
            CancelReason::Fok => "fok",
            CancelReason::Stp => "stp",
            CancelReason::Amend => "amend",
        }
    }

    pub fn parse(s: &str) -> Option<CancelReason> {
        [
            CancelReason::User,
            CancelReason::Unfilled,
            CancelReason::Fok,
            CancelReason::Stp,
            CancelReason::Amend,
        ]
        .into_iter()
        .find(|r| r.as_str() == s)
    }
}

/// `qty` is the quantity removed by this cancel, which may be less than the order's leaves.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Cancel {
    pub id: OrderId,
    pub qty: Qty,
    pub reason: CancelReason,
}

/// Everything an engine emitted for one inbound event, in emission order.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Outcome {
    pub fills: Vec<Fill>,
    pub cancels: Vec<Cancel>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct BookEntry {
    pub id: OrderId,
    pub price: Price,
    pub visible: Qty,
    pub reserve: Qty,
}

impl BookEntry {
    pub fn leaves(&self) -> Qty {
        self.visible + self.reserve
    }
}

/// Resting orders in priority order: bids best (highest) first, asks best (lowest) first,
/// and within a price level oldest priority first.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Snapshot {
    pub bids: Vec<BookEntry>,
    pub asks: Vec<BookEntry>,
}

impl Snapshot {
    pub fn side(&self, side: Side) -> &[BookEntry] {
        match side {
            Side::Buy => &self.bids,
            Side::Sell => &self.asks,
        }
    }

    pub fn is_crossed(&self) -> bool {
        match (
            self.bids.iter().map(|e| e.price).max(),
            self.asks.iter().map(|e| e.price).min(),
        ) {
            (Some(b), Some(a)) => b >= a,
            _ => false,
        }
    }

    pub fn find(&self, id: OrderId) -> Option<(Side, &BookEntry)> {
        if let Some(e) = self.bids.iter().find(|e| e.id == id) {
            return Some((Side::Buy, e));
        }
        self.asks.iter().find(|e| e.id == id).map(|e| (Side::Sell, e))
    }

    pub fn len(&self) -> usize {
        self.bids.len() + self.asks.len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

/// One step of an engine's output: what it emitted for an event plus the book after it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Step {
    pub outcome: Outcome,
    pub book: Snapshot,
}

impl fmt::Display for Fill {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "fill taker={} maker={} {}@{}",
            self.taker, self.maker, self.qty, self.price
        )
    }
}

impl fmt::Display for Cancel {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "cancel id={} qty={} reason={}",
            self.id,
            self.qty,
            self.reason.as_str()
        )
    }
}

/// Whether an aggressor with limit `limit` on `side` may trade at `price`.
pub fn crosses(side: Side, limit: Option<Price>, price: Price) -> bool {
    match (side, limit) {
        (_, None) => true,
        (Side::Buy, Some(l)) => price <= l,
        (Side::Sell, Some(l)) => price >= l,
    }
}

/// True when `a` is a strictly better resting price than `b` for orders on `side`.
pub fn better_price(side: Side, a: Price, b: Price) -> bool {
    match side {
        Side::Buy => a > b,
        Side::Sell => a < b,
    }
}

pub trait Engine {
    fn process(&mut self, event: &Event) -> Outcome;
    fn snapshot(&self) -> Snapshot;

    fn step(&mut self, event: &Event) -> Step {
        let outcome = self.process(event);
        Step {
            outcome,
            book: self.snapshot(),
        }
    }
}
