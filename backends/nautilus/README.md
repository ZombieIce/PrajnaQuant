# Nautilus worker (`2.0.0rc6`)

This is the MVP-4 [#139](https://github.com/ZombieIce/PrajnaQuant/issues/139)
worker for the [Rust adapter contract](../../crates/prajna-adapter/README.md),
not an Experiment Engine or #133 A15 acceptance report. Python does not plan
orders, size positions, apply platform gates or maintain the authoritative ledger.
Rust sends each Order after incorporating the preceding Fill / Reject.
POC-0 continues to use its separate `2.0.0rc5` pin.

## Run setup and transport

Install `requirements.txt` in a dedicated Python 3.12 environment, then launch
`python backends/nautilus/worker.py`. One process and one new engine serve one Run.
The worker reads the exact `RunPacket::write` layout: `packet.json` and four
version-1 Parquet tables with Decimal128(38,18), UTC nanoseconds and matching DSV.
No JSON/Parquet financial value is converted through float. Supported Instruments
are single-Venue, single-currency, non-inverse Equity/ETF with integral size
increments and positive integral lots. Other mappings fail explicitly.

Before `begin_run`, the controller writes a worker-only `worker.json` beside the
immutable neutral packet tables (it does not change their schema or identity):

```json
{
  "initial_cash": "100000",
  "costs": {
    "commission_rate": "0.001",
    "minimum_commission": "5",
    "buy_tax_rate": "0",
    "sell_tax_rate": "0.001",
    "buy_slippage_rate": "0.001",
    "sell_slippage_rate": "0.001"
  }
}
```

All six fields are required decimal strings. The controller must derive these
from the same effective Run configuration used by the platform ledger; no
default costs or funds are substituted. Initial cash must be exactly
representable at native currency precision. Native CASH/NETTING aggregates the
account, but each order/response retains its platform VP ID; orders are never
netted by the adapter. Platform Instrument IDs map one-to-one to native-symbol
IDs at execution Venue `PRAJNA`, explicitly recorded because Nautilus reserves
`SYNTH` for non-execution synthetic instruments.

Stdout exclusively carries `prajna-adapter@1` JSON lines, with the exact Rust
`begin_run/ready`, `open/opened`, `order/fill|reject`, `close/closed`,
`end_run/done` shapes. Every Order requires a unique string order ID, a VP ID,
whole-lot quantity and the current Session-open timestamp. The shim blocks for
each next request; it submits native market orders and waits for native
Fill/Reject before accepting another request. Partial/unmapped responses abort.
EOF, malformed inputs and mapping failures emit a terminal
`{"type":"error","reason":"..."}` and exit nonzero; this is a transport failure,
not a `Reject` or successful completion. Native callback exceptions are retained
and re-raised after `run()` because Nautilus can otherwise log and suppress them.
The controller must treat terminal errors, EOF, nonzero exit and timeout as failed Runs.

## Construction and comparison results

The worker creates `backend-diagnostics.jsonl` exclusively (existing output is an
error). Consumers must collect this sidecar as a Run result, never treat a missing
file as an empty event list. It keeps the one-request/one-response Rust protocol
unchanged while returning account balances and richer diagnostics. Three explicit
categories are kept separate:

* `adapter_construction`: native Instrument mapping; synthetic open bid/ask
  (`open * (1 - sell_slip)` / `open * (1 + buy_slip)`), close bid=ask=close;
  HALT/TRADING delivery; registered comparison FeeModel and shim time mapping.
* `backend_native`: actual instrument-status callbacks, native Fill/Reject/Denied,
  full prices/quantities/fees, VP identity and native event time.
* `backend_comparison`: per-fill proportional commission, minimum commission
  and side tax versus native `Money`; per-Session CASH balance and positions
  (mapped back to platform IDs). `adapter_error` records failed conversion/runs.

The custom FeeModel computes the same scale-18 half-even notional and commission/
tax table as Rust. Nautilus `Money` then applies currency precision (CNY: two
decimals); differences are explicitly classified `backend_native`, with expected
and actual fee strings. Equal fees are marked `within_tolerance` (exact equality).
These native fees/balances are diagnostics only; Rust recomputes authoritative
costs from the returned native fill price and quantity. Slippage is already in
the price, not charged twice.

Status events reach Nautilus at open + 0.5 ms; the #137-proven synchronous shim
uses barriers at open/close + 1 ms so all quotes and statuses are processed first.
The protocol `executed_at` maps native callback time to the requested Session
open. Both times and the mapping are recorded, not presented as native same-open
timestamps. Sessions must have more than 2 ms duration and separation. No
strategy decision is computed from close data in Python.

## Precision and verified limits

Quotes must be exactly representable by the Instrument price increment. The
worker rejects inexact slip products (including products requiring more than
scale 18), tick misalignment and native conversion loss, never rounding quotes.
Unlike the isolated #137 Price probe, rc6's **Position** construction internally
converts to float and panics above price precision 16. Instruments above 16 are
therefore rejected before `ready`, even though native `Price` itself supports 18.
The neutral packet and Rust JSON protocol still preserve scale 18 exactly; this
is a reported Backend limit, not reduced Domain precision.

The worker uses a new engine per Run; reset is not needed for this lifecycle.
The unchanged #137 probe independently verifies rc6 reset on its hand fixture.
Native L1 quote sizes are synthetic 1,000,000,000 units with liquidity consumption
disabled; unsupported partial fills are fatal rather than fabricated full fills.
These are synthetic checks, not market-liquidity, PIT or total-return evidence.

## Tests and platform coverage

```bash
python -m unittest discover -s backends/nautilus -v
PRAJNA_NAUTILUS_PYTHON=python cargo test -p prajna-adapter --locked
```

The shared Rust S1/S2/S3 and nonempty B2 MA20/60 tests plus the 2-VP case run with
the fake by default and the real worker when the required executable is set.
They compare every platform fill, execution-state trace and daily two-layer mark
to Fast Event, using existing independent golden Decision tapes; this is not the
complete #133 A15 differential classification/Experiment acceptance.
Worker tests separately verify hand-calculated native fees/cash, HALT rejection,
numeric-token rejection and preflight precision failures. Dependencies are
required, with no skip paths. Full adds `nautilus-rc6-worker` on macOS ARM64 /
Python 3.12; lightweight CI remains Rust-only for the adapter and installs no
Nautilus. Linux x86_64 is **not added**: wheel availability is not runtime
verification; its integration behavior remains Unknown.
