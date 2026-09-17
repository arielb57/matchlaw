# matchlaw semantics

This document is the single source both engines were written from. Each rule has a name
(the `Rule` enum in `src/semantics.rs`), a statement, and a minimal event sequence in the
`events.csv` format. Expected outputs use the `fills.csv` record format, with the event index
left out.

Notation: `new,id,account,side,type,tif,price,qty,display`, `cancel,id`,
`amend,id,,,,,price,qty,`.

## Model

- **Orders.** Every order has an id, an account, a side, a quantity and a time in force:
  `gtc` (rests), `ioc` (remainder cancelled), `fok` (all or nothing). Limit orders carry a
  price; market orders carry none and must be `ioc` or `fok`.
- **Icebergs.** A `gtc` limit order with a `display` shows at most `display` units
  (`visible`); the rest is `reserve`. An *incoming* iceberg matches its full quantity; only
  when it rests is the remainder split into visible and reserve.
- **Priority timestamp.** A global clock ticks whenever an order starts resting, re-enters
  after losing priority, or refills. Resting orders are ordered by price (best first), then
  by timestamp (oldest first).
- **Matching.** An incoming order trades against the best opposite order while it has
  quantity left and the opposite price is within its limit. Trades happen at the resting
  order's price, for `min(remaining, resting visible)`.
- **Outputs.** Per event, an engine emits fills (`taker, maker, price, qty`), cancels
  (`id, qty, reason`) and the full book after the event. A cancel's `qty` is the quantity it
  removed. Reasons: `user`, `unfilled` (IOC or market remainder), `fok`, `stp`, `amend`.
- **Unknown ids.** Cancelling or amending an order that is not resting emits nothing.

## PRICE_PRIORITY

An aggressor trades at the best opposite price first and never beyond its limit.

```
new,1,1,sell,limit,gtc,103,2,
new,2,1,sell,limit,gtc,101,2,
new,3,1,sell,limit,gtc,102,2,
new,4,2,buy,market,ioc,,5,
```
Event 3: `fill 4←2 2@101`, `fill 4←3 2@102`, `fill 4←1 1@103`; book `sell 1 @103 1`.
A limit buy at 101 would stop after the first fill and rest 3 @101.

## TIME_PRIORITY

Within a level the order with the oldest timestamp trades first. An amend keeps its
timestamp only when it lowers the quantity at an unchanged price. An amend that raises the
quantity or changes the price removes the order and re-enters it as a new aggressor with the
new price and the new remaining quantity; it can trade immediately.

`qty` in an amend is the new *total* size of the order, including what has already
filled. The remaining quantity changes by the same amount as the size (never below zero);
any reduction is reported as a cancel with reason `amend`. Amending down to the filled
quantity removes the order.

```
new,1,1,buy,limit,gtc,100,10,
new,2,2,buy,limit,gtc,100,10,
amend,1,,,,,100,3,
```
Event 2: `cancel 1 7 amend`; book `buy 1 @100 3`, `buy 2 @100 10` (1 keeps its place).
With `amend,1,,,,,100,11,` instead: no cancel; book `buy 2`, `buy 1 @100 11`.

```
new,1,1,sell,limit,gtc,100,10,
new,2,2,buy,limit,gtc,100,4,
amend,1,,,,,100,4,
```
Event 2: `cancel 1 6 amend`; the book is empty.

## ICEBERG_REFILL_PRIORITY

When a resting iceberg's visible quantity reaches zero and reserve remains, it refills
`min(display, reserve)`, takes a new timestamp and goes to the back of its level. The same
incoming order may keep trading with it once it reaches it again. Visible quantity never
exceeds the display size.

```
new,1,1,sell,limit,gtc,100,6,3
new,2,2,sell,limit,gtc,100,2,
new,3,3,buy,limit,gtc,100,3,
new,4,3,buy,limit,gtc,100,4,
```
Event 2: `fill 3←1 3@100`; order 1 refills its whole remaining reserve (3) and the book is
`sell 2 @100 2`, `sell 1 @100 3/0`.
Event 3: `fill 4←2 2@100`, `fill 4←1 2@100`; book `sell 1 @100 1/0`.

## STP_MODE

Two orders of the same account never trade. When the best resting order belongs to the
aggressor's account, the configured mode applies:

| mode | effect |
|---|---|
| `cancel-newest` | cancel the aggressor's remaining quantity; matching stops |
| `cancel-oldest` | cancel the resting order entirely (visible and reserve); matching continues |
| `cancel-both` | cancel the resting order, then the aggressor's remainder; matching stops |
| `decrement-and-cancel` | let `q = min(aggressor remaining, resting remaining)`; cancel `q` from the resting order (reserve first, then visible, keeping its priority; removed at zero), then `q` from the aggressor; matching continues if the aggressor has quantity left |

The resting order's cancel is emitted before the aggressor's.

```
new,1,7,sell,limit,gtc,100,5,
new,2,8,sell,limit,gtc,100,5,
new,3,7,buy,limit,gtc,100,6,
```
Event 2:
- `cancel-newest`: `cancel 3 6 stp`; book unchanged.
- `cancel-oldest`: `cancel 1 5 stp`, `fill 3←2 5@100`; book `buy 3 @100 1`.
- `cancel-both`: `cancel 1 5 stp`, `cancel 3 6 stp`; book `sell 2 @100 5`.
- `decrement-and-cancel`: `cancel 1 5 stp`, `cancel 3 5 stp`, `fill 3←2 1@100`; book `sell 2 @100 4`.

With equal quantities under `decrement-and-cancel` both orders are cancelled in full and
nothing trades:
```
new,1,7,sell,limit,gtc,100,5,
new,2,7,buy,limit,gtc,100,5,
```
Event 1: `cancel 1 5 stp`, `cancel 2 5 stp`; the book is empty.

## FOK_ATOMICITY

A fill-or-kill order is decided by running it against the current book, with all of the
rules above (including refills and STP), on a scratch copy. If that run fills its entire
quantity, the run is committed, including any STP cancels it caused. Otherwise the only
output is `cancel id qty fok` and the book is unchanged. Under `cancel-newest`, `cancel-both`
and `decrement-and-cancel`, meeting a same-account order therefore kills the FOK.

```
new,1,1,sell,limit,gtc,100,6,1
new,2,7,sell,limit,gtc,100,3,
new,3,7,buy,limit,fok,100,2,
```
Event 2 under `cancel-newest`: after one unit trades, order 1 refills behind order 2, which
belongs to account 7, so matching would stop: `cancel 3 2 fok`, book unchanged.
Under `cancel-oldest`: `fill 3←1 1@100`, `cancel 2 3 stp`, `fill 3←1 1@100`.

A market order against an empty side is cancelled whole: `unfilled` for IOC, `fok` for FOK.

## CROSSED_BOOK

After every event the best bid is strictly below the best ask. A limit order rests only
after it has traded everything within its limit.

```
new,1,1,sell,limit,gtc,100,2,
new,2,2,sell,limit,gtc,100,2,
new,3,3,buy,limit,gtc,100,5,
```
Event 2: `fill 3←1 2@100`, `fill 3←2 2@100`; book `buy 3 @100 1`. Resting 3 after the first
fill would leave bid 100 against ask 100.

## QTY_CONSERVATION

For every order, at every event: `filled + cancelled + resting = submitted`, where
`submitted` is the original quantity plus every amend increase. Every unit that leaves an
order must appear in a fill or a cancel record.

```
new,1,1,sell,limit,gtc,100,11,2
new,2,1,buy,limit,gtc,100,8,
```
Event 1 under `decrement-and-cancel`: `cancel 1 8 stp`, `cancel 2 8 stp`; book
`sell 1 @100 2/1`. Reducing order 1 without the `cancel 1 8 stp` record breaks conservation.

## How the checker names a divergence

The checker compares fills (in order), cancels (as a multiset) and the book after every
event. At the first mismatch it tests the engine's output in this order and reports the
first rule that explains it:

1. crossed book → `CROSSED_BOOK`
2. a fill between two orders of one account → `STP_MODE`
3. a FOK that neither filled completely nor left the book untouched with a single `fok`
   cancel → `FOK_ATOMICITY`
4. a conservation break for any touched order → `QTY_CONSERVATION`
5. an iceberg showing more than its display → `ICEBERG_REFILL_PRIORITY`
6. a fill beyond the limit, fills that get better in price, a better-priced order of another
   account left resting, or an STP cancel against an order beyond the limit → `PRICE_PRIORITY`
7. the first differing fill: price → `PRICE_PRIORITY`; maker, when either maker refilled this
   event → `ICEBERG_REFILL_PRIORITY`, otherwise → `TIME_PRIORITY`; an extra fill against an
   order the oracle refilled → `ICEBERG_REFILL_PRIORITY`
8. differing STP cancels → `STP_MODE` (or `ICEBERG_REFILL_PRIORITY` when the engine never
   reached an order that the oracle's refill put ahead of an iceberg)
9. book order within a level → `TIME_PRIORITY`, or `ICEBERG_REFILL_PRIORITY` when a refilled
   order is involved
10. anything else → `QTY_CONSERVATION`
