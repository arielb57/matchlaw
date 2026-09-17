//! Mutation tests: every deliberately broken engine must be caught, named with the expected
//! rule, and shrunk to a minimal stream on which the correct engine passes.

use matchlaw::gen::{generate, GenConfig};
use matchlaw::hunt::{check_engine, check_mutant, hunt, shrink};
use matchlaw::mutants::Mutation;
use matchlaw::*;

#[test]
fn every_mutant_is_caught_and_shrunk_to_a_minimal_stream() {
    for m in Mutation::ALL {
        let c = hunt(m, 0, 300, 400).unwrap_or_else(|| panic!("{} was never caught", m.name()));
        assert_eq!(c.first_rule, m.expected_rule(), "{}", m.name());
        assert_eq!(c.minimal_divergence.rule, m.expected_rule(), "{}", m.name());
        assert!(
            c.minimal.len() <= 4,
            "{} shrank only to {} events",
            m.name(),
            c.minimal.len()
        );
        assert_eq!(
            c.minimal_divergence.event_index,
            c.minimal.len() - 1,
            "shrinking keeps the divergence last"
        );

        // Minimality: dropping any single event loses the catch with the expected rule.
        for i in 0..c.minimal.len() {
            let mut fewer = c.minimal.clone();
            fewer.remove(i);
            let still = matches!(check_mutant(m, &fewer), Err(d) if d.rule == m.expected_rule());
            assert!(
                !still,
                "{}: event {i} of the minimal stream is removable",
                m.name()
            );
        }

        // The stream exposes the mutant, not a checker false positive.
        let stp = m.stp_mode();
        assert_eq!(
            check_engine(&mut FastEngine::new(stp), &c.minimal, stp),
            Ok(c.minimal.len())
        );
    }
}

#[test]
fn mutants_are_named_correctly_on_almost_every_catch() {
    for m in Mutation::ALL {
        let (mut caught, mut right) = (0, 0);
        for seed in 0..150 {
            let events = generate(seed, &GenConfig::adversarial(1_000));
            if let Err(d) = check_mutant(m, &events) {
                caught += 1;
                right += usize::from(d.rule == m.expected_rule());
            }
        }
        assert!(
            caught >= 50,
            "{} caught on only {caught} of 150 streams",
            m.name()
        );
        assert!(
            right * 100 >= caught * 95,
            "{}: expected rule on {right} of {caught} catches",
            m.name()
        );
    }
}

#[test]
fn the_same_divergence_is_reported_from_a_recorded_csv() {
    let m = Mutation::IcebergKeepsPriority;
    let c = hunt(m, 0, 300, 400).expect("caught");
    let mut engine = FastEngine::with_mutation(m.stp_mode(), Some(m));
    let steps: Vec<Step> = c.minimal.iter().map(|e| engine.step(e)).collect();
    let events = matchlaw::csv::parse_events(&matchlaw::csv::write_events(&c.minimal)).unwrap();
    let parsed = matchlaw::csv::parse_steps(&matchlaw::csv::write_steps(&steps), events.len()).unwrap();
    let d = replay(&events, &parsed, m.stp_mode()).unwrap_err();
    assert_eq!(d.rule, Rule::IcebergRefillPriority);
    assert_eq!(d.event_index, c.minimal_divergence.event_index);
}

#[test]
fn shrink_respects_its_evaluation_budget() {
    let events = generate(7, &GenConfig::adversarial(500));
    let mut calls = 0;
    let out = shrink(
        &events,
        |_| {
            calls += 1;
            false
        },
        25,
    );
    assert_eq!(calls, 25);
    assert_eq!(out, events);
}

#[test]
fn shrink_finds_the_single_relevant_event() {
    let events = generate(3, &GenConfig::adversarial(300));
    let target = events[137].clone();
    let out = shrink(&events, |e| e.contains(&target), 10_000);
    assert_eq!(out, vec![target]);
}
