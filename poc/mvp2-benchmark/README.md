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

Measured 2026-10-09 10:29–10:38 UTC on **Apple M1, 8 physical/logical cores,
16 GiB RAM, macOS 26.2**, rustc 1.98.1, native `aarch64-apple-darwin` release CLI.
The Python 3.8.0 controller reports `x86_64` (Rosetta); that is the controller's
architecture, not the Rust CLI's. Each Execution records `aarch64` separately.
Machine load outside the runner was not controlled.

Source revision: `153f4b27af1ea1ced1d1f8c5671d0a0ab6c4def9`;
all **48** samples record **`reproducible: true`**, 288 successful Runs and newly
created results. Measurement revision refers to this source commit, not the
later evidence-only commit. See [raw measurements](raw-measurements.json) for
all raw `time -l` output/provenance and [numeric summaries](medians.json) for all
series, medians, ranges and nearest-rank p95.

### Median measurements (3 samples per cell)

Wall includes result persistence; factor/Run columns are the CLI's phase times.
RSS is whole-process peak RSS in MiB (bytes / 1,048,576).

| Threads | Factor cache | Level | Wall s | Factor ms | Run ms | Runs/s | RSS MiB |
| ---: | --- | --- | ---: | ---: | ---: | ---: | ---: |
| 1 | cold | summary | 6.531 | 1685 | 4528 | 44.10 | 319.0 |
| 1 | cold | full | 21.393 | 1683 | 4511 | 13.46 | 337.7 |
| 1 | hot | summary | 4.932 | 114 | 4505 | 58.40 | 284.0 |
| 1 | hot | full | 19.924 | 116 | 4566 | 14.46 | 399.2 |
| 2 | cold | summary | 4.209 | 1115 | 2758 | 68.42 | 340.3 |
| 2 | cold | full | 18.864 | 1153 | 2548 | 15.27 | 358.4 |
| 2 | hot | summary | 2.974 | 73 | 2590 | 96.85 | 287.3 |
| 2 | hot | full | 17.760 | 74 | 2650 | 16.22 | 403.5 |
| 4 | cold | summary | 3.359 | 929 | 2080 | 85.73 | 370.5 |
| 4 | cold | full | 17.969 | 905 | 2066 | 16.03 | 479.3 |
| 4 | hot | summary | 2.424 | 41 | 2036 | 118.81 | 299.9 |
| 4 | hot | full | 17.028 | 40 | 2214 | 16.91 | 415.5 |
| 8 | cold | summary | 2.490 | 733 | 1431 | 115.64 | 425.2 |
| 8 | cold | full | 17.022 | 687 | 1396 | 16.92 | 518.5 |
| 8 | hot | summary | 1.535 | 28 | 1190 | 187.68 | 317.0 |
| 8 | hot | full | 16.106 | 27 | 1145 | 17.88 | 433.2 |

In this measured synthetic matrix:

- Cold samples compute 32 factors and record 84 cache hits; hot samples compute
  none and record 116 hits. Cold hits include dependency reuse within the same
  process, not evidence of a preexisting cache.
- Summary publishes 3 files, approximately 339,424–339,426 bytes; Full publishes
  1,731 files, approximately 234,414,392–234,414,394 bytes. Exact values per
  invocation are in the raw report; metadata timestamps/counter digit lengths
  can change byte counts without changing result identity.
- All 24 Summary samples pass the recorded absence-of-`runs/` assertion.
  All 48 samples have the same Summary logical hash
  `5ac6bee67549190bacbe2f6bfa8b402357030b0c1c5f50d90adafdbd1eace2be`.
- Across individual samples peak RSS ranges from 296,763,392 to 561,496,064
  bytes. The table shows medians, not these extremes.
- More Rayon threads reduce the measured factor/Run phase times; Full's total
  wall time falls less than its Run phase time. Phase timing does not isolate
  serialization, filesystem I/O or provenance capture; no causal attribution or
  real-data scalability claim is made.

The dependency guard excluded DuckDB and the legacy warehouse/research stack.
Offline release build took 9m12s; building the fixture example took 1m59s on the
new worktree's initially empty target. Available space was about 28.6 GiB before
and 27.6 GiB after these builds; target was about 1.1 GiB. No shared target was
cleaned and no cold-build comparison is claimed.

An initial measurement attempt was aborted after one sample because the
manifest's git revision retains a newline. The runner comparison was fixed to
strip it; the complete matrix above was rerun from the new clean source commit,
and the aborted sample is not included.
