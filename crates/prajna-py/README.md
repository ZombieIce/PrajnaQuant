# prajna-py

Local Python-hosted PyO3 extension for [ADR 0021](../../docs/decisions/0021-python-strategy-entry-and-decision-boundary.md).
In addition to the smoke API, this crate generates and validates a single Run's
close-session Decision sequence. It reuses Rust panel loading and registered
Factor computation; it does not execute a backtest or expose accounts.
Read APIs reuse Rust's research panel, registered Factor definitions, persistent
Factor Cache, and Experiment result storage. Experiment construction, Engine
integration and Execution provenance are separate MVP-5 tickets. This is not a
second research or execution engine.

## Environment and build

Use GIL-enabled CPython 3.12 for the tested development path, a Rust toolchain
compatible with the workspace, and maturin 1.15.0. The extension targets
`abi3-py312` (Python >= 3.12); newer interpreters are not certified by this ticket.
Run from the repository root:

```sh
python3.12 -m venv .venv
source .venv/bin/activate
python -m pip install 'maturin==1.15.0'
maturin develop --locked --offline
python -c 'import prajna; print(prajna.__version__, prajna.smoke_test())'
```

The expected output is `0.1.0 prajna-rust-ok`. `maturin develop` builds an
abi3 wheel and installs the mixed `python/prajna` package as editable in the
active venv. This is a local-only workflow; do not publish to PyPI.
If the offline build reports missing Cargo sources, run `cargo fetch --locked`
once to populate the cache, then repeat the offline command.

PyO3 stays at **0.29.0**, matching B3 and avoiding another PyO3 version in the
workspace. Maturin **1.15.0** is pinned in the build backend and CI.
The crate deliberately omits PyO3's `extension-module` feature: maturin sets
`PYO3_BUILD_EXTENSION_MODULE` for extension builds, while ordinary Cargo
tests and the existing `quant-research/b3-pyo3` embedding path retain libpython
linking. B3's source, feature declaration and dependency declaration are unchanged.
PyArrow is a runtime dependency (`>=18,<24`); Polars is not a Python dependency.

## Public smoke API

- `prajna.__version__`: crate version, supplied by Rust.
- `prajna.smoke_test(duration_ms=0) -> str`: wait in Rust for the specified
  nonnegative `u64` milliseconds and return `prajna-rust-ok`. The call releases
  the GIL via `Python::detach`, including for the default zero-duration call.
  Negative/overflowing integers raise `OverflowError`; non-integer arguments
  raise `TypeError`. The wait is a concurrency probe, not a backtest or benchmark.

## Single-Run strategy API

```python
import prajna

strategy = prajna.Strategy(
    strategy_id="research_example",
    version="1",
    bar_columns=("close",),
    factors=(prajna.FactorDependency("momentum", "momentum", {"n": 5}),),
    lookback_sessions=6,
    rebalance_every=2,
)

def decide(view, params, prev_decision):
    # Example: remain in cash. This is a full Decision, not a skipped call.
    return prajna.Decision({})

decisions = prajna.generate_decisions(
    lake_root="/path/to/lake",
    dsv="dsv:sha256:<manifest hash>",
    venue="SYNTH",
    members=("A.SYNTH", "B.SYNTH"),
    strategy=strategy,
    decide=decide,
    params={},
    availability_assumption="none",
)
```

`Strategy.identity` is `strategy_id@version`; components must be nonempty ASCII
letters/digits/`._-`. Capability must be `Vectorizable`. Bar columns are a unique
subset of `open`, `high`, `low`, `close`, `volume`, `amount`. Factor dependencies
use `FactorDependency(name, kind, params)` with unique nonempty aliases and
strictly checked Rust parameters. The existing read-only API retains
`Factor(kind, params)`; both share the Rust definition parser.

| Kind | Parameters | Rust definition version |
| --- | --- | --- |
| `momentum` | `n >= 1` | 1 |
| `volatility` | `n >= 2` | 1 |
| `trend_filter` | `n >= 1` | 1 |
| `rotation_score` | `short`, `long`, `vol`, `w_s`, `w_l`, `w_v`, optional `trend` | 2 |

Windows, input `close`, availability propagation, missing/status rules and
direction are the registered [Rust Factor contract](../prajna-research/src/factor.rs),
not Python implementations. Temporary callback calculations do not become Factor
Values or enter Factor Cache. This entry computes in Rust without persistent
Factor Cache; cached read APIs are outside this ticket.

Rust starts at the first full `lookback_sessions` window where **each** declared
Factor has at least one member's current `ok` value available by that close. With
no Factor dependencies, the first full window is sufficient. It then calls every
`rebalance_every` Venue Sessions, including later missing-data sessions; it does
not restart the clock or skip an empty view. Both intervals must be positive.
If warm-up never completes, the sequence is empty and no callback runs.

Every invocation must return `Decision(targets, ranked=None)`:

- `targets`: canonical instrument ID to finite, nonnegative binary64 weight;
  sum must be **strictly <= 1**, without clipping, normalization or epsilon.
- `ranked`: optional `(instrument_id, finite_score, positive_rank)` tuples;
  IDs and ranks must be unique. Ranking may include unselected Universe members.
- Every target/ranked ID must belong to this Run's Session Universe, including
  zero-weight entries. Invalid outputs raise a dated `ValueError`; `None` or a
  different return type raises `TypeError`. Python callback exceptions propagate.
- Output `session_date` is assigned by Rust. Getters return defensive copies.
  `prev_decision` is the previous validated intent, **not holdings or fills**.
  Params must be a finite JSON object, canonicalized in Rust and decoded afresh
  each call; mutations cannot carry through params/previous Decision.

## As-of View and trust boundary

The frozen Rust `AsOfView` exposes only:

- `session_date`, `ts_close` (UTC nanoseconds), `universe`, and
  `availability_assumption`;
- `sessions`: exactly `lookback_sessions` Venue Session dates/close times;
- `bars`: keys `instrument_id`, `session_date`, `available_at` plus declared
  Bar columns, as `pyarrow.Table`; D7 prices/quantities retain Decimal128 scale 18;
- `factors`: alias to the declared Rust Values `pyarrow.Table`s (keys, `value`,
  `available_at`, `status`);
- `execution_status`: known D7 status rows in the same window.

Rows must be in the window, in the current Universe, and have
`available_at <= decision close`. Late historical rows only enter after they
become known. Unknown availability is excluded by default. The explicit
`treat_unknown_as_ts_close` Run assumption permits it and is visible on the view;
raw unknown timestamps remain null rather than inventing publication evidence.
D7 Bar availability applies to the whole row, not separately timed open/close
fields. Non-ok Factor values remain null; nothing is filled from future rows.
Missing bars remain absent, not zero-priced or executable. The DSV must provide
an execution_status table; a missing table is an explicit error.

This implementation supports **Static Universe only**: explicit members validated
against the Venue's DSV instruments. It does not establish historical membership,
real-market PIT, corporate actions or dividend total returns.

**Safe buffers, zero-copy export:** slicing the original Arrow panel would retain
full buffers, allowing Python `buffers()` to read excluded future/out-of-window
rows. Rust therefore projects and compacts only authorized rows/columns first.
The resulting compact buffers are exported without copying through Arrow
PyCapsule/`RecordBatchReader`, and are read-only in PyArrow. The source-panel
boundary is **not** zero-copy; this owner-approved safety copy is intentional and
must be included in future throughput measurements.

Callbacks run serially under the GIL and must be deterministic pure functions,
without random sources or cross-call mutable state. This is a data/argument
boundary, **not a Python sandbox or purity detector**: arbitrary trusted Python
can still access its globals, files or independently loaded data. No account,
holdings, orders or fills are passed. The final close may generate a Decision;
next-session open execution, cash sizing, costs and pending state belong to the
future shared Engine phase, not this API.

## Validation

With the above venv active, from the repository root:

```sh
cargo fmt --all -- --check
cargo clippy -p prajna-py --all-targets --locked --offline -- -D warnings
cargo test -p prajna-py --locked --offline
python -m unittest discover -s python/tests -v
cargo check -p quant-research --no-default-features --features b3-pyo3 --all-targets --locked --offline
```

The Python tests use the installed real extension: import/version, smoke result,
argument errors, Python-thread progress strictly during a 300 ms Rust call, and
strategy declaration/scheduling, Rust Factor warm-up, invalid Decision rejection,
as-of availability, undeclared/future indexing and compact Arrow buffer checks.
`strategy_fixture` is a test-only example that publishes immutable synthetic
DSVs with explicit delayed/unknown Bar/status availability overrides. It is not
a production ingestion API or a Python Lake writer. Tests resolve their fixture
paths from the checkout, not the caller's current directory.
Cargo tests build the Rust test harness; behavioral binding coverage is in Python.
The required `CI / lightweight` job also guards normal/build/dev dependencies
against DuckDB, the legacy warehouse and `quant-research`, builds the extension
in a fresh Python 3.12 venv, and runs these Python tests.

## Read-only research API

All three reads return `pyarrow.Table` through the Arrow C stream **PyCapsule**
protocol. PyArrow (>=18, <24) is a package dependency; Python Polars is not.
Rust I/O, filtering and Factor computation run with the GIL released. Existing
Rust Polars columns are converted to arrow-rs buffers before capsule transfer;
the transfer itself does not serialize via JSON or IPC. Dataset reads currently
load the existing complete venue panel before filtering; this is not an
out-of-core or Parquet predicate-pushdown API.

```python
import prajna

panel = prajna.read_dataset(
    lake, dsv, universe, "SYNTH",
    columns=["instrument_id", "session_date", "close", "has_bar"],
    start="2026-01-05", end="2026-01-16",
)
factor = prajna.Factor("momentum", {"n": 2})
values = prajna.read_factor(lake, dsv, universe, "SYNTH", factor)
summary = prajna.read_run(
    lake, experiment_id, execution_id, run_id,
    table="summary", result_level="summary",
)
```

- `read_dataset(lake, dsv, universe, venue, *, columns=None, start=None, end=None)`:
  DSV and Universe are their stored content identities, not paths or ad-hoc
  member lists. Dates are ISO dates with inclusive bounds. The table is the
  existing Rust instrument/session grid: missing bars remain null and
  `has_bar=False`. All nine Rust panel columns are available by default,
  including `close_available_at` and exact `open_decimal` / `close_decimal`.
  Invalid columns, empty/duplicate projections, inverted ranges, missing
  Universe members and venue mismatches raise `ValueError`. Valid selections
  with no matching dates return a schema-preserving empty table.
- `Factor(kind, params)`: frozen Rust declaration. Kinds are `momentum`,
  `volatility`, `trend_filter` (each `{"n": <integer>}`), and `rotation_score`
  (`short`, `long`, `vol`, `w_s`, `w_l`, `w_v`, optional `trend`).
  Unknown kinds/parameters and invalid windows/numbers raise `ValueError`.
  Computation, versions, missing rules and availability propagation remain
  those of Rust's [Factor registry](../prajna-research/src/factor.rs).
- `read_factor(lake, dsv, universe, venue, factor, *, availability_assumption="none")`:
  reads or computes the **entire DSV** for the stored Universe using the same
  `FactorCache::get_or_compute` and `FactorKey::for_tree` as the CLI. The explicit
  alternative assumption is `treat_unknown_as_ts_close`; no assumption is
  inferred from dates. The five columns are `instrument_id`, `session_date`,
  `value`, `available_at` (UTC nanoseconds), `status`. Schema metadata contains
  `prajna.fv` and `prajna.cache_hit` (true only when the whole dependency graph
  was reused without computation). A corrupt entry fails, never silently
  recomputes. This is the only read API allowed to write: **Rust** persists
  Factor Cache entries, never normalized data, Universe definitions or results.
- `read_run(lake, experiment, execution, run, *, table, result_level="summary")`:
  requires an explicit Execution identity because the same Run Spec can occur
  in multiple code/environment Executions. `result_level` (`summary`,
  `standard`, `full`) is a minimum required retention level for this Run,
  including promoted Runs, not an execution/promotion request. Returns only
  the selected Run's summary row, or the named saved table with its original
  storage schema (including existing JSON payload columns). Full tables vary
  by Engine; names are recorded in `execution.json`.
  `ResultNotSavedError` distinguishes omitted tables/levels from
  `EngineNotProducedError` for known tables the Engine does not produce.
  Both subclass `ValueError`; unknown names/IDs, I/O and integrity failures
  also raise `ValueError` with context. Saved logical hashes are checked before
  returning values. A legitimately saved empty table remains valid; missing
  tables never become empty-table fallbacks.

The Python tests publish a checked-in synthetic fixture through Rust and compare
every returned column/value with independently written Rust Parquet. They cover
Universe/date/column selections, all four FactorKinds, availability and null
statuses, cold/warm cache identity and CLI reuse, Vector Summary/Standard/Full
and Fast Event Full tables, explicit absence errors, and corruption rejection.
These are read/interop tests, not a new PIT, portfolio-accounting or real-market
validation claim. Full research panels are **not** strategy-safe As-of Views.
