//! The named rules of SEMANTICS.md. The checker reports every divergence as one of these.

use std::fmt;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Rule {
    /// An aggressor trades at the best opposite price first and never beyond its limit.
    PricePriority,
    /// Within a price level, the order with the oldest priority timestamp trades first.
    /// Amends keep priority only when they lower quantity at the same price.
    TimePriority,
    /// When an iceberg's visible quantity is exhausted it refills up to its display size,
    /// takes a new timestamp and joins the back of its level.
    IcebergRefillPriority,
    /// Orders of the same account never trade with each other; the configured mode decides
    /// which side is cancelled.
    StpMode,
    /// A fill-or-kill order is either filled completely or leaves no trace.
    FokAtomicity,
    /// After every event the best bid is strictly below the best ask.
    CrossedBook,
    /// For every order, filled + cancelled + resting equals submitted.
    QtyConservation,
}

impl Rule {
    pub const ALL: [Rule; 7] = [
        Rule::PricePriority,
        Rule::TimePriority,
        Rule::IcebergRefillPriority,
        Rule::StpMode,
        Rule::FokAtomicity,
        Rule::CrossedBook,
        Rule::QtyConservation,
    ];

    pub fn name(self) -> &'static str {
        match self {
            Rule::PricePriority => "PRICE_PRIORITY",
            Rule::TimePriority => "TIME_PRIORITY",
            Rule::IcebergRefillPriority => "ICEBERG_REFILL_PRIORITY",
            Rule::StpMode => "STP_MODE",
            Rule::FokAtomicity => "FOK_ATOMICITY",
            Rule::CrossedBook => "CROSSED_BOOK",
            Rule::QtyConservation => "QTY_CONSERVATION",
        }
    }

    pub fn statement(self) -> &'static str {
        match self {
            Rule::PricePriority => {
                "an aggressor trades at the best opposite price first and never beyond its limit"
            }
            Rule::TimePriority => {
                "within a level the oldest priority trades first; only a same-price quantity decrease keeps priority"
            }
            Rule::IcebergRefillPriority => {
                "an iceberg refill shows at most its display size, takes a new timestamp and goes to the back of its level"
            }
            Rule::StpMode => "same-account orders never trade; the STP mode decides which side is cancelled",
            Rule::FokAtomicity => "a fill-or-kill order is filled completely or leaves no trace",
            Rule::CrossedBook => "after every event the best bid is strictly below the best ask",
            Rule::QtyConservation => "for every order, filled + cancelled + resting = submitted",
        }
    }
}

impl fmt::Display for Rule {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}
