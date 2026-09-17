use std::collections::HashMap;
use std::process::ExitCode;
use std::time::Instant;

use matchlaw::csv::{parse_events, parse_steps, write_events, write_step};
use matchlaw::gen::{generate, GenConfig};
use matchlaw::hunt::{check_mutant, hunt};
use matchlaw::mutants::Mutation;
use matchlaw::semantics::Rule;
use matchlaw::{replay, Engine, Event, FastEngine, Oracle, StpMode};

const USAGE: &str = "\
matchlaw: an executable spec for continuous matching engines

USAGE:
  matchlaw generate [--seed N] [--events N] [--profile adversarial|benchmark] [--out FILE]
  matchlaw run EVENTS.csv [--stp MODE] [--engine fast|oracle|MUTANT] [--out FILE]
  matchlaw replay EVENTS.csv FILLS.csv [--stp MODE]
  matchlaw hunt [--mutant MUTANT|all] [--seeds N] [--len N]
  matchlaw bench [--events N] [--oracle-events N] [--seeds N]
  matchlaw rules

STP modes: cancel-newest, cancel-oldest (default), cancel-both, decrement-and-cancel
Mutants:   iceberg-keeps-priority, stp-cancels-aggressor, amend-down-resets-priority,
           fok-partial-fill, rest-after-first-fill, silent-stp-decrement, trade-through

replay exits 0 when the recorded output matches, 1 on a divergence, 2 on bad input.";

struct Args {
    positional: Vec<String>,
    flags: HashMap<String, String>,
}

fn parse_args(raw: &[String]) -> Result<Args, String> {
    let mut positional = Vec::new();
    let mut flags = HashMap::new();
    let mut it = raw.iter();
    while let Some(a) = it.next() {
        if let Some(name) = a.strip_prefix("--") {
            let value = it.next().ok_or_else(|| format!("--{name} needs a value"))?;
            flags.insert(name.to_string(), value.clone());
        } else {
            positional.push(a.clone());
        }
    }
    Ok(Args { positional, flags })
}

impl Args {
    /// Reject flags this subcommand does not know.
    ///
    /// Silently ignoring `--stpp cancel-newest` hands back a confident "no rule
    /// broken" for a mode the engine was never checked against, which is the
    /// one answer this tool must never give by accident.
    fn only(&self, known: &[&str]) -> Result<(), String> {
        let mut unknown: Vec<&str> = self
            .flags
            .keys()
            .map(String::as_str)
            .filter(|f| !known.contains(f))
            .collect();
        if unknown.is_empty() {
            return Ok(());
        }
        unknown.sort_unstable();
        Err(format!(
            "unknown flag{} {} — this command takes {}",
            if unknown.len() > 1 { "s" } else { "" },
            unknown
                .iter()
                .map(|f| format!("--{f}"))
                .collect::<Vec<_>>()
                .join(", "),
            known
                .iter()
                .map(|f| format!("--{f}"))
                .collect::<Vec<_>>()
                .join(", "),
        ))
    }

    fn num<T: std::str::FromStr>(&self, name: &str, default: T) -> Result<T, String> {
        match self.flags.get(name) {
            None => Ok(default),
            Some(v) => v.parse().map_err(|_| format!("--{name}: invalid number {v:?}")),
        }
    }

    fn stp(&self) -> Result<StpMode, String> {
        let v = self
            .flags
            .get("stp")
            .map(String::as_str)
            .unwrap_or("cancel-oldest");
        StpMode::parse(v).ok_or_else(|| format!("unknown STP mode {v:?}"))
    }

    fn file(&self, i: usize, what: &str) -> Result<String, String> {
        let path = self
            .positional
            .get(i)
            .ok_or_else(|| format!("missing {what} file\n\n{USAGE}"))?;
        std::fs::read_to_string(path).map_err(|e| format!("{path}: {e}"))
    }

    fn emit(&self, text: &str) -> Result<(), String> {
        match self.flags.get("out") {
            Some(path) => std::fs::write(path, text).map_err(|e| format!("{path}: {e}")),
            None => {
                print!("{text}");
                Ok(())
            }
        }
    }
}

fn main() -> ExitCode {
    let raw: Vec<String> = std::env::args().skip(1).collect();
    let Some((command, rest)) = raw.split_first() else {
        println!("{USAGE}");
        return ExitCode::SUCCESS;
    };
    let result = parse_args(rest).and_then(|args| match command.as_str() {
        "generate" => args
            .only(&["seed", "events", "profile", "out"])
            .and_then(|()| cmd_generate(&args)),
        "run" => args.only(&["stp", "engine", "out"]).and_then(|()| cmd_run(&args)),
        "replay" => args.only(&["stp"]).and_then(|()| cmd_replay(&args)),
        "hunt" => args
            .only(&["mutant", "seeds", "len", "stp"])
            .and_then(|()| cmd_hunt(&args)),
        "bench" => args
            .only(&["events", "oracle-events", "seeds", "stp"])
            .and_then(|()| cmd_bench(&args)),
        "rules" => {
            for r in Rule::ALL {
                println!("{:<24} {}", r.name(), r.statement());
            }
            Ok(ExitCode::SUCCESS)
        }
        "help" | "--help" | "-h" => {
            println!("{USAGE}");
            Ok(ExitCode::SUCCESS)
        }
        other => Err(format!("unknown command {other:?}\n\n{USAGE}")),
    });
    match result {
        Ok(code) => code,
        Err(e) => {
            eprintln!("error: {e}");
            ExitCode::from(2)
        }
    }
}

fn cmd_generate(args: &Args) -> Result<ExitCode, String> {
    let seed = args.num("seed", 1u64)?;
    let n = args.num("events", 1000usize)?;
    let cfg = match args
        .flags
        .get("profile")
        .map(String::as_str)
        .unwrap_or("adversarial")
    {
        "adversarial" => GenConfig::adversarial(n),
        "benchmark" => GenConfig::benchmark(n),
        p => return Err(format!("unknown profile {p:?}")),
    };
    args.emit(&write_events(&generate(seed, &cfg)))?;
    Ok(ExitCode::SUCCESS)
}

fn cmd_run(args: &Args) -> Result<ExitCode, String> {
    let events = parse_events(&args.file(0, "events")?).map_err(|e| e.to_string())?;
    let stp = args.stp()?;
    let mut engine: Box<dyn Engine> = match args.flags.get("engine").map(String::as_str).unwrap_or("fast") {
        "fast" => Box::new(FastEngine::new(stp)),
        "oracle" => Box::new(Oracle::new(stp)),
        m => match Mutation::parse(m) {
            Some(m) => Box::new(FastEngine::with_mutation(stp, Some(m))),
            None => return Err(format!("unknown engine {m:?}")),
        },
    };
    let mut out = String::from("record,event,a,b,c,d,e\n");
    for (i, e) in events.iter().enumerate() {
        write_step(&mut out, i, &engine.step(e));
    }
    args.emit(&out)?;
    Ok(ExitCode::SUCCESS)
}

fn cmd_replay(args: &Args) -> Result<ExitCode, String> {
    let events = parse_events(&args.file(0, "events")?).map_err(|e| format!("events: {e}"))?;
    let steps = parse_steps(&args.file(1, "fills")?, events.len()).map_err(|e| format!("fills: {e}"))?;
    let stp = args.stp()?;
    match replay(&events, &steps, stp) {
        Ok(n) => {
            println!("OK: {n} events replayed under {}, no rule broken", stp.as_str());
            Ok(ExitCode::SUCCESS)
        }
        Err(d) => {
            println!(
                "DIVERGENCE at event {} ({})",
                d.event_index,
                describe(&events[d.event_index])
            );
            println!("rule:   {}  ({})", d.rule, d.rule.statement());
            println!("detail: {}", d.detail);
            for (label, step) in [("expected", &d.expected), ("got", &d.actual)] {
                let mut rows = String::from("header\n");
                write_step(&mut rows, d.event_index, step);
                println!("{label}:\n{}", indent(&rows));
            }
            Ok(ExitCode::from(1))
        }
    }
}

fn describe(e: &Event) -> String {
    write_events(std::slice::from_ref(e))
        .lines()
        .nth(1)
        .unwrap_or_default()
        .to_string()
}

fn indent(csv: &str) -> String {
    let body: Vec<String> = csv.lines().skip(1).map(|l| format!("  {l}")).collect();
    if body.is_empty() {
        "  (nothing)".to_string()
    } else {
        body.join("\n")
    }
}

fn cmd_hunt(args: &Args) -> Result<ExitCode, String> {
    let seeds = args.num("seeds", 500u64)?;
    let len = args.num("len", 400usize)?;
    let mutants: Vec<Mutation> = match args.flags.get("mutant").map(String::as_str).unwrap_or("all") {
        "all" => Mutation::ALL.to_vec(),
        m => vec![Mutation::parse(m).ok_or_else(|| format!("unknown mutant {m:?}"))?],
    };
    let mut all_caught = true;
    for m in mutants {
        println!(
            "== {} (expect {}, stp {})",
            m.name(),
            m.expected_rule(),
            m.stp_mode().as_str()
        );
        match hunt(m, 0, seeds, len) {
            Some(c) => {
                println!(
                    "caught on seed {} after {} events; shrunk to {} events, divergence at event {}:",
                    c.seed,
                    c.events_until_caught,
                    c.minimal.len(),
                    c.minimal_divergence.event_index
                );
                print!("{}", indent(&write_events(&c.minimal)));
                println!(
                    "\n  -> {}: {}",
                    c.minimal_divergence.rule, c.minimal_divergence.detail
                );
            }
            None => {
                all_caught = false;
                println!("not caught in {seeds} seeds");
            }
        }
    }
    Ok(if all_caught {
        ExitCode::SUCCESS
    } else {
        ExitCode::from(1)
    })
}

fn median(v: &mut [usize]) -> usize {
    if v.is_empty() {
        return 0;
    }
    v.sort_unstable();
    v[v.len() / 2]
}

fn cmd_bench(args: &Args) -> Result<ExitCode, String> {
    let n = args.num("events", 1_000_000usize)?;
    let oracle_n = args.num("oracle-events", n)?;
    let seeds = args.num("seeds", 200u64)?;
    let events = generate(42, &GenConfig::benchmark(n));

    println!(
        "Throughput ({} generated events, benchmark profile, seed 42, stp cancel-oldest)\n",
        n
    );
    println!("| engine | events | seconds | events/sec |");
    println!("|---|---:|---:|---:|");
    let mut fast = FastEngine::new(StpMode::CancelOldest);
    let t = Instant::now();
    for e in &events {
        std::hint::black_box(fast.process(e));
    }
    let fast_secs = t.elapsed().as_secs_f64();
    println!(
        "| fast | {} | {:.3} | {:.0} |",
        n,
        fast_secs,
        n as f64 / fast_secs
    );
    let mut oracle = Oracle::new(StpMode::CancelOldest);
    let t = Instant::now();
    for e in &events[..oracle_n.min(n)] {
        std::hint::black_box(oracle.process(e));
    }
    let oracle_secs = t.elapsed().as_secs_f64();
    let oracle_rate = oracle_n.min(n) as f64 / oracle_secs;
    println!(
        "| oracle | {} | {:.3} | {:.0} |",
        oracle_n.min(n),
        oracle_secs,
        oracle_rate
    );
    println!(
        "\nfast/oracle speedup: {:.0}x; resting orders at end: {}\n",
        (n as f64 / fast_secs) / oracle_rate,
        fast.resting_orders()
    );

    let len = 2_000;
    println!("Time to divergence ({seeds} adversarial streams of {len} events per mutant)\n");
    println!(
        "| mutant | stp mode | caught | median events to catch | named expected rule | other rules named |"
    );
    println!("|---|---|---:|---:|---:|---|");
    for m in Mutation::ALL {
        let mut caught = Vec::new();
        let mut right = 0;
        let mut others: std::collections::BTreeMap<Rule, usize> = Default::default();
        for seed in 0..seeds {
            let events = generate(seed, &GenConfig::adversarial(len));
            if let Err(d) = check_mutant(m, &events) {
                caught.push(d.event_index + 1);
                if d.rule == m.expected_rule() {
                    right += 1;
                } else {
                    *others.entry(d.rule).or_default() += 1;
                }
            }
        }
        let count = caught.len();
        println!(
            "| {} | {} | {}/{} | {} | {}/{} ({}) | {} |",
            m.name(),
            m.stp_mode().as_str(),
            count,
            seeds,
            median(&mut caught),
            right,
            count,
            m.expected_rule(),
            others
                .iter()
                .map(|(r, n)| format!("{r} x{n}"))
                .collect::<Vec<_>>()
                .join(", ")
        );
    }
    Ok(ExitCode::SUCCESS)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(raw: &[&str]) -> Args {
        parse_args(&raw.iter().map(|s| (*s).to_string()).collect::<Vec<_>>()).unwrap()
    }

    #[test]
    fn known_flags_are_accepted() {
        assert!(args(&["--stp", "cancel-both"]).only(&["stp"]).is_ok());
        assert!(args(&[]).only(&["stp"]).is_ok());
    }

    #[test]
    fn a_misspelt_flag_is_an_error_not_a_default() {
        // The danger is silence: --stpp would leave replay on cancel-oldest and
        // report "no rule broken" for a mode the engine was never checked under.
        let err = args(&["--stpp", "cancel-newest"]).only(&["stp"]).unwrap_err();
        assert!(err.contains("--stpp"), "{err}");
        assert!(err.contains("--stp"), "{err}");
    }

    #[test]
    fn every_unknown_flag_is_named_once_in_order() {
        let err = args(&["--zeta", "1", "--alpha", "2", "--stp", "cancel-both"])
            .only(&["stp"])
            .unwrap_err();
        assert!(err.contains("flags --alpha, --zeta"), "{err}");
    }
}
