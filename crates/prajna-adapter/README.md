# Rust Adapter contract

`prajna-adapter` is the process-independent MVP-4 Order / Fill / Reject boundary
([#138](https://github.com/ZombieIce/PrajnaQuant/issues/138),
[ADR 0020](../../docs/decisions/0020-accurate-event-adapter-and-ledger-authority.md)).
Its deterministic in-process fake is **not** Nautilus or #133 A15 evidence.
The [pinned Python worker](../../backends/nautilus/README.md) implements this
protocol and runs the shared parity/2-VP suite in Full. Experiment integration
and complete real-Backend difference classification remain separate tickets.

## Ownership and crate boundary

The adapter depends on Domain, Account, Data and Research, never the legacy warehouse,
DuckDB, Python/PyO3 or Nautilus. Backend-specific types do not enter Domain.
Research owns a `MarketExecution` seam and the existing Fast Event planner, sizing,
`ExecutionState`, costs and ledger loop; the adapter implements that seam. There is
no second strategy, sizing, performance or accounting core. The embedded
`FastEventResult` describes the shared execution rules, not an `accurate_event@1`
Experiment identity or a claim of real Accurate Engine integration.

## One-time Run packet

`RunPacket::from_dataset` reads D7 normalized tables. `write` creates a new,
non-overwritable directory containing `packet.json` (protocol and Dataset Version)
and `instruments.parquet`, `sessions.parquet`, `bars.parquet`,
`execution_status.parquet`. These reuse version-1 `prajna-data` Arrow schemas,
Decimal128(38,18), UTC nanoseconds, explicit lot/size/price increments and DSV
metadata. Missing bars stay missing; every Instrument/Session must have explicit
status. MVP-4 is single Venue, single currency, long-only and full market fills.
`read` rejects incompatible schemas, DSV mismatches, duplicate or unknown keys and
missing specifications/statuses. Packet prices, Sessions, status and lot sizes must
match the platform execution inputs before a Backend starts.

`from_dataset_with_lots` resolves the complete Run's identity-bearing lot-size map
before packet validation, including explicit overrides of absent DSV lot sizes.
It changes only the effective packet's Instrument lot field, not the immutable DSV.
Experiment records both the source kind and resolved values in the Run Spec.

## Session protocol (`prajna-adapter@1`)

`Backend::begin_run` receives the packet and the registered platform cost table once.
A transport implementation may persist the packet and use the `begin_run` / `ready`
messages; this crate does not launch a process. `exchange` takes and returns one
JSON line. Open/closed acknowledgements and Run-end acknowledgement are required:

1. T close: the shared execution state receives a Decision.
2. Next Session open: Rust sends `open` with date, UTC-ns string and synthetic
   `raw_open`, `bid`, `ask`, `tradable` quotes. Quotes use the shared scale-18
   half-even arithmetic: bid/ask = raw open times (1 minus/plus registered slip).
   A carried valuation price never makes a missing bar tradable.
3. After `opened`, Rust plans VP-specific orders from the authoritative ledger,
   sorted account-wide by sell/buy, Instrument and VP. Each `order` includes
   string order/VP IDs, Instrument, side, exact quantity and submission time.
4. Backend returns `fill` or `reject`. Identity, full quantity and open time must
   match; partial/stale/unmapped responses abort explicitly. Platform fees are
   recomputed from actual execution notional, including minimum commission and tax.
   Slippage is embedded in execution price, never deducted twice. Backend `fee`
   is retained only as a diagnostic (the fake reports zero).
5. Rust marks and verifies both layers at open-after and close; `close` / `closed`
   completes the Session. `end_run` / `done` completes the Run.

All numeric protocol fields are strings. Decimal fields require plain decimal
notation with at most 18 fractional digits; JSON numeric tokens, exponents,
out-of-range and inexact values are rejected. Instrument increments express precision;
the neutral packet does not silently quantize prices to a Backend's tick.

Rejected orders retain their submission trace but produce no Fill, cost, cash or
position mutation. Rejected buy legs follow the shared Skip/Retry policy; entry-only
also retains unentered targets. A rejected sell requeues the original target and
suppresses the remaining orders for that VP in that Session, so provisional sell
proceeds cannot fund buys. The next open replans from actual holdings. New close
Decisions replace pending targets normally. All actual sell fills settle before buy planning. Full-target quantities retain
Fast Event's pre-open-equity basis; entry-only budgets use actual post-sell cash.
Each buy is lot-capped against cash after preceding actual Fill/Reject facts,
including buys whose provisional budget would have been zero. Different Backend
prices therefore feed both same-Session affordability and the next Session's ledger.
Any nonpositive/adversely-invalid price, negative cash or exact
two-layer rounding discrepancy aborts rather than fabricating a fill or balance.

## Events and evidence

`AdapterResult.adapter_events` records synthetic quotes and platform gate events.
`backend_events` records returned Fill/Reject messages separately, including fee
diagnostics. A platform gate is not a native Backend rejection.
`FakeBackend::reject_orders` configures deterministic rejection by order ID;
its Session/order state resets per Run, without cross-VP netting.

Rust tests cover scale-18 JSON and Parquet round trips, immutable packet/status
validation, S1/S2/S3 Decision tapes (3x10 and nonempty MA20/60 3x130 S3 fills),
costed full-target parity with Fast Event, two independent VPs, exact daily
two-layer identities, buy Skip/Retry, entry-only retry, rejected-exit replanning,
canonical platform fees, same-Session native-price/rejection-driven cash resizing,
both entry-only budgets and malformed Backend responses.
Existing Research tests retain independent Python golden comparison. Fake equality
alone does not prove independent quantitative correctness, PIT, total return,
native halt handling, worker transport or #133 A15 acceptance.
