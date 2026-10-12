# MVP-3 Fast Event Summary throughput / RSS

Issue #123 / #112 F12: synthetic **64 Instruments × 252 Sessions**, 288 TopKRank
Runs, `fast_event@1`, `lot + full_target`, default skip. No performance threshold.
This protocol measures the Experiment CLI with fixed-point account/VP invariants,
not real-market throughput, PIT, total return or production engine selection.

## Reproduce

Preserve at least 10 GiB available disk before building. From a clean committed
checkout, build the release binary and fixture publisher from that same revision:

```sh
cargo build --release -p prajna-experiment --bin prajna-experiment --example prepare_fixture --locked --offline
python3 poc/mvp2-benchmark/benchmark.py run --engine fast_event --output /tmp/prajna-mvp3-measurements
python3 poc/mvp2-benchmark/benchmark.py summarize /tmp/prajna-mvp3-measurements/raw-measurements.json
python3 -m unittest tests.test_mvp2_benchmark -v
```

Requires macOS `/usr/bin/time -l` and Python 3.8+. Output must be a new directory
outside the checkout. `CARGO_TARGET_DIR` is honored. The shared MVP-2 runner keeps
Vector's original 16-cell protocol; `--engine fast_event` selects **8 Summary
cells**, 1/2/4/8 Rayon threads × cold/hot Factor Cache, three serial repeats.
The report rejects failed Runs, false/missing invariant markers, differing Summary
logical hashes, prior Executions, incomplete repeats, dirty/changed provenance,
missing RSS, and Summary `runs/` directories. These are validity checks, not speed
or memory pass thresholds. Failure keeps an incomplete raw record, no medians.

The registered [definition](experiment.json) uses the same 288-Run parameter grid,
DSV/Static Universe, costs and availability as MVP-2, adding the fixture's account
capital **100000 CNY**, Instrument **lot_size=100**, minimum commission **100 CNY**,
one VP ID 1 with all capital, and explicit full-target lot semantics. Sizing,
capital and minimum commission affect these results; no comparison to the old
Vector performance scores is asserted. The fixture seed remains data provenance;
the deterministic Engine has no seed.

Every process starts with a fresh dataset/Universe-only lake; hot samples copy
factors from an untimed one-thread warmup. OS page cache is not flushed. Copying,
warmup and compilation are outside measured time. Runs/s = 288 / monotonic CLI
wall seconds, including process startup, loading, strategy/ledger computation and
Summary persistence. CLI factor/Run/total milliseconds are also saved. Peak RSS is
whole-process macOS maximum resident set size **in bytes**, not incremental engine
memory. Other machine load is not controlled; no concurrent build is started by
the runner. Summary releases Run details after E11 reduction but the engine still
constructs and verifies both valuation points within each active Run.

Raw provenance follows `poc/mvp2-benchmark`: exact command/stdout/stderr/time output,
exit code/timestamps, compiler/target, CPU/cores/memory/OS, source commit and clean
status, Execution diff identity/reproducibility, lockfile/fixture/binary hashes,
thread environment, cache and published bytes/files. The medians preserve every
numeric series, median/min/max/nearest-rank p95; three-sample p95 is the maximum.
Build resources are separately recorded and are not mixed with runtime throughput.

## Recorded results

Measured on 2026-10-12 (UTC timestamps in raw data), Apple M1, 8 logical/physical
cores, 16 GiB RAM, macOS 26.2; rustc 1.98.1, native aarch64-apple-darwin release,
Python 3.12.2 arm64 controller. Source revision:
`c85d4abd2394b1f7c092509a089eff0dd5bbad7d` (the later evidence commit does not
change the measured runtime). All 24 CLI samples have clean reproducible
provenance, 288 completed Runs, all account/VP invariant flags true, and no `runs/`.
All eight cells share Summary logical hash
`b0f6e0f8c0893299ed604e60a552e8519734279620c2694b6332cb5c243b87b0`.

| Rayon threads | Cold median Run/s | Hot median Run/s | Cold median RSS MiB | Hot median RSS MiB |
| --- | --- | --- | --- | --- |
| 1 | 27.51 | 31.56 | 125.75 | 85.52 |
| 2 | 49.49 | 57.63 | 147.61 | 102.41 |
| 4 | 78.09 | 104.08 | 193.45 | 126.83 |
| 8 | 103.26 | 133.38 | 250.17 | 166.61 |

[Raw measurements](raw-measurements.json) retain all samples and
[medians](medians.json) retain the numeric series, ranges and p95. Recomputing
medians with the public summarize command exactly matches the committed report.
There is no adoption/rejection verdict or performance threshold.

[Build resources](build-resources.json) contain the exact formal release command
(including `--bin`, `--example` and `--offline`) and clean source identity; the raw
runner's inherited `build_command` is the generic reference CLI command. The
formal warm rebuild took 16.049 s. The initial dependency bootstrap is explicitly
non-reproducible because source work continued during that build; it is not the
measured binary. No cold-build cost is claimed.

[Verification](verification.json), [resource attempts](verification-resources.json)
and [dependency graph](dependency-tree.txt) record the workspace/CI boundaries.
Workspace tests: 307 passed, one pre-existing audited-local-snapshot test ignored.
Workspace Clippy first stopped at the disk guard, then passed after scoped removal
of this worktree's completed test executables and incremental cache; each command,
space sample and removed object is retained. No user cache/other worktree was removed.
The release/Experiment dependency graph excludes DuckDB and legacy execution stacks.

Python 3.12 with isolated temporary DuckDB: 74/76 tests pass; two pre-existing
MVP-1 scale Vector byte-golden float-tail failures reproduce on untouched base
`fc43482` in the same environment. See [current output](python-results.log) and
[baseline reproduction](python-baseline-failures.log). These goldens were not
rewritten. The targeted benchmark + independent Fast Event suite passes 39/39
([output](targeted-python.log)). Frontend offline install/build passes. The ignored
real-snapshot test and baseline Python failures are limitations, not synthetic
accounting/performance acceptance passes.
