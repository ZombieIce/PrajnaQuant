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
