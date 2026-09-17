//! Differential testing: the fast engine and the oracle must agree on every fill, cancel and
//! book snapshot after every event, and both must satisfy the engine-independent invariants.

use matchlaw::check::{invariant_violation, Ledger};
use matchlaw::gen::{generate, GenConfig};
use matchlaw::*;

const STREAMS_PER_MODE: u64 = 50_000;
const EVENTS_PER_STREAM: usize = 40;

fn run_mode(stp: StpMode) {
    for seed in 0..STREAMS_PER_MODE {
        let events = generate(seed, &GenConfig::adversarial(EVENTS_PER_STREAM));
        let mut fast = FastEngine::new(stp);
        let mut oracle = Oracle::new(stp);
        let mut ledger = Ledger::default();
        let mut before = Snapshot::default();
        for (i, event) in events.iter().enumerate() {
            let f = fast.step(event);
            let o = oracle.step(event);
            assert_eq!(
                f,
                o,
                "{} seed {seed} event {i} ({event:?}) diverged\nstream:\n{}",
                stp.as_str(),
                matchlaw::csv::write_events(&events[..=i])
            );
            let touched = ledger.record(event, &before, &f);
            if let Some((rule, detail)) = invariant_violation(&ledger, event, &before, &f, &touched) {
                panic!("{} seed {seed} event {i}: {rule}: {detail}", stp.as_str());
            }
            for fill in &f.outcome.fills {
                assert!(fill.qty > 0, "zero-quantity fill {fill}");
            }
            before = f.book;
        }
    }
}

#[test]
fn cancel_newest_agrees_with_oracle() {
    run_mode(StpMode::CancelNewest);
}

#[test]
fn cancel_oldest_agrees_with_oracle() {
    run_mode(StpMode::CancelOldest);
}

#[test]
fn cancel_both_agrees_with_oracle() {
    run_mode(StpMode::CancelBoth);
}

#[test]
fn decrement_and_cancel_agrees_with_oracle() {
    run_mode(StpMode::DecrementAndCancel);
}

#[test]
fn long_streams_agree_and_replay_cleanly() {
    for stp in StpMode::ALL {
        for seed in 0..20 {
            let events = generate(seed, &GenConfig::adversarial(3_000));
            let mut fast = FastEngine::new(stp);
            let steps: Vec<Step> = events.iter().map(|e| fast.step(e)).collect();
            let text = matchlaw::csv::write_steps(&steps);
            let parsed = matchlaw::csv::parse_steps(&text, events.len()).unwrap();
            assert_eq!(parsed, steps);
            assert_eq!(replay(&events, &parsed, stp), Ok(events.len()));
        }
    }
}

#[test]
fn generated_streams_exercise_every_feature() {
    let mut fills = 0;
    let mut reasons = std::collections::HashSet::new();
    let mut max_level_depth = 0;
    for seed in 0..200 {
        let events = generate(seed, &GenConfig::adversarial(EVENTS_PER_STREAM));
        let mut fast = FastEngine::new(StpMode::DecrementAndCancel);
        for e in &events {
            let s = fast.step(e);
            fills += s.outcome.fills.len();
            reasons.extend(s.outcome.cancels.iter().map(|c| c.reason));
            for side in [&s.book.bids, &s.book.asks] {
                for e in side.iter() {
                    max_level_depth = max_level_depth.max(side.iter().filter(|x| x.price == e.price).count());
                }
            }
        }
    }
    assert!(fills > 1_000, "only {fills} fills");
    assert_eq!(reasons.len(), 5, "not every cancel reason occurs: {reasons:?}");
    assert!(max_level_depth >= 5, "levels never get deep: {max_level_depth}");
}
