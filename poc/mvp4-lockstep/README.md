# MVP-4 Nautilus lockstep feasibility spike (#137)

This is a protocol prototype and real Backend probe for [#137](https://github.com/ZombieIce/PrajnaQuant/issues/137), implementing the synchronous strategy-shim shape allowed by [#133 A6](https://github.com/ZombieIce/PrajnaQuant/issues/133) and [ADR 0020](../../docs/decisions/0020-accurate-event-adapter-and-ledger-authority.md).
It is **not** a platform Accurate Engine, Rust Adapter, shared ExecutionState integration, production worker protocol, or A15 golden comparison. The Python controller stands in for Rust solely for this hand-worked fixture. The worker never decides quantities, weights, eligibility, or accounting.

## Reproduce

Use a separate CPython 3.12 environment (the recorded run is macOS ARM64).
Do not install these requirements into the POC-0 rc5 environment.

```sh
python3.12 -m venv .venv-mvp4
.venv-mvp4/bin/python -m pip install -r poc/mvp4-lockstep/requirements-nautilus.txt
.venv-mvp4/bin/python -m unittest discover -s poc/mvp4-lockstep -v
.venv-mvp4/bin/python poc/mvp4-lockstep/controller.py
.venv-mvp4/bin/python poc/mvp4-lockstep/controller.py --halt
.venv-mvp4/bin/python poc/mvp4-lockstep/capture.py --output /tmp/mvp4-observations.json
```

Dependencies are independently locked to `nautilus_trader==2.0.0rc6` and `pyarrow==21.0.0`.
PyPI's `2.0.0` prerelease list was checked at ticket startup on 2026-10-12; rc6 was the latest.
The POC-0 rc5 requirements and evidence are unchanged.
The separate `Full` workflow job runs these real integration tests; missing dependencies, native stderr, timeout, or a skipped test fail validation. The lightweight job does not install rc6.

## Selected lockstep shape and event sequence

1. Controller writes one Arrow IPC file and starts one independent Python worker.
2. Worker loads the file once, registers three Equity instruments, and queues synthetic open and close QuoteTicks plus InstrumentStatus events in one BacktestEngine.
3. At each Session open, all three quotes update before a timer barrier at `open + 1 ms`. InstrumentStatus is delivered at `open + 0.5 ms`, before orders.
4. The shim emits `session_open` and **blocks on stdin** awaiting that Session's `orders` batch. The controller computes the batch from its ledger of previous actual Fill/Reject messages, not a precomputed run-long order tape.
5. The shim submits exactly those native market orders. Native callbacks emit attributed Fill/Reject messages. The engine continues to that Session's close and sends Backend account diagnostics at `close + 1 ms`.
6. The next open blocks again, using the **same engine state**. No reset occurs between Sessions.

The controller has a 30-second receive timeout and fails on worker disconnect, malformed messages, unresolved orders, attribution mismatch, or native account disagreement. The timer's datetime conversion uses integer microseconds, never float timestamps; the fixture's Session boundaries are microsecond-aligned. The synthetic 1 ms barrier is an Adapter construction, not a real execution latency model.

The alternative multiple-`run(end=...)` shape remains **Unknown / not tested**: the permitted shim shape already works, so the spike does not need it. If the shim is unavailable in another version, do not substitute Python execution logic or open-loop replay.

## Prototype protocol (`mvp4-lockstep-spike@1`)

This is a trusted local-file prototype, not a network API or final Domain schema.

### Once per run

`start` contains `protocol`, an absolute `packet` path, `initial_cash`, a boolean `reset_probe`, and a `decimal_probe` codec vector. Arrow IPC columns are:

| Column | Arrow type | Meaning |
| --- | --- | --- |
| session_id, instrument_id | utf8 | Stable IDs |
| open_ns, close_ns | int64 | UTC nanoseconds |
| open, close | utf8 | Exact decimal price strings |
| execution_status | utf8 | TRADING or HALT (probe injection) |

Schema metadata carries the protocol version, Instrument specifications (ID, USD currency, price precision, price increment, lot size), buy/sell slippage decimal strings, and the codec vector. The bars table is intentionally minimal: it represents complete open/close observations, not a platform OHLCV/PIT contract.
This fixture supports only SIM/USD long-only Equity, integral quantities, complete three-instrument Sessions, and no missing bars.
It does not infer missing status or accept an unmapped status.

### Each Session

```json
{"type":"session_open","session_id":"s2","ts_event":"1767843000001000000"}
{"type":"orders","session_id":"s2","orders":[{"order_id":"s2-c","virtual_portfolio_id":"alpha","instrument_id":"C.SIM","side":"BUY","quantity":"16"}]}
{"type":"fill","session_id":"s2","order_id":"s2-c","virtual_portfolio_id":"alpha","instrument_id":"C.SIM","side":"BUY","quantity":"16","price":"30.30","backend_fee":"0.00","ts_event":"1767843000001000000"}
```

`reject` returns the original Order fields and a native reason; native OrderDenied is explicitly distinguished when present. `backend_status` records observed native status callbacks. `session_close` contains diagnostic `backend_cash`, `backend_positions`, and timestamp. `ready` records Backend identity and exact codec round trips; `done` terminates the run. The optional reset probe emits `reset` between two runs on the same loaded engine.
Protocol validation failures emit `error` and terminate nonzero; they are not native order rejections. This matters because rc6 can log a Python callback exception and otherwise let `run()` finish normally.

All JSON-line prices, quantities, fees, cash and timestamps are strings. Decimal strings are parsed without f64, allow at most 18 fractional digits, and must have magnitude below `10^20` (Domain scale-18 mantissa bound). Codec vectors round-trip through both Arrow metadata and the subprocess stdio boundary, including `1e-18` and the largest positive scale-18 Domain value. Codec preservation does **not** imply that every vector is a valid Equity order or Backend Money value.

Order IDs are unique within a Run and retain VirtualPortfolioId through callbacks. The fixture has alpha and beta VP ledgers; the Backend nets positions only as its account-level diagnostic. This is not the A15.4 same-instrument 2-VP acceptance fixture. Large/partial fills, nonterminal order states, batching acknowledgments, and all platform cost rules remain for the actual Adapter; this prototype fails if an order does not reach a single Fill or Reject before close.

## Hand-worked 3 instruments x 3 Sessions

Initial cash: alpha `600`, beta `400`, total `1000` USD. Commission and tax are zero; rc6 requires an explicit FeeModel, so the worker uses an explicit zero FixedFeeModel. Slippage is an Adapter-constructed 1% spread: ask = raw open x 1.01, bid = raw open x 0.99. It is already in the Fill price and is **not deducted again**.

| Session | Raw open A/B/C | Close A/B/C | Orders and actual fills |
| --- | --- | --- | --- |
| s1 | 10 / 20 / 30 | 11 / 21 / 30 | alpha buys A 10 @ 10.10; beta buys B 5 @ 20.20 |
| s2 | 12 / 20 / 30 | 12 / 19 / 31 | alpha sells A 10 @ 11.88; buys C `floor(499 / 30.30) = 16` @ 30.30 |
| s3 | 13 / 18 / 32 | 14 / 18 / 32 | beta sells B 5 @ 17.82; alpha sells C 16 @ 31.68; buys A `floor(133 / 13.13) = 10` @ 13.13 |

The opening batch sizes use available VP cash from **previous actual fills**, conservatively excluding same-batch sale proceeds. This is only the spike controller rule, not Fast Event sizing or a proposal to change it. The initial s1 intents are seeded before the run; no close-derived same-Session execution is claimed.

| Close | Cash | Positions | Close valuation | NAV |
| --- | --- | --- | --- | --- |
| s1 | 798.00 | A 10, B 5 | 110 + 105 | 1013.00 |
| s2 | 432.00 | B 5, C 16 | 95 + 496 | 1023.00 |
| s3 | 896.68 | A 10 | 140 | 1036.68 |

Cash derivation: `1000 - 101 - 101 = 798`; `798 + 118.80 - 484.80 = 432`; `432 + 89.10 + 506.88 - 131.30 = 896.68`.
Final VP cash is alpha `508.58`, beta `388.10`; A 10 belongs only to alpha.
Every close compares controller cash and aggregate positions exactly against native account diagnostics; each hand literal checks `NAV = cash + sum(quantity x close)`. This is synthetic feasibility evidence, not Rust ledger authority or real-market/PIT/total-return validation.

## Observations

Raw protocol traces, environment, source hashes and native stderr are in [observations.json](observations.json). It was captured from a dirty implementation worktree on the recorded base revision; source hashes identify the executed Python files. The committed tests retain the independent hand literals.

| Question | Status | Evidence / limitation |
| --- | --- | --- |
| Synchronous stdio shim lockstep | 已证实 | Three sessions, seven native fills, actual Fill-derived next-session sizes, one engine |
| `reset()` | 已证实 | Same loaded engine replays 3x3 twice; all fills, order IDs, cash, positions and NAV match; no native error logs. Not a throughput or arbitrary-state reset guarantee |
| InstrumentStatus(HALT) native rejection | 已证实 | C receives HALT before each open barrier; three external buys become native OrderRejected with `Market C.SIM is CLOSED` reason; no fills, cash remains 1000 |
| Price / Equity / QuoteTick scale 18 | 已证实 | `1.123456789012345678` has exact raw `1123456789012345678`; Equity precision 18 and QuoteTick preserve it |
| Scale 19 Price | 不支持 | Native `WEI_PRECISION (18)` ValueError |
| Arbitrary synthetic bid/ask at scale 18 | 不支持 | `1.123456789012345678 x 1.01 = 1.13469135690246913478` needs scale 20. No silent rounding: conversion fails explicitly |
| Off-tick synthetic price | 不支持 | Adapter rejects `10.01` for tick `0.05`, and checks declared instrument precision |
| Multiple `run(end=...)` lockstep | Unknown | Not needed / not tested |

rc6's observed maximum is **18**, despite a `FIXED_PRECISION` constant reporting 16; neither the spec's earlier precision hypothesis nor the constant alone is evidence of actual limits. Precision capacity does not waive Instrument tick constraints. Selecting a rounding policy/tolerance for unmappable synthetic prices requires the worker/Adapter ticket to register the difference under #133 A15.3; this spike does not decide it.

The hand trace counts Adapter constructions separately (9 open quotes, 9 close quotes, 9 status injections) from Backend callbacks (9 statuses, 7 fills). The HALT trace has the same construction counts and 9 native statuses / 3 native rejects. HALT injection intentionally bypasses the future platform eligibility gate to probe Backend behavior; it is not evidence that platform orders should be sent during a halt.

**Recommended next step:** implement the Rust-facing worker/Adapter against ADR 0020 using this closed-loop shape, explicitly handling unmappable synthetic prices and keeping platform ExecutionState/cost/ledger authority in Rust. Independent review of this spike comes first.
