//! End-to-end: the binary generates a stream, runs an engine over it and replays the result.

use std::path::PathBuf;
use std::process::{Command, Output};

fn matchlaw(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_matchlaw"))
        .args(args)
        .output()
        .expect("binary runs")
}

fn tmp(name: &str) -> String {
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("cli");
    std::fs::create_dir_all(&dir).unwrap();
    dir.join(name).to_string_lossy().into_owned()
}

#[test]
fn generate_run_replay_round_trip_passes_for_the_fast_engine() {
    let (events, fills) = (tmp("rt_events.csv"), tmp("rt_fills.csv"));
    assert!(
        matchlaw(&["generate", "--seed", "5", "--events", "2000", "--out", &events])
            .status
            .success()
    );
    for stp in [
        "cancel-newest",
        "cancel-oldest",
        "cancel-both",
        "decrement-and-cancel",
    ] {
        assert!(matchlaw(&["run", &events, "--stp", stp, "--out", &fills])
            .status
            .success());
        let out = matchlaw(&["replay", &events, &fills, "--stp", stp]);
        let stdout = String::from_utf8_lossy(&out.stdout);
        assert_eq!(out.status.code(), Some(0), "{stdout}");
        assert!(stdout.contains("OK: 2000 events replayed"), "{stdout}");
    }
}

#[test]
fn replay_names_the_rule_and_event_for_a_broken_engine() {
    let (events, fills) = (tmp("m_events.csv"), tmp("m_fills.csv"));
    std::fs::write(
        &events,
        "action,id,account,side,type,tif,price,qty,display\n\
         new,1,1,buy,limit,gtc,999,6,2\n\
         new,2,1,buy,limit,gtc,999,8,\n\
         new,3,3,sell,limit,fok,998,8,\n",
    )
    .unwrap();
    assert!(matchlaw(&[
        "run",
        &events,
        "--engine",
        "iceberg-keeps-priority",
        "--out",
        &fills
    ])
    .status
    .success());
    let out = matchlaw(&["replay", &events, &fills, "--stp", "cancel-oldest"]);
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert_eq!(out.status.code(), Some(1), "{stdout}");
    assert!(stdout.contains("DIVERGENCE at event 2"), "{stdout}");
    assert!(stdout.contains("rule:   ICEBERG_REFILL_PRIORITY"), "{stdout}");
}

#[test]
fn bad_input_exits_with_code_two() {
    let events = tmp("bad_events.csv");
    std::fs::write(&events, "new,1,1,sideways,limit,gtc,1,1,\n").unwrap();
    let out = matchlaw(&["replay", &events, &events]);
    assert_eq!(out.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&out.stderr).contains("line 1: invalid side"));
    assert_eq!(
        matchlaw(&["replay", &events, &events, "--stp", "cancel-random"])
            .status
            .code(),
        Some(2)
    );
    assert_eq!(matchlaw(&["frobnicate"]).status.code(), Some(2));
}
