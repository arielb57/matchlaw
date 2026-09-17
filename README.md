# matchlaw

An executable spec for continuous matching engines: replay your fills, get the first rule broken.

## The problem

Matching engines built for research, crypto venues or internal crossing are usually tested by
reading their output. The bugs that matter are subtle: an iceberg refill that keeps its time
priority, a self-trade prevention mode that cancels the wrong side, a partial fill that leaves
the book crossed. Each one produces output that looks plausible, and it may only show up
thousands of events into a session. Tools exist for call auctions, but continuous matching
with STP and hidden quantity has no reference oracle you can run against your own event log.

## How it works

`SEMANTICS.md` defines the behaviour: limit and market orders, GTC/IOC/FOK, cancel, amend,
icebergs, and four STP modes. Every rule has a name and a minimal example. Two engines are
built separately from that document:

- **Fast engine** (`src/fast.rs`): one `BTreeMap<Price, Level>` per side. Each level is a
  `VecDeque` of `(order id, epoch)` entries plus a `HashMap` from id to order. Cancels,
  refills and loss of priority bump or drop the order's epoch instead of searching the queue.
  Stale entries are skipped when they reach the front and compacted once they make up most of
  a level.
- **Oracle** (`src/oracle.rs`): one flat `Vec` of resting orders. Before every single match
  it is re-sorted by (side, price, priority timestamp) and scanned from the front. A FOK
  order runs on a full clone of the oracle, which is kept only if the order filled completely.
  The oracle is slow, and it is short enough to check against the spec line by line.

The **checker** (`src/check.rs`) reads your inbound events and the records your engine
emitted: fills, cancels, and the full book after each event. It steps the oracle alongside
them and stops at the first event where they differ. It then works out which rule explains
the difference. First come checks that need no oracle: crossed book, self-trade fill,
non-atomic FOK, and a per-order ledger that tests `filled + cancelled + resting = submitted`.
After that it compares your output with the oracle's: fill prices, the first differing maker
(and whether the oracle refilled an iceberg during this event), STP cancels, and queue order
within each level. The rule names are `PRICE_PRIORITY`, `TIME_PRIORITY`,
`ICEBERG_REFILL_PRIORITY`, `STP_MODE`, `FOK_ATOMICITY`, `CROSSED_BOOK` and
`QTY_CONSERVATION`.

A **seeded generator** (`src/gen.rs`) produces adversarial streams. Prices sit within ±2
ticks, so single levels build long queues. There are 3 accounts, so self-crossing is
constant, and icebergs show a third of their size or less. Half of all amends keep their
price. When the checker catches a broken engine, **`hunt`** shrinks the stream by removing
chunks and then single events, with a fixed budget of checker runs. It shrinks several catches
and keeps the shortest result.

Here is a worked example. The engine under test keeps an iceberg's priority when it refills:

```
new,66,1,buy,limit,gtc,1000,5,1     # iceberg: shows 1, hides 4
new,69,1,buy,limit,gtc,1000,7,2     # behind it at the same price
new,71,3,sell,limit,gtc,998,10,     # sweeps the level
```
Order 71 takes 1 from order 66. Order 66 refills and must queue behind 69, so the next fill
belongs to 69. The broken engine fills 66 again, and the checker reports
`ICEBERG_REFILL_PRIORITY` at event 2.

## Install and usage

Requires a Rust toolchain (tested with 1.94.1). No runtime dependencies.

```
git clone <this repository> matchlaw && cd matchlaw
cargo build --release
cargo test
```

Generate a stream, run the reference fast engine on it, and replay the output:

```
$ cargo run --release -q -- generate --seed 7 --events 10000 --out events.csv
$ head -4 events.csv
action,id,account,side,type,tif,price,qty,display
new,1,3,buy,limit,gtc,999,8,1
new,2,3,buy,market,ioc,,10,
new,3,3,sell,limit,gtc,1002,9,2
$ cargo run --release -q -- run events.csv --stp cancel-oldest --out fills.csv
$ head -6 fills.csv
record,event,a,b,c,d,e
book,0,buy,1,999,1,7
cancel,1,2,10,unfilled,,
book,1,buy,1,999,1,7
book,2,buy,1,999,1,7
book,2,sell,3,1002,2,7
$ cargo run --release -q -- replay events.csv fills.csv --stp cancel-oldest
OK: 10000 events replayed under cancel-oldest, no rule broken
```

Replaying the same output under a different STP mode shows what a divergence looks like
(exit code 1):

```
$ cargo run --release -q -- replay events.csv fills.csv --stp cancel-newest
DIVERGENCE at event 5 (new,6,3,buy,limit,gtc,1002,2,)
rule:   STP_MODE  (same-account orders never trade; the STP mode decides which side is cancelled)
detail: self-trade prevention cancels: expected [cancel id=6 qty=2 reason=stp], got [cancel id=3 qty=9 reason=stp]
expected:
  cancel,5,6,2,stp,,
  book,5,buy,1,999,1,0
  book,5,sell,3,1002,2,7
got:
  cancel,5,3,9,stp,,
  book,5,buy,6,1002,2,0
  book,5,buy,1,999,1,0
```

To check your own engine, write its output in the `fills.csv` format:

| record | columns after `event` |
|---|---|
| `fill` | taker id, maker id, price, qty |
| `cancel` | order id, quantity removed, reason (`user`, `unfilled`, `fok`, `stp`, `amend`) |
| `book` | side, order id, price, visible, reserve: every resting order after the event, in priority order |

`event` is the 0-based index of the inbound event. An event with no `book` rows means the book
is empty. The events format is described at the top of `SEMANTICS.md` and in `src/csv.rs`.

Other commands:

```
$ cargo run --release -q -- hunt --mutant iceberg-keeps-priority
== iceberg-keeps-priority (expect ICEBERG_REFILL_PRIORITY, stp cancel-oldest)
caught on seed 0 after 83 events; shrunk to 3 events, divergence at event 2:
  new,66,1,buy,limit,gtc,1000,5,1
  new,69,1,buy,limit,gtc,1000,7,2
  new,71,3,sell,limit,gtc,998,10,
  -> ICEBERG_REFILL_PRIORITY: fill #1: expected fill taker=71 maker=69 2@1000, got fill taker=71 maker=66 1@1000; a refilled iceberg must queue behind the level

$ cargo run --release -q -- rules          # list the rules
$ cargo run --release -q -- run events.csv --engine oracle       # or any mutant name
$ cargo run --release -q -- bench          # the numbers below
```

The deliberately broken engines are the fast engine with one switch flipped
(`src/mutants.rs`):

| mutant | bug | rule the checker must name |
|---|---|---|
| `iceberg-keeps-priority` | refilled iceberg stays at the front of its level | `ICEBERG_REFILL_PRIORITY` |
| `stp-cancels-aggressor` | under cancel-oldest, cancels the incoming order instead | `STP_MODE` |
| `amend-down-resets-priority` | a same-price size decrease sends the order to the back | `TIME_PRIORITY` |
| `fok-partial-fill` | FOK executed like IOC | `FOK_ATOMICITY` |
| `rest-after-first-fill` | a limit order rests after one fill even if it still crosses | `CROSSED_BOOK` |
| `silent-stp-decrement` | decrement-and-cancel reduces the resting order without a cancel record | `QTY_CONSERVATION` |
| `trade-through` | limit orders ignore their limit price | `PRICE_PRIORITY` |

## Testing

`cargo test` runs in about 10 seconds with an optimised test profile.

- **Differential** (`tests/differential.rs`): 50,000 seeded streams of 40 events for each of
  the 4 STP modes (200,000 streams in total). After every event the fast engine and the oracle
  must emit identical fills and cancels and identical book snapshots. Engine-independent
  invariants are also checked: the book is never crossed; `filled + cancelled + resting =
  submitted` holds for every touched order; no fill pairs two orders of one account; a FOK
  fills completely or leaves no trace; no iceberg shows more than its display size. A coverage
  test checks that these streams produce every cancel reason, more than 1,000 fills, and
  levels at least 5 orders deep. Longer 3,000-event streams also go through CSV and the
  replay checker.
- **Mutation** (`tests/mutants.rs`): each mutant is hunted, and the checker must name the
  expected rule. The shrunk stream must have at most 4 events, lose the catch if any single
  event is removed, and pass for the correct engine. Across 150 streams per mutant, at least
  95% of catches must name the expected rule.
- **Hand-written edge cases** (`tests/edge_cases.rs`), with outputs worked out by hand:
  - an iceberg refill that exactly exhausts its reserve
  - decrement-and-cancel with equal quantities
  - an amend down to the filled quantity
  - a market order against an empty side
  - a FOK whose feasibility depends on an iceberg refilling behind a same-account order
  - all four STP modes on one book
  - amend up, reprice, and crossing amends
- **Checker and CLI** (`tests/checker.rs`, `tests/cli.rs`): hand-tampered outputs for each
  rule, malformed CSV with line numbers, exit codes, and a full generate → run → replay round
  trip through the binary.

## Results

Measured with `cargo run --release -- bench` on macOS (Darwin 25), single-threaded, release
build. The CPU model was not recorded; expect different absolute numbers on your machine.
The stream is 1,000,000 events from the benchmark profile: seed 42, 50 accounts, prices
within ±20 ticks, 36% cancels, 10% amends. About 3,800 orders are resting at the end.

| engine | events | seconds | events/sec |
|---|---:|---:|---:|
| fast | 1,000,000 | 0.152 | 6,576,011 |
| oracle | 1,000,000 | 7.221 | 138,482 |

The fast engine is 47× faster here. The gap grows with book depth, because the oracle does
work proportional to the whole book on every match. When the book holds only a handful of
orders, the oracle can be the faster one: sorting a tiny vector beats a `BTreeMap` and a
`HashMap`. That is exactly
why a fast engine needs an independent check: its speed comes from bookkeeping that the oracle
does not have.

Time to divergence: 200 adversarial streams of 2,000 events per mutant, counting events up to
and including the one where the checker stops.

| mutant | caught | median events to catch | named the expected rule |
|---|---:|---:|---:|
| iceberg-keeps-priority | 200/200 | 63 | 200/200 |
| stp-cancels-aggressor | 200/200 | 11 | 199/200 (1× `ICEBERG_REFILL_PRIORITY`) |
| amend-down-resets-priority | 125/200 | 281 | 125/125 |
| fok-partial-fill | 200/200 | 9 | 200/200 |
| rest-after-first-fill | 200/200 | 17 | 200/200 |
| silent-stp-decrement | 200/200 | 12 | 200/200 |
| trade-through | 200/200 | 8 | 200/200 |

`amend-down-resets-priority` is the hardest to catch. It only shows when a same-price
decrease hits an order that has another order behind it at its level, and 75 of 200 streams
never set that up within 2,000 events.

## Design notes

Detecting a divergence is easy. Naming the rule is the hard part. One bug often breaks several
rules at once: a refilled iceberg that keeps its priority also changes which order the
aggressor reaches next. That order may be a same-account order the oracle would have
STP-cancelled, so the first visible difference can be a missing STP cancel. Rather than
a decision tree fitted to the mutants, the checker runs the oracle-free invariants
first, since they are unambiguous, and then compares against the oracle in causal order:
prices, then the first differing maker, then STP cancels, then queue order. The oracle
records which orders refilled during the current event, and that is the only hint the
classifier takes from inside it. The approach is honest about its limits: the classification
is a heuristic over the first differing event, and the benchmark prints every catch that named
a different rule instead of hiding it.

The second decision was to make the oracle *obviously* correct rather than *fast enough*.
It re-sorts before every match and clones itself for every FOK. A faster "reference"
implementation would need its own test suite. The fast engine's FOK check cannot clone the
book, so it copies one price level at a time into a scratch queue and simulates refills and
STP encounters there. That is where subtle disagreements were most likely, so it has its own
hand-written test. Mutants are switches on the real fast engine rather than separate copies:
each one is then a single-line deviation, and the checker is tested against realistic bugs
instead of rewritten engines with unrelated differences.

## Limitations

- The semantics are one reasonable set, not a standard. Real venues differ: some keep priority
  on amend-down of an iceberg's display, some let a refill keep priority, some apply STP at
  the account-group or trader level, and some cancel the resting order only up to the incoming
  quantity. If your venue differs, the checker will report that as a broken rule.
- No stop orders, pegged or post-only orders, minimum quantity, self-match IDs separate from
  accounts, auctions, trading halts, or multiple instruments.
- Your engine must emit a full book snapshot after every event. That makes large logs large
  (book depth × events). There is no mode that checks fills only, or only the top of book.
- Replay stops at the first divergence. It does not resynchronise and continue.
- Rule naming is a heuristic over the first differing event (see the misclassified catch
  above). The event index and the expected-versus-actual records are always exact.
- Prices and quantities are integers (ticks and lots).
- Shrinking only removes events. It does not simplify prices or quantities, so minimal
  streams keep the generator's ids and values.

## License

MIT. See `LICENSE`.
