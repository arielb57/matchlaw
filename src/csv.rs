//! Reading and writing the two CSV formats: inbound events, and an engine's emitted records.
//!
//! Events (`events.csv`):
//! ```text
//! action,id,account,side,type,tif,price,qty,display
//! new,1,7,buy,limit,gtc,100,10,
//! new,2,8,sell,limit,gtc,101,50,10
//! new,3,7,sell,market,ioc,,5,
//! amend,2,,,,,102,40,
//! cancel,1,,,,,,,
//! ```
//!
//! Engine output (`fills.csv`), one row per record, tagged with the 0-based event index.
//! Book rows list every resting order after the event in priority order; an event with no
//! book rows means the book is empty.
//! ```text
//! record,event,a,b,c,d,e
//! fill,4,taker,maker,price,qty,
//! cancel,4,id,qty,reason,,
//! book,4,side,id,price,visible,reserve
//! ```

use std::collections::HashSet;
use std::fmt::Write as _;

use crate::types::*;

#[derive(Debug, PartialEq, Eq)]
pub struct ParseError {
    pub line: usize,
    pub message: String,
}

impl std::fmt::Display for ParseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "line {}: {}", self.line, self.message)
    }
}

impl std::error::Error for ParseError {}

fn err<T>(line: usize, message: impl Into<String>) -> Result<T, ParseError> {
    Err(ParseError {
        line,
        message: message.into(),
    })
}

fn field<'a>(fields: &[&'a str], i: usize) -> &'a str {
    fields.get(i).copied().unwrap_or("")
}

fn number<T: std::str::FromStr>(fields: &[&str], i: usize, name: &str, line: usize) -> Result<T, ParseError> {
    let raw = field(fields, i);
    raw.parse()
        .or_else(|_| err(line, format!("invalid {name} {raw:?}")))
}

fn data_lines<'a>(text: &'a str, header: &'a str) -> impl Iterator<Item = (usize, Vec<&'a str>)> {
    text.lines().enumerate().filter_map(move |(i, l)| {
        let l = l.trim();
        if l.is_empty() || l.starts_with('#') || l.starts_with(header) {
            return None;
        }
        Some((i + 1, l.split(',').map(str::trim).collect()))
    })
}

pub fn parse_events(text: &str) -> Result<Vec<Event>, ParseError> {
    let mut events = Vec::new();
    let mut seen = HashSet::new();
    for (line, f) in data_lines(text, "action") {
        let event = match field(&f, 0) {
            "new" => {
                let id: OrderId = number(&f, 1, "id", line)?;
                if !seen.insert(id) {
                    return err(line, format!("order id {id} submitted twice"));
                }
                let account = number(&f, 2, "account", line)?;
                let side = match field(&f, 3) {
                    "buy" => Side::Buy,
                    "sell" => Side::Sell,
                    s => return err(line, format!("invalid side {s:?}")),
                };
                let tif = match field(&f, 5) {
                    "gtc" => Tif::Gtc,
                    "ioc" => Tif::Ioc,
                    "fok" => Tif::Fok,
                    s => return err(line, format!("invalid tif {s:?}")),
                };
                let price = match field(&f, 4) {
                    "limit" => Some(number(&f, 6, "price", line)?),
                    "market" if tif == Tif::Gtc => return err(line, "market orders must be ioc or fok"),
                    "market" => None,
                    s => return err(line, format!("invalid order type {s:?}")),
                };
                let qty: Qty = number(&f, 7, "qty", line)?;
                if qty == 0 {
                    return err(line, "qty must be positive");
                }
                let display = match field(&f, 8) {
                    "" => None,
                    _ => {
                        let d: Qty = number(&f, 8, "display", line)?;
                        if d == 0 {
                            return err(line, "display must be positive");
                        }
                        Some(d)
                    }
                };
                Event::New(NewOrder {
                    id,
                    account,
                    side,
                    price,
                    qty,
                    display,
                    tif,
                })
            }
            "cancel" => Event::Cancel {
                id: number(&f, 1, "id", line)?,
            },
            "amend" => Event::Amend {
                id: number(&f, 1, "id", line)?,
                price: number(&f, 6, "price", line)?,
                qty: number(&f, 7, "qty", line)?,
            },
            a => return err(line, format!("unknown action {a:?}")),
        };
        events.push(event);
    }
    Ok(events)
}

pub fn write_events(events: &[Event]) -> String {
    let mut s = String::from("action,id,account,side,type,tif,price,qty,display\n");
    for e in events {
        match e {
            Event::New(o) => {
                let (kind, price) = match o.price {
                    Some(p) => ("limit", p.to_string()),
                    None => ("market", String::new()),
                };
                let display = o.display.map(|d| d.to_string()).unwrap_or_default();
                let _ = writeln!(
                    s,
                    "new,{},{},{},{},{},{},{},{}",
                    o.id,
                    o.account,
                    o.side.as_str(),
                    kind,
                    o.tif.as_str(),
                    price,
                    o.qty,
                    display
                );
            }
            Event::Cancel { id } => {
                let _ = writeln!(s, "cancel,{id},,,,,,,");
            }
            Event::Amend { id, price, qty } => {
                let _ = writeln!(s, "amend,{id},,,,,{price},{qty},");
            }
        }
    }
    s
}

pub fn parse_steps(text: &str, event_count: usize) -> Result<Vec<Step>, ParseError> {
    let mut steps = vec![Step::default(); event_count];
    let mut last_event = 0;
    for (line, f) in data_lines(text, "record") {
        let event: usize = number(&f, 1, "event index", line)?;
        if event >= event_count {
            return err(line, format!("event index {event} but only {event_count} events"));
        }
        if event < last_event {
            return err(line, "records must be ordered by event index");
        }
        last_event = event;
        let step = &mut steps[event];
        match field(&f, 0) {
            "fill" => step.outcome.fills.push(Fill {
                taker: number(&f, 2, "taker", line)?,
                maker: number(&f, 3, "maker", line)?,
                price: number(&f, 4, "price", line)?,
                qty: number(&f, 5, "qty", line)?,
            }),
            "cancel" => {
                let reason = CancelReason::parse(field(&f, 4));
                let Some(reason) = reason else {
                    return err(line, format!("invalid cancel reason {:?}", field(&f, 4)));
                };
                step.outcome.cancels.push(Cancel {
                    id: number(&f, 2, "id", line)?,
                    qty: number(&f, 3, "qty", line)?,
                    reason,
                });
            }
            "book" => {
                let entry = BookEntry {
                    id: number(&f, 3, "id", line)?,
                    price: number(&f, 4, "price", line)?,
                    visible: number(&f, 5, "visible", line)?,
                    reserve: number(&f, 6, "reserve", line)?,
                };
                match field(&f, 2) {
                    "buy" => step.book.bids.push(entry),
                    "sell" => step.book.asks.push(entry),
                    s => return err(line, format!("invalid side {s:?}")),
                }
            }
            r => return err(line, format!("unknown record {r:?}")),
        }
    }
    Ok(steps)
}

pub fn write_step(out: &mut String, index: usize, step: &Step) {
    for f in &step.outcome.fills {
        let _ = writeln!(out, "fill,{index},{},{},{},{},", f.taker, f.maker, f.price, f.qty);
    }
    for c in &step.outcome.cancels {
        let _ = writeln!(out, "cancel,{index},{},{},{},,", c.id, c.qty, c.reason.as_str());
    }
    for (side, entries) in [(Side::Buy, &step.book.bids), (Side::Sell, &step.book.asks)] {
        for e in entries {
            let _ = writeln!(
                out,
                "book,{index},{},{},{},{},{}",
                side.as_str(),
                e.id,
                e.price,
                e.visible,
                e.reserve
            );
        }
    }
}

pub fn write_steps(steps: &[Step]) -> String {
    let mut s = String::from("record,event,a,b,c,d,e\n");
    for (i, step) in steps.iter().enumerate() {
        write_step(&mut s, i, step);
    }
    s
}
