//! Hand-written scenarios with outputs worked out from SEMANTICS.md. Each one runs through both
//! engines, which must agree with each other and with the expected result.

use matchlaw::*;

fn limit(id: OrderId, account: Account, side: Side, price: Price, qty: Qty) -> Event {
    Event::New(NewOrder {
        id,
        account,
        side,
        price: Some(price),
        qty,
        display: None,
        tif: Tif::Gtc,
    })
}

fn iceberg(id: OrderId, account: Account, side: Side, price: Price, qty: Qty, display: Qty) -> Event {
    Event::New(NewOrder {
        id,
        account,
        side,
        price: Some(price),
        qty,
        display: Some(display),
        tif: Tif::Gtc,
    })
}

fn with_tif(e: Event, tif: Tif) -> Event {
    match e {
        Event::New(o) => Event::New(NewOrder { tif, ..o }),
        other => other,
    }
}

fn market(id: OrderId, account: Account, side: Side, qty: Qty, tif: Tif) -> Event {
    Event::New(NewOrder {
        id,
        account,
        side,
        price: None,
        qty,
        display: None,
        tif,
    })
}

fn fill(taker: OrderId, maker: OrderId, price: Price, qty: Qty) -> Fill {
    Fill {
        taker,
        maker,
        price,
        qty,
    }
}

fn cancel(id: OrderId, qty: Qty, reason: CancelReason) -> Cancel {
    Cancel { id, qty, reason }
}

fn entry(id: OrderId, price: Price, visible: Qty, reserve: Qty) -> BookEntry {
    BookEntry {
        id,
        price,
        visible,
        reserve,
    }
}

/// Runs both engines and the checker's invariants; returns the agreed steps.
fn run(stp: StpMode, events: &[Event]) -> Vec<Step> {
    let mut fast = FastEngine::new(stp);
    let mut oracle = Oracle::new(stp);
    let steps: Vec<Step> = events
        .iter()
        .map(|e| {
            let f = fast.step(e);
            assert_eq!(f, oracle.step(e), "engines disagree on {e:?}");
            f
        })
        .collect();
    assert_eq!(replay(events, &steps, stp), Ok(events.len()));
    steps
}

#[test]
fn iceberg_refill_that_exactly_exhausts_reserve_goes_behind_the_level() {
    let steps = run(
        StpMode::CancelOldest,
        &[
            iceberg(1, 1, Side::Sell, 100, 6, 3),
            limit(2, 2, Side::Sell, 100, 2),
            limit(3, 3, Side::Buy, 100, 3),
            limit(4, 3, Side::Buy, 100, 4),
            limit(5, 3, Side::Buy, 100, 1),
        ],
    );
    assert_eq!(steps[2].outcome.fills, vec![fill(3, 1, 100, 3)]);
    // The refill takes the whole remaining reserve and queues behind order 2.
    assert_eq!(steps[2].book.asks, vec![entry(2, 100, 2, 0), entry(1, 100, 3, 0)]);
    assert_eq!(
        steps[3].outcome.fills,
        vec![fill(4, 2, 100, 2), fill(4, 1, 100, 2)]
    );
    assert_eq!(steps[3].book.asks, vec![entry(1, 100, 1, 0)]);
    assert_eq!(steps[4].outcome.fills, vec![fill(5, 1, 100, 1)]);
    assert!(steps[4].book.is_empty());
}

#[test]
fn aggressor_keeps_trading_a_refilled_iceberg_that_is_alone_at_its_level() {
    let steps = run(
        StpMode::CancelOldest,
        &[
            iceberg(1, 1, Side::Buy, 50, 10, 2),
            limit(2, 2, Side::Sell, 49, 7),
        ],
    );
    assert_eq!(
        steps[1].outcome.fills,
        vec![
            fill(2, 1, 50, 2),
            fill(2, 1, 50, 2),
            fill(2, 1, 50, 2),
            fill(2, 1, 50, 1)
        ]
    );
    assert_eq!(steps[1].book.bids, vec![entry(1, 50, 1, 2)]);
}

#[test]
fn incoming_iceberg_matches_its_full_size_then_rests_only_its_display() {
    let steps = run(
        StpMode::CancelOldest,
        &[
            limit(1, 1, Side::Sell, 10, 4),
            iceberg(2, 2, Side::Buy, 10, 20, 5),
        ],
    );
    assert_eq!(steps[1].outcome.fills, vec![fill(2, 1, 10, 4)]);
    assert_eq!(steps[1].book.bids, vec![entry(2, 10, 5, 11)]);
}

#[test]
fn decrement_and_cancel_with_equal_quantities_cancels_both_and_trades_nothing() {
    let steps = run(
        StpMode::DecrementAndCancel,
        &[limit(1, 7, Side::Sell, 100, 5), limit(2, 7, Side::Buy, 100, 5)],
    );
    assert!(steps[1].outcome.fills.is_empty());
    assert_eq!(
        steps[1].outcome.cancels,
        vec![cancel(1, 5, CancelReason::Stp), cancel(2, 5, CancelReason::Stp)]
    );
    assert!(steps[1].book.is_empty());
}

#[test]
fn decrement_and_cancel_takes_from_reserve_first_and_keeps_priority() {
    let steps = run(
        StpMode::DecrementAndCancel,
        &[
            iceberg(1, 7, Side::Sell, 100, 10, 4),
            limit(2, 8, Side::Sell, 100, 1),
            limit(3, 7, Side::Buy, 100, 3),
        ],
    );
    assert_eq!(
        steps[2].outcome.cancels,
        vec![cancel(1, 3, CancelReason::Stp), cancel(3, 3, CancelReason::Stp)]
    );
    assert_eq!(steps[2].book.asks, vec![entry(1, 100, 4, 3), entry(2, 100, 1, 0)]);
}

#[test]
fn stp_modes_cancel_the_side_they_name() {
    let events = [
        limit(1, 7, Side::Sell, 100, 5),
        limit(2, 8, Side::Sell, 100, 5),
        limit(3, 7, Side::Buy, 100, 6),
    ];

    let newest = run(StpMode::CancelNewest, &events);
    assert_eq!(newest[2].outcome.cancels, vec![cancel(3, 6, CancelReason::Stp)]);
    assert!(newest[2].outcome.fills.is_empty());
    assert_eq!(newest[2].book.asks.len(), 2);

    let oldest = run(StpMode::CancelOldest, &events);
    assert_eq!(oldest[2].outcome.cancels, vec![cancel(1, 5, CancelReason::Stp)]);
    assert_eq!(oldest[2].outcome.fills, vec![fill(3, 2, 100, 5)]);
    assert_eq!(oldest[2].book.bids, vec![entry(3, 100, 1, 0)]);

    let both = run(StpMode::CancelBoth, &events);
    assert_eq!(
        both[2].outcome.cancels,
        vec![cancel(1, 5, CancelReason::Stp), cancel(3, 6, CancelReason::Stp)]
    );
    assert_eq!(both[2].book.asks, vec![entry(2, 100, 5, 0)]);
    assert!(both[2].book.bids.is_empty());

    let dc = run(StpMode::DecrementAndCancel, &events);
    assert_eq!(
        dc[2].outcome.cancels,
        vec![cancel(1, 5, CancelReason::Stp), cancel(3, 5, CancelReason::Stp)]
    );
    assert_eq!(dc[2].outcome.fills, vec![fill(3, 2, 100, 1)]);
    assert_eq!(dc[2].book.asks, vec![entry(2, 100, 4, 0)]);
}

#[test]
fn amend_down_to_the_filled_quantity_removes_the_order() {
    let steps = run(
        StpMode::CancelOldest,
        &[
            limit(1, 1, Side::Sell, 100, 10),
            limit(2, 2, Side::Buy, 100, 4),
            Event::Amend {
                id: 1,
                price: 100,
                qty: 4,
            },
        ],
    );
    assert_eq!(steps[2].outcome.cancels, vec![cancel(1, 6, CancelReason::Amend)]);
    assert!(steps[2].book.is_empty());
}

#[test]
fn amend_down_keeps_priority_but_amend_up_and_reprice_lose_it() {
    let base = [limit(1, 1, Side::Buy, 100, 10), limit(2, 2, Side::Buy, 100, 10)];

    let down = run(
        StpMode::CancelOldest,
        &[
            base[0].clone(),
            base[1].clone(),
            Event::Amend {
                id: 1,
                price: 100,
                qty: 3,
            },
        ],
    );
    assert_eq!(down[2].book.bids, vec![entry(1, 100, 3, 0), entry(2, 100, 10, 0)]);

    let up = run(
        StpMode::CancelOldest,
        &[
            base[0].clone(),
            base[1].clone(),
            Event::Amend {
                id: 1,
                price: 100,
                qty: 11,
            },
        ],
    );
    assert!(up[2].outcome.cancels.is_empty());
    assert_eq!(up[2].book.bids, vec![entry(2, 100, 10, 0), entry(1, 100, 11, 0)]);

    let away_and_back = run(
        StpMode::CancelOldest,
        &[
            base[0].clone(),
            base[1].clone(),
            Event::Amend {
                id: 1,
                price: 99,
                qty: 10,
            },
            Event::Amend {
                id: 1,
                price: 100,
                qty: 10,
            },
        ],
    );
    assert_eq!(
        away_and_back[3].book.bids,
        vec![entry(2, 100, 10, 0), entry(1, 100, 10, 0)]
    );
}

#[test]
fn amend_that_crosses_trades_as_an_aggressor() {
    let steps = run(
        StpMode::CancelOldest,
        &[
            limit(1, 1, Side::Sell, 101, 5),
            limit(2, 2, Side::Buy, 99, 8),
            Event::Amend {
                id: 2,
                price: 101,
                qty: 8,
            },
        ],
    );
    assert_eq!(steps[2].outcome.fills, vec![fill(2, 1, 101, 5)]);
    assert_eq!(steps[2].book.bids, vec![entry(2, 101, 3, 0)]);
    assert!(steps[2].book.asks.is_empty());
}

#[test]
fn market_order_against_an_empty_side_is_cancelled_whole() {
    let steps = run(
        StpMode::CancelOldest,
        &[
            limit(1, 1, Side::Buy, 100, 5),
            market(2, 2, Side::Buy, 7, Tif::Ioc),
            market(3, 2, Side::Buy, 7, Tif::Fok),
        ],
    );
    assert_eq!(
        steps[1].outcome,
        Outcome {
            fills: vec![],
            cancels: vec![cancel(2, 7, CancelReason::Unfilled)]
        }
    );
    assert_eq!(
        steps[2].outcome,
        Outcome {
            fills: vec![],
            cancels: vec![cancel(3, 7, CancelReason::Fok)]
        }
    );
    assert_eq!(steps[2].book.bids, vec![entry(1, 100, 5, 0)]);
}

#[test]
fn market_order_sweeps_levels_best_price_first() {
    let steps = run(
        StpMode::CancelOldest,
        &[
            limit(1, 1, Side::Sell, 103, 2),
            limit(2, 1, Side::Sell, 101, 2),
            limit(3, 1, Side::Sell, 102, 2),
            market(4, 2, Side::Buy, 5, Tif::Ioc),
        ],
    );
    assert_eq!(
        steps[3].outcome.fills,
        vec![fill(4, 2, 101, 2), fill(4, 3, 102, 2), fill(4, 1, 103, 1)]
    );
    assert_eq!(steps[3].book.asks, vec![entry(1, 103, 1, 0)]);
}

#[test]
fn fok_that_cannot_fill_leaves_no_trace_even_with_cancel_oldest_candidates() {
    let events = [
        limit(1, 7, Side::Sell, 100, 3),
        limit(2, 8, Side::Sell, 100, 3),
        with_tif(limit(3, 7, Side::Buy, 100, 4), Tif::Fok),
    ];
    let steps = run(StpMode::CancelOldest, &events);
    // Only 3 is fillable once order 1 is excluded, so the self-trade cancel must not happen either.
    assert_eq!(
        steps[2].outcome,
        Outcome {
            fills: vec![],
            cancels: vec![cancel(3, 4, CancelReason::Fok)]
        }
    );
    assert_eq!(steps[2].book, steps[1].book);
}

#[test]
fn fok_feasibility_follows_iceberg_refill_order_into_a_self_trade() {
    // After the iceberg's first slice trades it refills behind order 2 (same account as the
    // FOK). Under cancel-newest that stops the FOK even though order 1 alone holds enough.
    let events = [
        iceberg(1, 1, Side::Sell, 100, 6, 1),
        limit(2, 7, Side::Sell, 100, 3),
        with_tif(limit(3, 7, Side::Buy, 100, 2), Tif::Fok),
    ];
    let newest = run(StpMode::CancelNewest, &events);
    assert_eq!(
        newest[2].outcome,
        Outcome {
            fills: vec![],
            cancels: vec![cancel(3, 2, CancelReason::Fok)]
        }
    );

    let oldest = run(StpMode::CancelOldest, &events);
    assert_eq!(
        oldest[2].outcome.fills,
        vec![fill(3, 1, 100, 1), fill(3, 1, 100, 1)]
    );
    assert_eq!(oldest[2].outcome.cancels, vec![cancel(2, 3, CancelReason::Stp)]);
    assert_eq!(oldest[2].book.asks, vec![entry(1, 100, 1, 3)]);
}

#[test]
fn ioc_remainder_is_cancelled_not_rested() {
    let steps = run(
        StpMode::CancelOldest,
        &[
            limit(1, 1, Side::Buy, 100, 3),
            with_tif(limit(2, 2, Side::Sell, 99, 5), Tif::Ioc),
        ],
    );
    assert_eq!(steps[1].outcome.fills, vec![fill(2, 1, 100, 3)]);
    assert_eq!(
        steps[1].outcome.cancels,
        vec![cancel(2, 2, CancelReason::Unfilled)]
    );
    assert!(steps[1].book.is_empty());
}

#[test]
fn cancel_and_amend_of_unknown_orders_are_silent_no_ops() {
    let steps = run(
        StpMode::CancelOldest,
        &[
            limit(1, 1, Side::Buy, 100, 3),
            Event::Cancel { id: 9 },
            Event::Amend {
                id: 9,
                price: 1,
                qty: 1,
            },
            Event::Cancel { id: 1 },
            Event::Cancel { id: 1 },
        ],
    );
    assert_eq!(steps[1].outcome, Outcome::default());
    assert_eq!(steps[2].outcome, Outcome::default());
    assert_eq!(steps[3].outcome.cancels, vec![cancel(1, 3, CancelReason::User)]);
    assert_eq!(steps[4].outcome, Outcome::default());
}
