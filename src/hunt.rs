//! Finding and shrinking streams on which the checker catches a broken engine.

use crate::check::{Checker, Divergence};
use crate::fast::FastEngine;
use crate::gen::{generate, GenConfig};
use crate::mutants::Mutation;
use crate::semantics::Rule;
use crate::types::*;

/// Runs `engine` and the checker side by side, stopping at the first divergence.
pub fn check_engine(
    engine: &mut dyn Engine,
    events: &[Event],
    stp: StpMode,
) -> Result<usize, Box<Divergence>> {
    let mut checker = Checker::new(stp);
    for event in events {
        let step = engine.step(event);
        checker.step(event, &step)?;
    }
    Ok(events.len())
}

pub fn check_mutant(mutation: Mutation, events: &[Event]) -> Result<usize, Box<Divergence>> {
    let stp = mutation.stp_mode();
    check_engine(&mut FastEngine::with_mutation(stp, Some(mutation)), events, stp)
}

/// Removes events while `keep` still holds, coarse chunks first, then single events.
/// `max_evaluations` bounds the work so a pathological predicate cannot run unbounded.
pub fn shrink(
    events: &[Event],
    mut keep: impl FnMut(&[Event]) -> bool,
    max_evaluations: usize,
) -> Vec<Event> {
    let mut current = events.to_vec();
    let mut evaluations = 0;
    let mut chunk = (current.len() / 2).max(1);
    loop {
        let mut progressed = false;
        let mut start = 0;
        while start < current.len() {
            if evaluations >= max_evaluations {
                return current;
            }
            let end = (start + chunk).min(current.len());
            let candidate: Vec<Event> = current[..start].iter().chain(&current[end..]).cloned().collect();
            evaluations += 1;
            if !candidate.is_empty() && keep(&candidate) {
                current = candidate;
                progressed = true;
            } else {
                start = end;
            }
        }
        if chunk == 1 && !progressed {
            return current;
        }
        if !progressed {
            chunk = (chunk / 2).max(1);
        }
    }
}

#[derive(Clone, Debug)]
pub struct Catch {
    pub seed: u64,
    /// Events processed up to and including the one where the divergence surfaced.
    pub events_until_caught: usize,
    pub first_rule: Rule,
    pub minimal: Vec<Event>,
    pub minimal_divergence: Box<Divergence>,
}

/// How many independent catches `hunt` shrinks before keeping the smallest. Removal-only
/// shrinking stops at a local minimum, so a few restarts from different seeds find much
/// shorter streams than one.
const SHRINK_RESTARTS: usize = 8;

/// Generates seeded streams until the checker flags `mutation` with its expected rule, then
/// shrinks those streams to minimal ones still flagged with the same rule, keeping the
/// shortest. `events_until_caught` and `seed` describe the first catch.
pub fn hunt(mutation: Mutation, first_seed: u64, max_seeds: u64, stream_len: usize) -> Option<Catch> {
    let expected = mutation.expected_rule();
    let flagged = |events: &[Event]| matches!(check_mutant(mutation, events), Err(d) if d.rule == expected);
    let mut best: Option<Catch> = None;
    let mut restarts = 0;
    for seed in first_seed..first_seed + max_seeds {
        let events = generate(seed, &GenConfig::adversarial(stream_len));
        let Err(d) = check_mutant(mutation, &events) else {
            continue;
        };
        if d.rule != expected {
            continue;
        }
        let minimal = shrink(&events[..=d.event_index], flagged, 5_000);
        match &mut best {
            None => {
                let minimal_divergence =
                    check_mutant(mutation, &minimal).expect_err("shrinking preserves the catch");
                best = Some(Catch {
                    seed,
                    events_until_caught: d.event_index + 1,
                    first_rule: d.rule,
                    minimal,
                    minimal_divergence,
                });
            }
            Some(b) if minimal.len() < b.minimal.len() => {
                b.minimal_divergence =
                    check_mutant(mutation, &minimal).expect_err("shrinking preserves the catch");
                b.minimal = minimal;
            }
            Some(_) => {}
        }
        restarts += 1;
        if restarts == SHRINK_RESTARTS {
            break;
        }
    }
    best
}
