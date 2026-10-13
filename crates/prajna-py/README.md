# prajna-py

Local Python-hosted PyO3 extension for [ADR 0021](../../docs/decisions/0021-python-strategy-entry-and-decision-boundary.md).
Typed Experiment definitions invoke the existing Rust parser, identity,
execution and ResultLevel storage APIs. Rust remains the only execution and
performance authority. Python strategy references can be defined and identified,
but their execution, Decision/As-of View and Arrow research/result readers are
not implemented here.

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
cargo build -p prajna-experiment --bin prajna-experiment --example prepare_fixture --locked --offline
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

## Experiment API

Prepare the checked-in synthetic dataset and its Static Universe:

```sh
target/debug/examples/prepare_fixture /tmp/prajna-lake /tmp/prajna-cli.json
```

Then construct the same S2 Experiment using typed objects:

```python
import json
from pathlib import Path
import prajna

definition = json.loads(Path("/tmp/prajna-cli.json").read_text())
experiment = prajna.Experiment(
    dataset_version=prajna.DatasetVersion(definition["dsv"]),
    universe=prajna.Universe(definition["universe"]),
    venue=prajna.Venue("SYNTH"),
    strategy=prajna.RustStrategy.S2_ROTATION,
    engine=prajna.Engine(prajna.EngineKind.VECTOR),
    costs=prajna.Costs(0.001, 10.0, 10.0, 0.0, 0.0),
    parameter_space=prajna.Grid(definition["parameter_space"]["grid"]),
)
Path("/tmp/prajna-python.json").write_text(experiment.to_json())
identity = experiment.describe("/tmp/prajna-lake")
print(identity.experiment_id, identity.runs)
result = experiment.run(
    "/tmp/prajna-lake", repository=Path.cwd(),
    level=prajna.ResultLevel.FULL, threads=2,
)
print(result.action, result.path, result.manifest)
```

- `to_definition()` / `to_json()` produce ordinary CLI input JSON, not a second
  canonicalizer. No seed is accepted. `describe(lake)` validates against local
  DSV/Universe data and returns Rust's Experiment ID, sorted/deduplicated Run
  definitions and canonical identity JSON. The latter includes normalized float
  bits and is **not** an input JSON round-trip format.
- `RustStrategy` supports S2 rotation, S1 buy-and-hold and S3 MA crossover.
  `Grid(axes)` and `List(combinations)` accept JSON parameter dictionaries;
  Rust validates built-in parameter schemas and canonicalizes identities.
  Non-string dictionary keys, non-JSON objects and nonfinite floats fail explicitly.
- `Engine` selects Vector, Fast Event or Accurate Event. Event engines require
  `FastEventSettings`, including decimal-string capital and costs, portfolios and
  `LotSizeSource()` (instrument lots) or `LotSizeSource(values)` (explicit lots).
  Accurate additionally requires `Backend(name, version)`. Its runtime worker
  paths are supplied as the paired `run(backend_python=..., backend_worker=...)`
  arguments and do not enter identity; Backend name/version do.
- Availability defaults to `AvailabilityAssumption.NONE`; the explicit alternative
  is `TREAT_UNKNOWN_AS_TS_CLOSE`. `sessions_per_year` defaults to 252.
  `unfilled_entry` is optional and preserves Rust's strategy-specific defaults.
- `run()` releases the GIL for Rust parsing, expansion, provenance capture,
  parallel execution and result storage. `repository` defaults to the current
  directory; omitted `threads` uses Rayon defaults, including `RAYON_NUM_THREADS`.
  `ExecutionResult` exposes the existing manifest, directory, invocation statistics
  and action. Individual failed Runs remain explicit manifest statuses, with
  errors in the stored Summary; no Python success-shaped replacement is made.
- `ResultLevel.SUMMARY` is the default; `STANDARD` and `FULL` save the same tables
  as CLI. Replays recompute and verify every saved table's logical hash, including
  scalar Float64 bits. Requesting more detail on an existing Execution raises
  an explicit error: use the existing CLI `--promote` command. This ticket does
  not add a Python promotion or Arrow table-reading API.
- Definition/data errors raise `ValueError`, provenance I/O errors `OSError`,
  and execution/provenance/replay errors `RuntimeError`. Native argument conversion
  can raise `TypeError` or `OverflowError`.

`PythonStrategy("research.rotation", "1")` serializes as
`"strategy": "research.rotation@1"`. Grid/List parameter objects may contain nested
JSON values; Rust tags floats and strings when canonicalizing to prevent type
collisions. Both CLI `run` and Python `run()` reject these references explicitly
with `requires Python host` until the two-stage Python Decision execution ticket
is complete. Defining a reference does not run Python code or capture Python
strategy source/environment provenance.

## Validation

With the above venv active, from the repository root:

```sh
cargo fmt --all -- --check
cargo clippy -p prajna-py --all-targets --locked --offline -- -D warnings
cargo test -p prajna-py --locked --offline
cargo test -p prajna-experiment --locked --offline
cargo build -p prajna-experiment --bin prajna-experiment --example prepare_fixture --locked --offline
python -m unittest discover -s python/tests -v
cargo check -p quant-research --no-default-features --features b3-pyo3 --all-targets --locked --offline
```

The Python tests use the installed real extension and explicitly built CLI/fixture
binaries; missing binaries fail with the required build command, never skip.
They cover S1/S2/S3 Grid/List identities and Full-table exact replay against CLI
on the 64×252 fixture (Vector and Fast Event lot), all ResultLevels, invalid
inputs, Python-reference rejection, and Python-thread progress strictly during
real Rust experiment execution as well as the smoke wait. Exact replay uses the
existing Rust scalar-level logical-hash checks, not approximate metric comparisons.
Fast Event manifests verify cash/quantity accounting; Vector remains a simplified
weight-return model. No real-market, PIT or total-return claim follows.
The required `CI / lightweight` job also guards normal/build/dev dependencies
against DuckDB, the legacy warehouse and `quant-research`, builds the extension
in a fresh Python 3.12 venv, and runs these Python tests.
