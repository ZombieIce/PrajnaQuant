# prajna-py

Local Python-hosted PyO3 extension for [ADR 0021](../../docs/decisions/0021-python-strategy-entry-and-decision-boundary.md).
Typed Experiment definitions invoke the existing Rust parser, identity,
execution and ResultLevel storage APIs. The single-Run strategy API generates
and validates close-session Decisions using restricted As-of Views and Rust
Factors. Matching Python strategy declarations and callbacks drive two-stage
Experiment execution with Python source/environment Execution provenance. Read APIs reuse Rust's research panel, registered Factor definitions,
persistent Factor Cache and Experiment result storage, returning `pyarrow.Table`.
Rust remains the only execution and performance authority.

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
cargo build -p prajna-experiment --bin prajna-experiment --example prepare_fixture --example acceptance_fixture --example pit_fixture --locked --offline
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
  not add a Python promotion API; saved tables can be read with `read_run` below.
- Definition/data errors raise `ValueError`, provenance I/O errors `OSError`,
  and execution/provenance/replay errors `RuntimeError`. Native argument conversion
  can raise `TypeError` or `OverflowError`.

`PythonStrategy("research.rotation", "1")` serializes as
`"strategy": "research.rotation@1"`. Grid/List parameter objects may contain nested
JSON values; Rust tags floats and strings when canonicalizing to prevent type
collisions. CLI `run` and Python `run()` without a matching declaration/callback
reject these references explicitly with `requires Python host`. Defining a reference does not run Python code or capture Python
strategy source/environment provenance.

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

Static Universes retain explicit members validated against the Venue DSV.
The optional MVP-6 PIT path below uses the frozen Historical Catalog instead;
synthetic PIT support does not establish real-market coverage or total returns.

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
the Experiment execution API, not this single-Run generator.

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

They also cover strategy declaration/scheduling, Rust Factor warm-up, invalid Decision rejection,
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
  Vector and Fast Event retain `corporate_actions` at Standard and Full levels,
  including a saved empty table for runs without corporate actions; Summary
  does not retain it. Accurate Event does not produce this table.
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


## Python strategy Experiment execution (#160)

Supply the matching declaration and pure function to the existing Experiment API:

```python
experiment = prajna.Experiment(
    dataset_version=prajna.DatasetVersion(dsv), universe=prajna.Universe(universe_id),
    venue=prajna.Venue("SYNTH"), strategy=prajna.PythonStrategy("allocation", "1"),
    engine=prajna.Engine(prajna.EngineKind.VECTOR), costs=prajna.Costs(),
    parameter_space=prajna.Grid({"weight": [0.25, 0.5]}),
)
strategy = prajna.Strategy("allocation", "1", rebalance_every=20)

def decide(view, params, previous):
    return prajna.Decision({view.universe[0]: params["weight"]})

result = experiment.run(lake, repository=repo, strategy=strategy, decide=decide,
                        level=prajna.ResultLevel.FULL, threads=2)
```

Rust expands and deduplicates canonical Run identities. Stage one calls the
#159 generator serially under the GIL for each Run, with original JSON parameter
values (identity float/string tags never enter callbacks). Only owned Rust
Decisions cross `Python::detach`; stage two reuses the existing Rayon Run pool
or Accurate worker pool and shared execution/ledger/storage path. Summary,
Standard and Full retain the existing Engine-specific tables. Invalid Decisions and callback exceptions
are retained as failed Runs; valid combinations continue. Experiment/declaration
errors abort before publication.
A `seed` parameter is rejected. Python purity/determinism remains a trust contract.
All parameter sequences remain in memory until execution begins; there is no
streaming/memory-capacity claim.

Python callback executions also return `ExecutionResult.statistics["python"]`
with invocation-only wall timings in milliseconds:

- `stage1_ms`: serial Decision preparation, including per-Run panel reads, Rust
  Factor computation, As-of Views, Python callbacks and Rust Decision validation.
- `stage2_ms`: the entire detached execution/storage call, including Rust
  provenance capture, execution and persistence/replay verification. The existing
  `statistics["run_time_ms"]` isolates the Run execution interval within it;
  `stage2_ms` must not be interpreted as pure Rayon compute time.
- `asof_view_ms`: one sample per constructed callback View, in canonical Run /
  Session order, including safe row/column compaction, sessions/status/factor
  tables, Arrow export and allocation of the Python View object. It excludes
  shared per-Run preparation, parameter decoding, the callback and validation.

These timings do not enter the manifest, result hashes or any identity. They
are regenerated on replay; failed callbacks may leave View samples for that
failed Run. External end-to-end timing is still needed for Python provenance
(`pip freeze`), Experiment parsing and returned JSON conversion. The
[MVP-5 record-only benchmark](../../poc/mvp5-benchmark/README.md) measures that
real public API in release mode and compares its boundaries with historical B3.

Execution metadata adds `python`: defining module file SHA-256, function code
SHA-256 (`marshal.dumps(__code__)`), function name, declaration JSON, `sys.version`,
and sorted output of `sys.executable -m pip freeze --all`. File-less Notebook
functions use the code digest as their source digest and set `reproducible=false`.
The snapshot covers installed packages, including editable/direct URL entries;
it is an environment snapshot, **not artifact hashes or a resolver lock**.
Only the defining module is file-hashed, not transitive local imports, globals,
closure values, dynamic imports or external files. Functions must be trusted,
pure, versioned and dependency-managed; metadata is not a sandbox or proof of
complete dependency closure. Source paths and code filenames are identity-bearing.
Source/declaration/environment changes create a new Execution while preserving
Experiment/Run identities. Same-Execution replay regenerates Decisions and verifies
all retained logical hashes, rejecting disagreement without overwriting results.

Accurate requires both `backend_python` and `backend_worker` as before. The
interpreter path preserves its venv symlink; a fresh process per Run receives no
Python host objects or module state. `-E -s` ignores Python environment overrides
and user-site packages while permitting the worker's trusted sibling imports.
Each child has a separate process group and temporary packet directory; existing
deadlines and group cleanup apply. Other OS environment variables remain inherited;
this is process/import isolation, not an OS security sandbox. Real Nautilus tests
run only in Full (macOS ARM64, pinned 2.0.0rc6), with skips forbidden there.

## MVP-5 synthetic acceptance

After the build commands above, run:

```sh
.venv/bin/python -m unittest discover -s python/tests -v
```

`python/tests/test_acceptance.py` is included in lightweight CI. The test-only
`acceptance_fixture` example publishes the checked-in `dataset-v1` (3×10)
and `b2-s2-scale-64x252-v2` fixtures and exports Decisions from the public
Rust built-in strategies. Python implementations live in
`python/tests/acceptance_strategies.py`, never in the Rust strategy enum.

- S2 reads only Rust `RotationScore` Values through the As-of View, sorts
  descending with instrument-ID tie breaking, and emits equal-weight TopK.
  Both TopK=1/3 combinations are checked at every Decision Session.
- S3 computes temporary MA20/60 crossover signals from permitted closes and
  its previous intent. These are not Factor Values or cached Python Factors.
  The 64×252 fixture has 192 post-warmup Sessions and changes its held set on
  **every** one; its full Decision sequence can therefore match the Rust
  change-only emitter. The 10-Session fixture cannot warm up MA60 and both
  emit no Decisions. This does **not** establish general MA strategy equivalence:
  fixed Python scheduling still requires full Decisions on unchanged targets,
  whereas the Rust built-in emits only on held-set changes. Other datasets
  can consequently rebalance differently.
- CLI/Python S2 definitions match Experiment and Run Spec identities and all
  saved Summary/Standard values. Python S2/S3 strategy references deliberately
  have different identities from built-ins; comparisons exclude only Run ID
  and built-in parameter metadata, retaining every result/metric column and
  every saved Engine-specific table. Float comparisons preserve exact bits.
- End-to-end Runs test hidden future rows, delayed/unknown availability,
  declared columns, compact Arrow buffers, and failed invalid Decisions
  (negative, overweight, outside Universe, NaN/infinite). Failed Runs keep an
  explicit Summary error and publish no Standard tables.
- File source hash, Python version and the actual pip-freeze snapshot are
  checked; Notebook non-reproducibility is tested in `test_execution.py`.
  Replay compares each Run's values and retained hashes.

The single real Python-hosted Accurate smoke in `test_execution.py` now uses
the S2 rewrite and retains the hostile-PYTHONPATH isolation check. It runs
only in Full and is **not** equivalence evidence. These checks establish
synthetic interface/semantic parity only, not real-market correctness,
historical PIT, total returns, or production suitability. No execution,
accounting, cost or time contract is changed.

## MVP-6 PIT Python entry (#178)

`RustStrategy.S2_PIT_ROTATION` and `Experiment(pit=...)` accept the same frozen
`pit` input object as the CLI. Use `Universe(catalog_content_id)`, `Venue("SZSE")`,
`availability_assumption=NONE`, and Vector or Fast Event `full_target + skip`.
Vector requires `Engine(EngineKind.VECTOR, initial_capital="10000")`, serialized
as `vector_pit.initial_capital`. Fast Event uses the existing settings object.
Inputs are rebuilt/validated by Rust; there is no Python Catalog or trading core.
The typed built-in Experiment and the same CLI definition share Experiment,
Execution and Run IDs and every saved logical hash under the same code/environment.

The optional `pit.fixed_universe` object is consumed by the same shared Rust PIT
plan as the CLI, for both the built-in Experiment and Python callbacks: an
explicit valid selector uses fixed-list membership, an omitted field keeps the
versioned MVP-6 default, and an explicit `null` keeps the legacy broad-index
selector. A fixed selector never silently falls back to the broad-index list;
an invalid fixed list fails with the CLI's validation error, and the broad-index
path still requires a versioned list. The minimum included-code check remains in
the shared Engine for CLI and Python-hosted execution.

Python callbacks also accept PIT Experiments using `PythonStrategy(id, version)`;
these retain their own strategy/Execution identities and Python provenance.
Their parameters are the strict PIT S2 object (`lookback`, `top_k`,
`rebalance_every`), which supplies the shared Rust PIT plan and Eligible Set.
Python callback scheduling retains the existing declaration contract: wait for a
full `lookback_sessions` window and each declared Factor's current, known `ok`
value for at least one current Universe member, then use `Strategy.rebalance_every`
from that first ready Session. Later empty Eligible Sets do not skip callbacks.
The parameter cadence controls the built-in S2 strategy, not Python callbacks;
cross-strategy result comparisons require explicitly aligned Decisions.
An empty Eligible Set is a valid cash Decision. A declaration can request
`FactorDependency("momentum", "etf_tri_momentum", {"lookback": 60})`; only this
Rust PIT Factor is supported on this path. Existing Static Factor kinds and
warm-up scheduling are unchanged. The Factor's window is independent of the
visible history window and the parameter controlling Eligible membership.

The immutable View additionally exposes:

- `universe`: the current Session's PIT lifecycle Instrument IDs;
- `eligible`: the current Session's Rust Eligible Set (before Risk Exit exclusion);
- `risk_excluded`: risk notices already known by this close;
- `execution_status`: only known derived rows for current members in the declared
  window, with `can_buy`, `can_sell`, `reasons` and `rule_version` in addition to
  the existing keys/status/source/availability columns.

Bars resolve native codes to their owning lifecycle, never splice reused codes,
and preserve unknown availability. Bars, Factor Values and statuses are compacted
before Arrow export and require `available_at <= decision close`; future and
late rows are absent. No future state is exposed through underlying buffers.

Positive targets and ranked entries must be in `eligible` and outside
`risk_excluded`. Zero targets may name Catalog lifecycles that have left the pool;
omitting them also requests exit. No actual holdings are passed to callbacks.
Rust remains responsible for actual sellability, whole-rebalance deferral and
persistent Risk Exit, including failed exits after Universe removal. Exit intent
does not authorize a fill at an unavailable price.

For the single-Run generator pass `pit=<same frozen input object>` and `members=[]`.
Mixing Static members or an availability override with PIT is rejected. The
same Rust constructors and validation are used by callback Experiment execution.

`python/tests/test_pit.py` and the synthetic-only `pit_fixture` Cargo example cover
3 lifecycles × 10 Sessions, adversarial buys/availability, buffer boundaries,
CLI identities/hashes, Python S2/common Engine values and replay. This remains
6a price engineering with explicit empty-action coverage, not real historical
PIT evidence, a real-market P0 closure or dividend total-return validation.
