# prajna-py

Local Python-hosted PyO3 extension for [ADR 0021](../../docs/decisions/0021-python-strategy-entry-and-decision-boundary.md).
Read APIs reuse Rust's research panel, registered Factor definitions, persistent
Factor Cache, and Experiment result storage. Experiment construction, Decision
callbacks and restricted As-of Views are not yet exposed. This is not a second
research or execution engine.

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

## Public smoke API

- `prajna.__version__`: crate version, supplied by Rust.
- `prajna.smoke_test(duration_ms=0) -> str`: wait in Rust for the specified
  nonnegative `u64` milliseconds and return `prajna-rust-ok`. The call releases
  the GIL via `Python::detach`, including for the default zero-duration call.
  Negative/overflowing integers raise `OverflowError`; non-integer arguments
  raise `TypeError`. The wait is a concurrency probe, not a backtest or benchmark.

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
argument errors, and Python-thread progress strictly during a 300 ms Rust call.
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
