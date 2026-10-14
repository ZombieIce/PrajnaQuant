# MVP-5 Python Decision throughput (#162)

Record-only measurements on the registered synthetic 64 instruments × 252
Sessions fixture. **No performance pass threshold. This does not overturn B3's
`reject` and is not a production throughput promise.** No real-market, PIT,
dividend-total-return or Accurate performance claim is made.

## Reproduce

From the repository root, use GIL-enabled CPython 3.12 and the environment/build
instructions in [prajna-py](../../crates/prajna-py/README.md):

```sh
source .venv/bin/activate
export PYO3_PYTHON="$VIRTUAL_ENV/bin/python"
maturin develop --release --locked --offline
cargo build -p prajna-experiment --bin prajna-experiment \
  --example prepare_fixture --example acceptance_fixture --locked --offline
cargo build -p prajna-experiment --example acceptance_fixture --release --locked --offline
python poc/mvp5-benchmark/benchmark.py measure \
  --output /tmp/mvp5-raw.json --repeats 5
python poc/mvp5-benchmark/benchmark.py summarize /tmp/mvp5-raw.json \
  > /tmp/mvp5-summary.json
```

The runner **first** reruns all of `test_acceptance.py` and refuses measurement
on failure or skip. This is #161's S2/S3 Decision and Vector/Fast Event
Summary/Standard equivalence plus adversarial/replay checks; the Full-only
Accurate smoke is not a throughput prerequisite or equivalence evidence.
The debug CLI/fixture binaries are necessary for these tests even though the
measured extension and fixture publisher are release builds.

On the recorded host Homebrew Python 3.12.15's `pyexpat` required Homebrew expat
2.9.0 instead of the system library. `brew install expat`, followed by
`export DYLD_LIBRARY_PATH=/opt/homebrew/opt/expat/lib`, restored pip/stdlib
operation. Set this variable in the actual child environment when wrapping a
command with macOS `/usr/bin/time`; SIP strips inherited `DYLD_*` variables.
No shell startup files or Python sources were patched.

## Workload and boundaries

- Public `Experiment.run()`, Vector Engine, Summary retention, same costs,
  Static Universe and source fixture as #161. Every sample requires newly
  created, successful Runs, never replay-only work.
- S2 uses Rust RotationScore (5/40 momentum, volatility 10, all weights 1,
  no trend), rebalance every 5 Sessions, TopK=1 and 3: two canonical Runs,
  43 callbacks each. Python imports the exact acceptance-tested pure function.
- S3 is MA20/60, one canonical Run, 192 callbacks, lookback 61. The fixed
  parameter combination deduplicates to **one Run**, so selecting two Rayon
  threads does not expose Run-level parallelism. These S3 columns record thread
  settings, not a multi-Run scaling result. Do not manufacture distinct Runs
  with ignored replica parameters. General fixed-schedule/change-only S3
  equivalence remains limited as documented by #161.
- Five repeats per cell, one/two Rayon threads, one fresh Python process per
  repeat/cell, with one untimed matching warmup. Cell ordering reverses every
  other repeat. Each warmup and timed call gets a fresh copy of the prepared
  lake outside the timer; persistent Factor Cache is initially absent, OS/page
  caches are uncontrolled/warmed. Python generation currently recomputes Rust
  Factors and rereads the panel per canonical Run, not via Factor Cache.
- End-to-end Runs/s = canonical Run count / public API wall seconds. Includes
  Python source/environment capture (`pip freeze`), parsing, data preparation,
  both stages, result storage and return conversion. Excludes process startup,
  imports, fixture publication, copying the lake, correctness checks and builds.
- `stage1_ms` includes serial GIL-bound Decision preparation, Rust Factors,
  Views and callbacks. `stage2_ms` includes detached provenance, Rayon execution
  and storage; the existing `run_time_ms` is the narrower Run interval. These
  are wall intervals, not summed worker CPU time. They need not add to the API
  wall time because wrapper/provenance work is outside them.
- Every View construction is timed, including the safety-required independent
  compact buffers and Arrow/PyArrow export. Per-call median/p95 pools all
  samples in a cell, not averages of per-Run medians. This is not a no-copy
  slice benchmark. Clock/sample collection overhead remains included.
- RSS is `getrusage(RUSAGE_SELF).ru_maxrss` normalized to bytes (macOS bytes,
  Linux KiB). Each candidate has its own process, but the high-water includes
  interpreter/imports, lake copy and untimed warmup, not just the timed call.
  It excludes transient `pip freeze` subprocess RSS and is not a whole-process
  tree or production capacity bound.

## Recorded result (2026-10-14)

Apple M1 / MacBookPro17,1, macOS 26.2 ARM64, 8 logical CPUs, 16 GiB RAM,
Rust 1.98.1, CPython 3.12.15, PyO3 0.29.0, PyArrow 23.0.1, maturin 1.15.0.
The gated release run passed all **6 acceptance tests with no skips** before
collecting 40 samples. Host load was uncontrolled (recorded one-minute load
averages 3.42–8.37); the host was shared, not benchmark-dedicated.

End-to-end median Runs/s, with historical B3 adjacent for context:

| Boundary | Strategy | Runs/call or group | 1 thread | 2 threads/workers | Peak RSS MiB |
| --- | --- | ---: | ---: | ---: | ---: |
| MVP-5 Rust built-in, Vector/Summary API | S2 | 2 | 2.851 | 3.022 | 112.08 |
| MVP-5 Python Decision, Vector/Summary API | S2 | 2 | 1.021 | 1.017 | 124.59 |
| MVP-5 Rust built-in, Vector/Summary API | S3 MA20/60 | 1 | 1.470 | 1.470 | 74.09 |
| MVP-5 Python Decision, Vector/Summary API | S3 MA20/60 | 1 | 0.218 | 0.218 | 103.27 |
| B3 Rust Native + account replay (historical) | S2 | 12 | — | 126.04 | 86.86 shared |
| B3 Python per-bar + account replay (historical) | S2 | 12 | — | 6.67 | 86.86 shared |
| B3 Python batch + account replay (historical) | S2 | 12 | — | 14.48 | 86.86 shared |
| B3 Rust Native + account replay (historical) | S3 MA20/60 | 12 | — | 472.64 | 86.86 shared |
| B3 Python per-bar + account replay (historical) | S3 MA20/60 | 12 | — | 11.58 | 86.86 shared |
| B3 Python batch + account replay (historical) | S3 MA20/60 | 12 | — | 144.38 | 86.86 shared |

MVP-5 RSS is the maximum of process high-waters across both thread settings and
all five repeats, **not** the median RSS in the machine-readable summary.
B3 RSS is one shared high-water for all candidates, not candidate attribution.
Historical one-thread throughput is not transcribed here; `—` is not zero.

Python segmentation (median milliseconds per public API call):

| Strategy | Threads | API wall | Stage 1 | Detached stage 2 | Rayon Runs only | View median / p95 per callback |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| S2 | 1 | 1958.734 | 775.089 | 510.516 | 12 | 1.773 / 2.588 |
| S2 | 2 | 1965.870 | 962.810 | 483.945 | 6 | 1.766 / 3.307 |
| S3 | 1 | 4588.532 | 3572.769 | 492.942 | 23 | 1.435 / 1.811 |
| S3 | 2 | 4588.281 | 3538.674 | 496.902 | 24 | 1.434 / 1.759 |

Each S2 View statistic pools 430 samples; each S3 statistic pools 960.
Medians of separate intervals are not additive. Two Rayon threads reduced
S2's narrower Run interval, but did not improve Python end-to-end throughput
in this small batch. S3 is dominated by serial Decision preparation and has
only one Run. Neither is a general scaling law or a new eligibility gate.

Artifacts:

- [Raw measurements, environment and command log](raw-measurements.json)
- [Validated medians and raw series](summary.json)
- [Release build commands/resources, including the resolved SIP/expat failure](release-build.log)
- [Full Python binding tests: 41 tests, one Full-only Nautilus skip](python-tests.log)
- [Targeted Rust tests/Clippy/formatter](rust-verification.log)
- [Repository Python regression and its limits](repository-python-tests.log)
- [Restored warehouse and report CLI tests](targeted-python-tests.log)

DSV: `dsv:sha256:80b804f22c01b7d0375ffd610e90dc73d401b82700d8edf57c3995bbd363186c`.
Source fixture SHA-256:
`c805d3ef0873171d89b4380ef593c5a032d9c302783ce620b11eb09a85d5c36e`.
Measured base revision: `bb2117567862d9a8e3231cedaca003a546dab76f`;
source/binary hashes and the dirty diff snapshot are in the raw archive.

## Evidence and interpretation

The runner archives raw per-invocation/per-View timings, fixture content and
identity, complete Experiment definitions, Execution metadata, hashes of the
fixture binary/extension/changed source/Cargo.lock, compiler/Python/package
versions, host details, correctness output and every subprocess command/output.
Summary validation rejects incomplete matrices, failed/replayed Runs, changed
logical hashes across thread counts/repeats and missing/invalid View samples.
Performance values themselves never produce `adopt`/`reject`.

The benchmark's source hashes and base revision identify the measured source
even when evidence files make the tree dirty. Execution `reproducible=false`
on that dirty tree is preserved, not relabeled as a clean-build result. Paths,
installed environment and source hashes remain part of Python provenance;
`pip freeze` is a snapshot, not an artifact/resolver lock.

Historical [B3](../poc0-benchmark/README.md#b3-real-strategies-and-parallel-boundary-ticket-11)
used Rust-embedded CPython 3.12.2, two workers × six independent Runs, a different
generated 64×252 dataset and S2 20/60/20 Top-5, and included Rust account replay.
MVP-5 uses Python-hosted extension calls with Vector/Summary persistence and
pip provenance, not that replay path. Both remain synthetic; adjacent tables
are context, **not a controlled speedup comparison**.

For interactive research with a few parameter combinations the pure Decision
API is the measured entry point, with explicit View/provenance costs. For
large parameter grids prefer Rust built-ins when available: Rayon accelerates
only stage two, not Python's serial first stage. No measured grid-size cutoff
or extrapolation to larger universes/history is justified here; remeasure the
actual strategy, declarations, retention level and batch size. Fast/Accurate,
cache-hot grids and larger Python grids are unmeasured.

## Verification limits

Targeted Rust `prajna-py` test/Clippy and workspace formatting pass; vendor
`polars-io` warnings and the dev linker's existing unwind-size warning remain.
The new public API regression verifies one timing per View and unchanged
manifest/results on replay. Report CLI tests run in lightweight CI without
requiring the extension or Nautilus.

The repository Python regression initially reported two MVP-1 scale
byte-golden float-tail failures and a missing `duckdb` import. Installing the
missing dependency restored all five warehouse tests; warehouse plus the three
new report tests pass. The two unchanged golden failures already reproduce on
an untouched base under Python 3.12; see the
[previous baseline evidence](../mvp3-benchmark/python-baseline-failures.log).
They are not fixed or accepted by this ticket. Full bundled-DuckDB workspace,
Nautilus integration and frontend builds are not rerun for this profiling-only
binding change. These limits must not be presented as an all-suite pass.
