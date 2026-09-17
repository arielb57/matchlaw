//! Deliberately broken variants of the fast engine, each paired with the rule the checker
//! must name when it catches the bug.

use crate::semantics::Rule;
use crate::types::StpMode;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Mutation {
    /// A refilled iceberg stays at the front of its level.
    IcebergKeepsPriority,
    /// Under cancel-oldest, the incoming order is cancelled instead of the resting one.
    StpCancelsAggressor,
    /// A same-price quantity decrease sends the order to the back of its level.
    AmendDownResetsPriority,
    /// Fill-or-kill orders are executed like immediate-or-cancel.
    FokPartialFill,
    /// A limit order rests its remainder after its first fill even if it still crosses.
    RestAfterFirstFill,
    /// Decrement-and-cancel reduces the resting order without reporting it.
    SilentStpDecrement,
    /// Limit orders trade through their limit price.
    TradeThrough,
}

impl Mutation {
    pub const ALL: [Mutation; 7] = [
        Mutation::IcebergKeepsPriority,
        Mutation::StpCancelsAggressor,
        Mutation::AmendDownResetsPriority,
        Mutation::FokPartialFill,
        Mutation::RestAfterFirstFill,
        Mutation::SilentStpDecrement,
        Mutation::TradeThrough,
    ];

    pub fn name(self) -> &'static str {
        match self {
            Mutation::IcebergKeepsPriority => "iceberg-keeps-priority",
            Mutation::StpCancelsAggressor => "stp-cancels-aggressor",
            Mutation::AmendDownResetsPriority => "amend-down-resets-priority",
            Mutation::FokPartialFill => "fok-partial-fill",
            Mutation::RestAfterFirstFill => "rest-after-first-fill",
            Mutation::SilentStpDecrement => "silent-stp-decrement",
            Mutation::TradeThrough => "trade-through",
        }
    }

    pub fn parse(s: &str) -> Option<Mutation> {
        Mutation::ALL.into_iter().find(|m| m.name() == s)
    }

    pub fn expected_rule(self) -> Rule {
        match self {
            Mutation::IcebergKeepsPriority => Rule::IcebergRefillPriority,
            Mutation::StpCancelsAggressor => Rule::StpMode,
            Mutation::AmendDownResetsPriority => Rule::TimePriority,
            Mutation::FokPartialFill => Rule::FokAtomicity,
            Mutation::RestAfterFirstFill => Rule::CrossedBook,
            Mutation::SilentStpDecrement => Rule::QtyConservation,
            Mutation::TradeThrough => Rule::PricePriority,
        }
    }

    /// The STP mode under which the bug is observable.
    pub fn stp_mode(self) -> StpMode {
        match self {
            Mutation::StpCancelsAggressor => StpMode::CancelOldest,
            Mutation::SilentStpDecrement => StpMode::DecrementAndCancel,
            _ => StpMode::CancelOldest,
        }
    }
}
