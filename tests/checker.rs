//! The checker against hand-built wrong outputs, and the CSV formats' error handling.

use matchlaw::csv::{parse_events, parse_steps, write_steps};
use matchlaw::*;

const EVENTS: &str = "\
action,id,account,side,type,tif,price,qty,display
new,1,1,sell,limit,gtc,100,5,
new,2,2,sell,limit,gtc,101,5,
new,3,3,buy,limit,gtc,101,7,
";

fn correct_steps(events: &[Event], stp: StpMode) -> Vec<Step> {
    let mut e = FastEngine::new(stp);
    events.iter().map(|ev| e.step(ev)).collect()
}

fn diverge(events: &[Event], steps: &[Step]) -> Box<Divergence> {
    replay(events, steps, StpMode::CancelOldest).expect_err("tampered output must diverge")
}

#[test]
fn correct_output_replays_cleanly() {
    let events = parse_events(EVENTS).unwrap();
    let steps = correct_steps(&events, StpMode::CancelOldest);
    assert_eq!(
        steps[2].outcome.fills,
        vec![
            Fill {
                taker: 3,
                maker: 1,
                price: 100,
                qty: 5
            },
            Fill {
                taker: 3,
                maker: 2,
                price: 101,
                qty: 2
            }
        ]
    );
    assert_eq!(replay(&events, &steps, StpMode::CancelOldest), Ok(3));
}

#[test]
fn skipping_the_better_level_is_price_priority() {
    let events = parse_events(EVENTS).unwrap();
    let mut steps = correct_steps(&events, StpMode::CancelOldest);
    steps[2].outcome.fills = vec![
        Fill {
            taker: 3,
            maker: 2,
            price: 101,
            qty: 5,
        },
        Fill {
            taker: 3,
            maker: 1,
            price: 100,
            qty: 2,
        },
    ];
    steps[2].book.asks = vec![BookEntry {
        id: 1,
        price: 100,
        visible: 3,
        reserve: 0,
    }];
    let d = diverge(&events, &steps);
    assert_eq!((d.event_index, d.rule), (2, Rule::PricePriority));
}

#[test]
fn stopping_early_and_resting_is_a_crossed_book() {
    let events = parse_events(EVENTS).unwrap();
    let mut steps = correct_steps(&events, StpMode::CancelOldest);
    steps[2].outcome.fills.truncate(1);
    steps[2].book.bids = vec![BookEntry {
        id: 3,
        price: 101,
        visible: 2,
        reserve: 0,
    }];
    steps[2].book.asks = vec![BookEntry {
        id: 2,
        price: 101,
        visible: 5,
        reserve: 0,
    }];
    let d = diverge(&events, &steps);
    assert_eq!(d.rule, Rule::CrossedBook);
    assert!(d.detail.contains("101"), "{}", d.detail);
}

#[test]
fn quantity_that_vanishes_is_qty_conservation() {
    let events = parse_events(EVENTS).unwrap();
    let mut steps = correct_steps(&events, StpMode::CancelOldest);
    steps[1].book.asks[1].visible = 4;
    let d = diverge(&events, &steps);
    assert_eq!((d.event_index, d.rule), (1, Rule::QtyConservation));
    assert!(d.detail.contains("order 2"), "{}", d.detail);
}

#[test]
fn a_self_trade_fill_is_stp_mode() {
    let text = "new,1,9,sell,limit,gtc,100,5,\nnew,2,9,buy,limit,gtc,100,5,\n";
    let events = parse_events(text).unwrap();
    let mut steps = correct_steps(&events, StpMode::CancelNewest);
    steps[1] = Step {
        outcome: Outcome {
            fills: vec![Fill {
                taker: 2,
                maker: 1,
                price: 100,
                qty: 5,
            }],
            cancels: vec![],
        },
        book: Snapshot::default(),
    };
    let d = replay(&events, &steps, StpMode::CancelNewest).unwrap_err();
    assert_eq!(d.rule, Rule::StpMode);
}

#[test]
fn wrong_stp_side_is_stp_mode_for_each_mode() {
    let text = "new,1,9,sell,limit,gtc,100,5,\nnew,2,9,buy,limit,gtc,100,3,\n";
    let events = parse_events(text).unwrap();
    for (actual_mode, claimed_mode) in [
        (StpMode::CancelNewest, StpMode::CancelOldest),
        (StpMode::CancelOldest, StpMode::CancelBoth),
        (StpMode::DecrementAndCancel, StpMode::CancelNewest),
    ] {
        let steps = correct_steps(&events, actual_mode);
        let d = replay(&events, &steps, claimed_mode).unwrap_err();
        assert_eq!(
            d.rule,
            Rule::StpMode,
            "{} replayed as {}: {}",
            actual_mode.as_str(),
            claimed_mode.as_str(),
            d.detail
        );
    }
}

#[test]
fn a_partial_fok_is_fok_atomicity() {
    let text = "new,1,1,sell,limit,gtc,100,5,\nnew,2,2,buy,limit,fok,100,8,\n";
    let events = parse_events(text).unwrap();
    let mut steps = correct_steps(&events, StpMode::CancelOldest);
    steps[1] = Step {
        outcome: Outcome {
            fills: vec![Fill {
                taker: 2,
                maker: 1,
                price: 100,
                qty: 5,
            }],
            cancels: vec![Cancel {
                id: 2,
                qty: 3,
                reason: CancelReason::Fok,
            }],
        },
        book: Snapshot::default(),
    };
    let d = diverge(&events, &steps);
    assert_eq!(d.rule, Rule::FokAtomicity);
}

#[test]
fn iceberg_showing_more_than_its_display_is_named() {
    let text = "new,1,1,sell,limit,gtc,100,9,3\n";
    let events = parse_events(text).unwrap();
    let mut steps = correct_steps(&events, StpMode::CancelOldest);
    steps[0].book.asks[0] = BookEntry {
        id: 1,
        price: 100,
        visible: 9,
        reserve: 0,
    };
    let d = diverge(&events, &steps);
    assert_eq!(d.rule, Rule::IcebergRefillPriority);
}

#[test]
fn reordering_a_level_is_time_priority() {
    let text = "new,1,1,sell,limit,gtc,100,5,\nnew,2,2,sell,limit,gtc,100,5,\n";
    let events = parse_events(text).unwrap();
    let mut steps = correct_steps(&events, StpMode::CancelOldest);
    steps[1].book.asks.reverse();
    let d = diverge(&events, &steps);
    assert_eq!((d.event_index, d.rule), (1, Rule::TimePriority));
}

#[test]
fn missing_trailing_steps_count_as_an_empty_book() {
    let events = parse_events(EVENTS).unwrap();
    let steps = correct_steps(&events, StpMode::CancelOldest);
    let text = write_steps(&steps[..1]);
    let parsed = parse_steps(&text, events.len()).unwrap();
    let d = replay(&events, &parsed, StpMode::CancelOldest).unwrap_err();
    assert_eq!((d.event_index, d.rule), (1, Rule::QtyConservation));
}

#[test]
fn malformed_input_is_rejected_with_the_line_number() {
    let bad = [
        ("new,1,1,up,limit,gtc,100,5,\n", "invalid side"),
        ("new,1,1,buy,market,gtc,,5,\n", "market orders must be ioc or fok"),
        ("new,1,1,buy,limit,gtc,100,0,\n", "qty must be positive"),
        ("new,1,1,buy,limit,gtc,100,5,0\n", "display must be positive"),
        (
            "new,1,1,buy,limit,gtc,100,5,\nnew,1,1,buy,limit,gtc,100,5,\n",
            "submitted twice",
        ),
        ("amend,1,,,,,abc,5,\n", "invalid price"),
        ("replace,1\n", "unknown action"),
    ];
    for (text, message) in bad {
        let e = parse_events(text).unwrap_err();
        assert!(e.message.contains(message), "{text:?}: {e}");
    }
    let e = parse_events("action,id\n\nnew,1,1,buy,limit,gtc,100,5,\nnew,2,1,buy,limit,xyz,100,5,\n")
        .unwrap_err();
    assert_eq!(e.line, 4);

    assert!(parse_steps("fill,5,1,2,100,1,\n", 3)
        .unwrap_err()
        .message
        .contains("only 3 events"));
    assert!(parse_steps("fill,1,1,2,100,1,\nfill,0,1,2,100,1,\n", 3)
        .unwrap_err()
        .message
        .contains("ordered"));
    assert!(parse_steps("cancel,0,1,2,nope,,\n", 3)
        .unwrap_err()
        .message
        .contains("reason"));
}
