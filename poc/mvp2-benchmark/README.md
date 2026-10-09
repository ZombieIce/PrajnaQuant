# MVP-2 synthetic throughput/resource benchmark

Scope: E16 / Issue #104, **64 instruments x 252 sessions, 288 Vector Runs**.
There is no performance pass threshold. These measurements do not establish
real-market throughput, PIT correctness, out-of-core behavior, or cash/holdings
ledger conservation.

## Reproduce from the repository root

Requires macOS (`/usr/bin/time -l`), Rust, Git and Python 3.8+. Check available
space before compiling; preserve at least 10 GiB. No DuckDB or legacy execution
stack is in the `prajna-experiment` dependency graph.

```sh
cargo build --release -p prajna-experiment --locked
cargo build --release -p prajna-experiment --example prepare_fixture --locked
# Add --offline to both commands when the locked dependencies are already cached.
# Commit source changes first. Use a NEW output directory OUTSIDE the worktree.
python3 poc/mvp2-benchmark/benchmark.py run --output /tmp/prajna-mvp2-measurements
python3 poc/mvp2-benchmark/benchmark.py summarize \
  /tmp/prajna-mvp2-measurements/raw-measurements.json
python3 -m unittest tests.test_mvp2_benchmark -v
```

The runner does not build or install anything; `CARGO_TARGET_DIR` is honored.
Build the binaries from the same clean revision before running. It refuses a
dirty worktree, an existing output directory, fewer than three repeats, or
in-worktree output. Raw data are saved progressively; errors exit nonzero, leave
an incomplete record, and never produce a successful median report.

## Workload and measurement protocol

- The checked-in `experiment.json` identifies the normalized
  `b2-s2-scale-64x252-v2.json` fixture and its all-instrument Static Universe.
  `prepare_fixture` republishes both in a temporary lake; the runner checks the
  resulting definition against the checked-in definition.
- Grid: short 5/10/20; long 40/60; vol 10/20; trend null/20; top_k 1/3/5;
  rebalance_every 1/5/10/20. The three weights are 1.0, from the fixture.
  Commission 0.001, buy/sell slippage 10 bps, buy/sell tax 0.0 match the fixture.
  `sessions_per_year=252`, availability `none`, Vector `vector@1`, no engine seed.
  The fixture's generator seed is recorded in its input, not used as an engine
  seed. Event-only initial cash, lot size and minimum commission do not apply
  to the Vector weight-return model (ADR 0017).
- Matrix: 1/2/4/8 Rayon threads x cold/hot Factor Cache x summary/full, three
  serial repeats per cell (48 measured CLI processes). One untimed summary
  warmup at one thread prepares factors for the hot-cache samples.
- Every sample uses a fresh lake containing only fixture data and Universe,
  plus a copy of factors for hot samples. No prior Experiment/Execution is
  copied. Thus all samples publish new results, never replay or promote.
  Cold means **Factor Cache empty**, not OS page cache cold; page cache is not
  flushed. Lake copying and warmup are excluded from timings.
- `/usr/bin/time -l` wraps each CLI process. Raw stdout, stderr, exact command,
  exit code, time output and timestamps are retained in `raw-measurements.json`.
  macOS reports maximum RSS in **bytes** for the whole CLI process. End-to-end
  wall time uses Python's monotonic counter around that process; runs/s is
  288 divided by this wall time. CLI total/factor/Run times are separately
  retained in milliseconds (integer precision).
- `written_bytes`/`written_files` are CLI **published Experiment/result** counts:
  they exclude Factor Cache, lock files and staging writes. Each sample also
  records complete lake file/byte totals before/after, including factors.
- The script checks every summary Execution for absence of `runs/` and records
  `summary_no_runs=true`. It rejects failed Runs, replays, incorrect cache
  conditions, missing RSS, incomplete/duplicate repeats, and differing Summary
  logical hashes across the entire matrix. These are measurement-validity
  checks, not performance thresholds.
- `medians.json` retains all raw numeric series per cell, medians, min/max and
  nearest-rank p95 (with three repeats p95 is the maximum, not a stable tail
  estimate). `raw-measurements.json` includes POC provenance: machine/CPU,
  memory, OS/architecture, compiler/target, git revision/diff hash/clean status,
  lockfile/fixture/binary hashes, build configuration and threading environment.
  Other machine load is not controlled; no concurrent build is started by the
  runner.

## Recorded results

The raw measurements and numeric summaries are recorded alongside this README
after executing the matrix from a clean committed source revision. Measurement
revision refers to that source commit, not the later evidence-only commit.
