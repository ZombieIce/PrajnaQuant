# POC-0 benchmark entry

## Parquet B1 path (ticket 05)

From the repository root, use the shared lightweight build and generate a versioned Parquet
fixture with a manifest. `--reuse` reads the existing file and verifies its size, SHA-256 and
fixture content identity before timing. The file has 7 columns; each candidate receives only
`date`, `symbol`, and `close` from the same selected row groups:

```bash
cargo run -p quant-research --no-default-features --locked --offline -- \
  benchmark-poc0-parquet --parquet target/poc-0/dataset-v1.parquet \
  --output target/poc-0/parquet-report.json
target/debug/quant-research benchmark-poc0-parquet --reuse \
  --parquet target/poc-0/dataset-v1.parquet --symbol A \
  --start 2026-01-06 --end 2026-01-07
```

The CLI preserves all earlier dates needed by rolling factors and the next date needed by
return evaluation, then filters the reported projection to the requested interval. It checks the
fixed independent golden first, and the Parquet bars against the source fixture. The report
separates generation, scan/decode, scan conversion, three candidate compute paths, layout
conversion, serialization, and total time. Cache state is explicitly uncontrolled; these
numbers are single observations, not a layout selection. The B1 return is an evaluation label,
not an event-account NAV.
The reader uses Polars' row-group strategy to skip non-overlapping groups before column decoding;
the single-threaded strategy still entered the column path for skipped groups in the pinned Polars
version. A repeat 10M-row filtered scan and peak RSS record are in
[`results/parquet-large-10m-rowgroups-2026-09-27.json`](results/parquet-large-10m-rowgroups-2026-09-27.json)
and its `.rss.json` companion. Cache state was not controlled between runs.

`--filler-rows N` writes deterministic extra rows in 4,096-row batches for a larger source. It
requires an explicit fixture `--symbol` filter when reading; filler rows are excluded from S2.
Before writing, the CLI checks free space against a conservative `160 × N + 4 MiB` output
estimate and a 10 GiB reserve. A failed gate writes an `unresolved` report and exits 2.
Measure runtime RSS separately after building:

```bash
python3 poc/poc0-benchmark/measure-parquet-rss.py \
  --parquet target/poc-0/large-10m.parquet \
  --output target/poc-0/large-rss-report.json \
  --symbol A --start 2026-01-06 --end 2026-01-07
```

Add `--limit-mib 256` only on a host where `RLIMIT_AS` is supported. On the current macOS host,
`setrlimit` failed before launching the child. The constrained-memory acceptance instead passed
in a local Colima Linux ARM64 VM using Docker's cgroup v2 limit, without `--limit-mib`. The
unrestricted 10-million-row run, macOS failure, and Linux cgroup evidence are recorded in
[`ticket 05`](../../.scratch/poc-0-benchmark/issues/05-parquet-and-out-of-core.md).
Build the Linux binary without the cap, regenerate the large file, then limit only the read:

```bash
docker run --rm -v "$PWD:/work" -v poc05-build:/build -e CARGO_TARGET_DIR=/build \
  -e CARGO_BUILD_JOBS=2 -e CARGO_INCREMENTAL=0 -w /work rust:bookworm \
  cargo build -p quant-research --no-default-features --locked
docker run --rm -v "$PWD:/work" -v poc05-build:/build -w /work rust:bookworm \
  /build/debug/quant-research benchmark-poc0-parquet \
  --parquet target/poc-0/large-10m.parquet --output target/poc-0/large-generated.json \
  --filler-rows 10000000 --symbol A --start 2026-01-06 --end 2026-01-07
docker run --name poc05-evidence --memory=256m --memory-swap=256m --network=none \
  -v "$PWD:/work" -v poc05-build:/build -w /work python:3.12-bookworm sh -c '
    cat /sys/fs/cgroup/memory.max
    python3 poc/poc0-benchmark/measure-parquet-rss.py \
      --binary /build/debug/quant-research --parquet target/poc-0/large-10m.parquet \
      --output poc/poc0-benchmark/results/parquet-large-10m-limited-cgroup-2026-09-27.json \
      --symbol A --start 2026-01-06 --end 2026-01-07
    result=$?
    cat /sys/fs/cgroup/memory.peak /sys/fs/cgroup/memory.events
    exit "$result"
  '
docker inspect poc05-evidence --format \
  'exit={{.State.ExitCode}} oomKilled={{.State.OOMKilled}} memory={{.HostConfig.Memory}} swap={{.HostConfig.MemorySwap}}'
```

Use a different container name for a second run. The pinned reports are
[`constrained projection`](results/parquet-large-10m-limited-cgroup-2026-09-27.json),
[`child RSS`](results/parquet-large-10m-limited-cgroup-2026-09-27.rss.json), and
[`Docker/cgroup evidence`](results/parquet-large-10m-limited-cgroup-2026-09-27.cgroup.json).
`memory.peak` includes the reader's file cache and the Python parent; it reached the 256 MiB
limit with `memory.events.max=646`, but `oom_kill=0` and the command exited 0. Do not compare
these single-run timings across uncontrolled cache conditions. Check the 10 GiB disk reserve
before building or regenerating a large file.

Run the fixed three-ETF, ten-session fixture from the repository root:

```bash
cargo run -p quant-research --release --locked --offline -- benchmark-poc0
```

The command writes `target/poc-0/benchmark-report.json`. A failed golden comparison still writes a report, skips timing, and exits with code 2. To run another versioned dataset and expected projection:

```bash
cargo run -p quant-research --release --locked --offline -- benchmark-poc0 \
  --dataset path/to/dataset.json \
  --expected path/to/expected.json \
  --output target/poc-0/custom-report.json
```

## Shared lightweight POC build path

The normal application keeps the default `app` feature, including its warehouse and bundled
DuckDB dependencies. To compile only the POC harness, disable default features. This uses the
workspace `target/` cache, so B1, B2, and B3 do not create per-candidate build directories:

```bash
CARGO_TARGET_DIR=target cargo run -p quant-research --no-default-features --release \
  --locked --offline -- benchmark-poc0 --candidate polars
```

The no-default-features dependency graph is the build boundary check:

```bash
cargo tree -p quant-research --no-default-features --locked --offline -e normal
```

It must not contain `ashare-warehouse`, `duckdb`, or `libduckdb-sys`. Before measuring a build,
capture a filesystem-space and target-size baseline; proceed only if the volume has at least 10 GiB
free and the conservative completion estimate leaves at least 10 GiB. The recorder applies that
gate and stores raw command output, wall time, target delta, free space, revision, lockfile hash,
and dependency-graph hash:

```bash
python3 poc/poc0-benchmark/capture-build-resource.py --profile dev \
  --output poc/poc0-benchmark/results/build-resource-dev.json
python3 poc/poc0-benchmark/capture-build-resource.py --profile release \
  --output poc/poc0-benchmark/results/build-resource-release.json
```

The recorder defaults to a 2 GiB maximum additional-size estimate and refuses either build if
the 10 GiB reserve would be crossed. These are warm-cache measurements; they do not represent a
cold build. Do not run `cargo clean` to manufacture one. The 2026-09-27 initial warm-cache dev and
release records are [`dev`](results/build-resource-dev-2026-09-27.json) and
[`release`](results/build-resource-release-2026-09-27.json). Both succeeded using the shared
workspace target; their dependency graph hash is identical and excludes DuckDB. Final monitored
reruns with source identity and runtime space samples are [`dev`](results/build-resource-dev-final-2026-09-27.json)
and [`release`](results/build-resource-release-final-2026-09-27.json). The budget refusal example
is [`blocked`](results/build-resource-blocked-2026-09-27.json). The source manifest is
[`here`](results/build-source-manifest-2026-09-27.json); dependency graph snapshots and host
inventory are also preserved under `results/`.

| Profile | Wall time | `target/` delta | Free space after |
| --- | ---: | ---: | ---: |
| Dev | 39.6 s | +487 MiB | 12.3 GiB |
| Release | 264.2 s | +440 MiB | 11.9 GiB |

These are single warm-cache observations, not stable duration estimates. There is no cold-build
sample. The isolated historical `poc/b1-layout` package uses its own lockfile and target by
default; new B1/B2/B3 work should use the shared no-default-features command above. Its Arrow
60.0.0 pin differs from the main harness's Arrow 58.4.0 and therefore is not part of this shared
cache claim. A later warm-cache rerun after source identity capture took 21.2 s (dev) and 3.0 s
(release); these are verification reruns, not substitutes for the initial measurements. The
recorder polls free space every two seconds and stops at the 10 GiB floor plus a safety margin.
The final reruns completed without approaching that runtime stop threshold.

The initial inventory's `du -sh target` was about 59G of allocated filesystem blocks. The raw
JSON's `workspace_target_bytes` is a recursive sum of file `st_size` values (logical file bytes),
so its larger value is a different measurement and should not be compared directly. The inventory
also records the exact default-app, no-default POC, and standalone B1 dependency graphs, lockfile
hashes, target locations, and `lsof` observations.

The input fixture records its seed identity, ten-session calendar, three instruments, generated OHLCV rules, one missing bar, per-session execution status and availability time, strategy parameters, costs, and time model. Its expanded content hash is pinned by the independent expected file. The expected projection separately lists each score/rank and target, the UNKNOWN and HALTED order rejections, fills, cash/holdings/NAV by date, and aggregate costs. Float comparisons use a fixed absolute tolerance of `1e-8`; discrete fields compare exactly.

The default `--candidate reference` runs the existing Rust ETF backtest as the measured candidate. Select `--candidate soa` to measure the Custom SoA Momentum Rotation instead. Both candidates are checked against the independent event-ledger projection and account identity first; only the selected candidate is timed. The selected path records one warmup and five raw timing samples. Reference timings cover the backtest call only; fixture parsing and report projection are outside that timer. Peak RSS is currently reported as unknown. Machine, CPU, memory, Rust version, lockfile hash, Git revision, tracked diff hash/summary, changed paths, and dirty-worktree completeness are recorded when available. An untracked worktree is explicitly marked incomplete because untracked file contents are not included in the Git diff hash.

This fixture is an event ledger. The current SoA candidate uses a separately declared weight-times-return model and is compared with the event path only for shared signal/rank behavior; its simplified returns are not asserted to reproduce event-account NAV. This single small correctness fixture establishes the CLI contract; its timings do not support an architecture or performance decision. The seed is a stable fixture identity; its hand-authored prices are not random samples.

## B1 SoA Momentum Rotation output

Select the reference or SoA candidate with `--candidate reference` or `--candidate soa`; the default remains `reference`. For example:

```bash
cargo run -p quant-research --release --locked --offline -- benchmark-poc0 --candidate soa
```

The `b1_soa` section reports the symbol-major SoA path's
momentum/volatility ranks, TopK targets, equal target weights, and simplified next-session
close-to-close portfolio return labels. Missing bars break return availability and instruments
without a bar on the signal date are excluded. For the selected SoA candidate, the section contains one warmup, five raw compute samples, and separate per-run factor, rank/TopK/weight, and return-projection timings. These are recorded only after the independent event-ledger golden and rank/target comparison pass.
Its checksum covers the complete vector projection. This weight-return projection has no cash,
fees, orders, fills, or event-account NAV and must not be compared directly with the ledger NAV.
Performance remains unresolved: the fixture is intentionally small and this is one machine/load.

## B1 Polars Momentum Rotation

Select `--candidate polars` to run the same fixed S2 workload through Polars lazy expressions.
The report stores the full projection, checksum, one warmup, five raw elapsed samples, phase
samples for Polars factor expressions, expression/sort/TopK/weight work and return projection,
plus the candidate's DataFrame construction time. Factor availability uses the same
observed-close window boundaries as SoA, including skipped missing bars; score ties sort by
symbol. Correctness is checked against the SoA projection and independent fixture golden before
timing. Returns are next-calendar-session close-to-close evaluation labels, not executed NAV.

```bash
cargo run -p quant-research --release --locked --offline -- benchmark-poc0 --candidate polars
```

The 3 × 10 fixture validates matching semantics only; it cannot support a layout decision or a
general performance claim. The release-mode raw report projection is archived at
[`results/b1-polars-2026-09-27.json`](results/b1-polars-2026-09-27.json), including provenance,
the fixed input identity, correctness status, all five raw samples, phase samples, conversion
samples, and checksum.

## B1 parameter sweep and factor cache (ticket 06)

Run the registered target workload after the shared warm-cache dev and release builds. Each build
record is kept separate from runtime throughput; the release command below uses the same locked
workspace target and dependency graph:

```bash
df -h .
python3 poc/poc0-benchmark/capture-build-resource.py --profile dev \
  --output poc/poc0-benchmark/results/b1-sweep-build-dev-2026-09-27.json
python3 poc/poc0-benchmark/capture-build-resource.py --profile release \
  --output poc/poc0-benchmark/results/b1-sweep-build-release-2026-09-27.json
python3 poc/poc0-benchmark/measure-b1-sweep-rss.py \
  --binary target/release/quant-research \
  --output poc/poc0-benchmark/results/b1-sweep-64x252-2026-09-27.json \
  --dev-build-record poc/poc0-benchmark/results/b1-sweep-build-dev-2026-09-27.json \
  --release-build-record poc/poc0-benchmark/results/b1-sweep-build-release-2026-09-27.json
```

The registered workload is 64 instruments × 252 synthetic weekday sessions, with deterministic
close generation and deterministic missing bars; it has no exchange holidays or real-market/PIT
claims. The fixed `expected-v1.json` 3 ETF × 10 session event ledger is checked first. The sweep
then runs the same S2 factor definition through SoA, Arrow and Polars. Windows are derived from
session count and clamped to short 2–20, long 4–60; score weights are 1.0 short, 1.0 long and 0.5
volatility. The six parameter Runs are unique `top_n` values from 1/5/10 (capped at the universe
size) crossed with `rebalance_every` 1/5.

The factor cache key is the synthetic dataset content hash, layout, factor implementation version,
momentum and volatility windows, score-weight bit patterns, and trend-filter flag. It stores per-day
ranked factor candidates; `top_n` and `rebalance_every` are intentionally excluded so those six
Runs reuse the same factor output. A miss removes the entry, computes and inserts the factors; a
hit reuses the factors and rebuilds targets/returns. Exact checksums are required between each
Run's miss and hit result. Cross-layout projections use the fixed `1e-8` numeric tolerance.

One untimed miss/hit warmup run per layout. Six timed repetitions rotate the first layout and
alternate cache-condition order. Each report keeps raw per-Run samples keyed by parameters,
single-Run median/p95, sequential scan Runs/s, two-thread parallel Runs/s and repeated scan samples.
Checksums are verified outside the sequential and parallel scan timers.
The RSS wrapper measures the whole child process, not per-layout attribution; the exact scope and
raw `getrusage` record are preserved. Conversion/preparation is included in the end-to-end runtime
but is not independently isolated here. Custom SoA's extra maintenance burden is qualitative and
not monetized.

Before measurement, the adoption hurdle is registered at a 20% parallel throughput advantage
over the best alternative in both cache conditions. The wrapper reports `adopt` only if correctness,
target-size and RSS gates pass and the hurdle is cleared; `reject` requires a 20% disadvantage in
both conditions; mixed evidence is `defer`; invalid or undersized evidence is `unresolved`. This
conclusion is scoped to the recorded synthetic workload. Cold Cargo build is not measured and no
target cache is cleared. Shared B1 harness dev/release build seconds and `target/` byte deltas stay
in the separate `build_cost_evidence` section. Per-layout build-cost attribution is Unknown because
all three implementations compile into the same crate and executable.

### Recorded result (2026-09-27)

The reproducible report, separate process-RSS record, and warm dev/release build records are
[`sweep JSON`](results/b1-sweep-64x252-2026-09-27.json),
[`RSS JSON`](results/b1-sweep-64x252-2026-09-27.rss.json),
[`dev build JSON`](results/b1-sweep-build-dev-2026-09-27.json), and
[`release build JSON`](results/b1-sweep-build-release-2026-09-27.json). Dataset content SHA-256 is
`f5e178f3bcbf606dcc84145fd1728c2f504ba942e196eeafa08d818b48c7f132`. The run passed the
independent fixture, cross-layout `1e-8` projection comparison, and exact per-layout cache-hit/miss
checksums. It records 36 individual Run samples for each layout/cache state (six per each of six
parameter combinations) and six complete sequential/parallel sweep repetitions.

| Layout | Cache | Pooled Run median / p95 | Parallel Runs/s |
| --- | --- | ---: | ---: |
| SoA | miss | 9.248 / 9.670 ms | 149.63 |
| Arrow | miss | 10.110 / 10.644 ms | 137.29 |
| Polars | miss | 280.640 / 294.763 ms | 6.07 |
| SoA | hit | 0.339 / 0.694 ms | 4264.08 |
| Arrow | hit | 0.338 / 0.698 ms | 4250.42 |
| Polars | hit | 0.341 / 0.778 ms | 4198.37 |

Peak RSS was 107,905,024 bytes for the whole release benchmark process. Hardware was Apple M1,
macOS, 8 logical CPUs and 16 GiB RAM; Rust was 1.98.1 and the locked dependency hash and dirty
source paths are in the report. Its Git diff identity is incomplete because the working tree has
untracked files; use the same source to reproduce the results. The 20% threshold was not reached:
SoA was 8.99% ahead of the best alternative for misses and 0.32% ahead for hits.
**Conclusion: `defer` Custom SoA adoption**
for this workload; the extra implementation/maintenance burden is not justified by the measured
gain. This is not a real-market or PIT result.

Warm shared-harness dev build: 5.582 s and -142,598,341 shared-target bytes (cache change, not
negative build cost). Warm release build: 9.675 s and +53,203 target bytes. Both kept more than
10 GiB free. Cold build and per-layout build costs
remain Unknown; build timings are not included in Runs/s.

Small CLI overrides are intended for tests only:

```bash
target/debug/quant-research benchmark-poc0-sweep --instruments 3 --sessions 10 \
  --output target/poc-0/b1-sweep-smoke.json
```

## B2 Fast Event Buy & Hold prototype

The report also emits `b2_fast_event_buy_hold`, an independent, deliberately small L1 event path. It creates an equal-weight Buy & Hold target from the fixture's Jan 12 close, submits market buys at subsequent opens, rejects the halted B order on Jan 13, and retries it at the next available open. It marks an existing B holding at its last observed close when the Jan 16 B bar is missing. Orders, fills, fixed costs, cash, holdings, daily NAV, and a stable projection checksum are included. The harness checks repeat-run equality, next-session ordering, the NAV identity, the halt rejection, stale marking, and non-zero costs before collecting one warmup and five timing samples. It separately records day-index initialization, event processing, and end-to-end samples. The CLI test also removes A's later bars and enables buy tax to check the final unexecuted target and non-negative cash.

This is a prototype and a correctness smoke on one tiny fixture. It does not establish production Fast Event semantics, broad benchmark performance, or an architecture decision; Nautilus comparison remains a separate ticket.

## B3 PyO3 per-bar callback comparison (ticket 10)

The optional `b3-pyo3` feature embeds CPython with PyO3 and feeds the same sorted fixture
bar stream to a Rust Native callback and Python `on_bar()` implementations. The no-op case
measures fixed boundary cost; S1 returns the Buy & Hold target once at the fixture's close
decision event. Python returns strategy decisions only. The existing Rust Fast Event path
interprets the S1 action and remains authoritative for orders, fills, costs, positions, cash,
marks, and the serialized portfolio projection. The report contains one warmup, five raw
callback samples, per-run latency statistics, callback-plus-Rust-account end-to-end samples,
Python version, event ordering, decision parity and portfolio checksum.

Use the repository Python 3.12 environment explicitly for both the resource-gated builds and
the benchmark. The recorder uses the shared root `target/`, captures the dependency graph,
wall time, target delta and before/after free space, and stops if the 10 GiB reserve is at risk:

```bash
PYO3_PYTHON="$PWD/.venv/bin/python" .venv/bin/python \
  poc/poc0-benchmark/capture-build-resource.py --profile dev --features b3-pyo3 \
  --output poc/poc0-benchmark/results/b3-pyo3-build-dev.json
PYO3_PYTHON="$PWD/.venv/bin/python" .venv/bin/python \
  poc/poc0-benchmark/capture-build-resource.py --profile release --features b3-pyo3 \
  --output poc/poc0-benchmark/results/b3-pyo3-build-release.json
PYO3_PYTHON="$PWD/.venv/bin/python" cargo run -p quant-research --release \
  --no-default-features --features b3-pyo3 --locked --offline -- \
  benchmark-poc0-b3 --output target/poc-0/b3-pyo3-callbacks.json
```

This fixed 3 ETF × 10 session fixture can establish basic callback parity and measure only
this tiny workload. It cannot identify a scale crossover, quantify GIL/parallel behavior, or
support an architecture choice. Build resource evidence is engineering cost and is not folded
into callback throughput.

The 2026-09-28 release run passed decision parity and compared callback-produced Python S1
decisions including date, symbol, and targets against Rust Native. A decision on another symbol
bar of the same date is rejected. The Python S1 target went through the same Rust account runner
against the independent fixture. All 29 present-bar events were delivered in date/symbol order;
the Python-derived order produced the same fills, daily ledger, costs, and final equity as the Rust reference (checksum
`f2ffcc44a2e94c778ad33e0731632bed69da98d05696f8934222f7e94f557928`). The authoritative sample is the
[review-fix rerun](results/b3-pyo3-callbacks-review-fix-2026-09-28.json), which includes the explicit
event-identity checks and the corrected 1e-8 float assertion: five-sample callback medians were
11.6 µs for Python empty, 12.7 µs for Python S1, 0.042 µs for Rust empty, and 0.209 µs for Rust S1.
Callback-plus-account end-to-end medians were 108.0 µs (Python empty), 109.3 µs (Python S1),
3.6 µs (Rust empty), and 5.7 µs (Rust S1). These values describe this tiny fixture and one warm
macOS ARM64 process; they are not a scale crossover or an architecture decision. The
[initial raw report](results/b3-pyo3-callbacks-2026-09-28.json) is the first, pre-fix run kept only
as earlier-run provenance; its Rust S1 median (0.292 µs) differs slightly, consistent with
sub-microsecond noise on this tiny fixture, and is not the number quoted above.

Initial warm feature builds took 83.14 s in dev (+2,427,198,271 logical target bytes) and
284.00 s in release (+434,471,616 bytes); later same-source warm verification rebuilds were
17.86 s in release (+1,022 logical target bytes). The configured Python environment occupied
162,369,399 bytes. See the [dev build](results/b3-pyo3-build-dev-2026-09-28.json),
[initial release build](results/b3-pyo3-build-release-2026-09-28.json), [final release rebuild](results/b3-pyo3-build-release-final-2026-09-28.json),
and [final callback test build](results/b3-pyo3-test-final-2026-09-28.json). Every recorded
build retained at least 10 GiB free; no target was cleaned. The event-identity review-fix test,
workspace test, workspace Clippy, B3 Clippy and release rebuild are recorded in their
`*-review-fix-2026-09-28.json` resource reports.

### B3 real strategies and parallel boundary (ticket 11)

The next stage compares S2 Momentum Rotation and S3 MA20/60 in Rust Native, Python `on_bar`,
and one-call-per-Run Python batch mode. The 3 ETF × 10 session fixture (and the versioned S3
MA20/60 fixture) gate strategy decisions and Rust account results against existing independent
goldens. The target workload is deterministic synthetic 64 instruments × 252 sessions. S2 uses
20/60 momentum, 20-session volatility, Top-5 and five-session rebalance; S3 extends the fixed
MA20/60 signal path across the target calendar. Scaled-workload parity is checked against Rust
Native; its generated input identity/hash is included in the report.

Before release measurements, the target gate was registered in ticket 11: for both S2 and S3,
Python per-bar must have parallel callback median no more than 2× Rust Native and end-to-end
parallel Runs/s (including Rust account replay) at least 80% of Rust Native to be eligible for
this target. If per-bar misses but batch passes both strategies, the result is `defer`; if neither
Python mode passes both, it is `reject` for this target workload. One warmup and five single-run
samples are used; two workers execute six independent Runs, with five repeated parallel groups.
Correctness is mandatory. Peak RSS is reported as a process high-water value; the embedded
interpreter and all candidates share one process, so RSS is not candidate-isolated and does not
decide the gate.

Rebuild under the 10 GiB resource gate and run the benchmark in release mode:

```bash
PYO3_PYTHON="$PWD/.venv/bin/python" .venv/bin/python \
  poc/poc0-benchmark/capture-build-resource.py --profile release --features b3-pyo3 \
  --output poc/poc0-benchmark/results/b3-strategies-release-build.json
PYO3_PYTHON="$PWD/.venv/bin/python" cargo run -p quant-research --release \
  --no-default-features --features b3-pyo3 --locked --offline -- \
  benchmark-poc0-b3-strategies \
  --output poc/poc0-benchmark/results/b3-pyo3-strategies-2026-09-28.json
```

The report separates strategy callback, initialization, Rust account replay, single-run
end-to-end, independent-run throughput, empty-call boundary baseline, and the GIL arrangement.
This compares Rust and Python directly and does not import ticket 09's incomparable Fast Event /
Nautilus throughput result. Conclusions are limited to this generated workload and host.

The 2026-09-28 release report passed the independent small-fixture golden and scaled Rust/Python
decision plus account checks. At the registered target, two-worker end-to-end throughput was:

| Strategy | Rust Native | Python per-bar | Python batch |
| --- | ---: | ---: | ---: |
| S2 Momentum Rotation | 126.04 Runs/s | 6.67 Runs/s | 14.48 Runs/s |
| S3 MA20/60 | 472.64 Runs/s | 11.58 Runs/s | 144.38 Runs/s |

Both Python modes missed the pre-registered gate for both strategies, so the result is `reject`
for this 64×252 target workload. This does not establish a general Python or GIL boundary. The
Python empty callback added 6.81–6.93 ms over the Rust empty loop for about 16.1k events; its
two-worker callback median was roughly 24× its single-run median on this host. Account replay
median was about 3.8–4.4 ms per Run and is included in Runs/s. Process peak RSS was 91,078,656
bytes, shared across embedded Python and all candidates, so it is not a per-candidate memory
comparison. Python 3.12.2 environment occupancy was 162,369,399 bytes. The final warm release
feature build took 19.51 s; logical target size changed by -960 bytes as shared-cache contents
changed. Raw samples and environment identity are in the [B3 strategy report](results/b3-pyo3-strategies-2026-09-28.json),
[release build record](results/b3-strategies-release-build-final-2026-09-28.json),
[strategy parity test](results/b3-strategy-parity-test-final-2026-09-28.json),
[workspace tests](results/b3-strategies-workspace-test-2026-09-28.json), and
[workspace Clippy](results/b3-strategies-workspace-clippy-2026-09-28.json).

## B2 Nautilus adapter comparison (ticket 08)

Use the guarded entry point for the pinned wheel, shared-target release build, and
correctness-first comparison. It skips installation when the exact wheel is already present:

```bash
.venv/bin/python poc/poc0-benchmark/nautilus_preflight.py \
  --output target/poc-0/nautilus-report.json \
  --preflight-record target/poc-0/nautilus-preflight.json \
  --build-record target/poc-0/nautilus-build.json
```

The wrapper measures free space before any pip install or Cargo build, reserves at least
10 GiB after a conservative completion estimate, monitors the build, and saves an
`unresolved` preflight record if the gate fails. A forced estimate failure is preserved in
[`space-gate refusal`](results/nautilus-preflight-space-blocked-2026-09-27.json).
The [successful preflight](results/nautilus-preflight-review-2026-09-27.json) and
[warm release build](results/nautilus-build-review-2026-09-27.json) record actual space,
commands and target changes. Cold build remains Unknown; no target was cleared.

`--backend nautilus` first writes the Rust reference report, then adds the Adapter projection
to that JSON file. The pin is `nautilus_trader==2.0.0rc5`; this run used Python 3.12 on
macOS ARM64. The Adapter maps each project instrument to a Nautilus `Equity`, converts daily
bars into separate synthetic open/close `QuoteTick`s, submits a market order only when the
next open quote arrives, and maps Nautilus fills into the project report fields. At each close,
the report also reads cash and net positions directly from Nautilus `Portfolio`; this direct
account snapshot is checked separately from the project ledger reconstructed from fills. The
Python process owns all Nautilus types and engine lifecycle.

Nautilus 2.x has no native next-bar-open mode for bar-only data, so the QuoteTick adapter is
the measured timing seam. The Adapter reads the fixture's 08:50 status at the 09:30 open.
B's Jan 13 `HALTED` status produces a **project-level** zero-quantity rejection without
submitting a Nautilus order. A new Nautilus order fills on Jan 14. The project events match
Rust's four attempts, but Nautilus receives only three orders; native HALTED rejection was
never exercised. Native order lifecycle and cross-engine throughput therefore remain
`unresolved`. Fill, direct account cash/positions, daily ledger and costs match on this fixed
fixture. The event sequence and hand calculation are in [ADR 0012](../../docs/decisions/0012-poc0-nautilus-status-gate.md).

The [review report](results/nautilus-adapter-review-2026-09-27.json) retains one warmup and
five raw conversion/initialization/event/end-to-end samples, with median, p95, range and a
stable projection hash. These samples are an internal Nautilus repeatability probe; no
cross-engine speed conclusion is drawn. The report includes Cargo.lock and pinned requirements
hashes, a resolved `pip freeze` snapshot hash, Rust/Python versions and build flags. Python
transitive packages are observed, not fully locked. The cold build and cold-start timing
remain Unknown. Earlier [Adapter-gated](results/nautilus-adapter-comparison-status-gated-2026-09-27.json)
and [initial](results/nautilus-adapter-comparison-2026-09-27.json) reports remain as history;
their pass/fail summaries are superseded by the separated native comparison above.

The earlier wheel installation and first warm-cache dev/release measurements remain in
[`install evidence`](results/nautilus-install-attempt-2026-09-27.json),
[`dev build`](results/nautilus-adapter-build-dev-2026-09-27.json) and
[`release build`](results/nautilus-adapter-build-release-2026-09-27.json).

## B2 S2/S3 registered decision protocol (ticket 09)

Registered before release measurements: the comparison subset is Fill quantity, price and
commission; cash, positions, daily NAV and total cost; single-Run latency, events/s, parallel
Runs/s and peak RSS. The halted-status native order lifecycle (including the project rejection
and native submission counts) is recorded separately and excluded under ADR 0012. To **adopt**
Fast Event on this workload, require at least 2x lower median single-Run latency **and** at
least 2x higher parallel Runs/s on each of S2 and S3, with no higher peak RSS, while all
common-subset correctness checks pass. Otherwise **defer** when both release candidates and
comparable execution scopes were measured; **reject** only on a demonstrated correctness
failure outside the excluded lifecycle. Without matched release workloads, a resource-gated
build, or an unavailable parallel/RSS measurement, retain **unresolved**. Build time and
target-byte delta are engineering costs shown separately, not included in events/s or Runs/s.
The absolute threshold is deliberately relative to the pinned Nautilus wheel on the same host.

Run the pinned Python comparison after the shared lightweight release build:

```bash
df -h .
cargo build -p quant-research --no-default-features --release --locked --offline
target/release/quant-research benchmark-poc0 --candidate soa --output target/poc-0/b2-rust.json
.venv/bin/python poc/poc0-benchmark/nautilus_adapter.py \
  --dataset poc/poc0-benchmark/fixtures/dataset-v1.json \
  --reference-report target/poc-0/b2-rust.json \
  --output target/poc-0/b2-comparison.json
```

S2 uses the fixed 3 ETF x 10 session golden: at Jan 7 close C is selected, its next open
is UNKNOWN; B is bought Jan 9, its Jan 13 HALTED exit defers the switch, then B sells and
A buys at Jan 14 open. A newer close target supersedes any older pending target. S3 uses
the versioned 3 instrument x 130 weekday fixture `fixtures/b2-ma20-60-v1.json`: the
first valid MA60 is session index 59; the cross-up at index 60 buys A the following
open, and the cross-down at index 85 sells A the following open. Both paths use
100,000 CNY initial cash, 100-share lots, 10 bps per-side slippage and 100 CNY minimum
commission. The synthetic prices and calendar do not establish historical PIT returns.
The S2 factor/rank/TopK projection is already checked against the existing fixed B1
workload; the S3 transitions, cash and costs have separately worked fixture assertions.

Rust per-strategy samples exclude dataset preparation, while the Nautilus end-to-end
samples include quote conversion and engine initialization. These timings are **internal
repeatability evidence**, not matched-scoped engine speed ratios. Unless a matched release
parallel and RSS protocol is run, the ticket 09 architecture selection remains unresolved.

For the isolated Rust parallel worker measurement (fixture golden gates run before timing):

```bash
.venv/bin/python poc/poc0-benchmark/measure-b2-rss.py --strategy s2 \
  --output poc/poc0-benchmark/results/b2-09-s2-throughput.json
.venv/bin/python poc/poc0-benchmark/measure-b2-rss.py --strategy s3 \
  --output poc/poc0-benchmark/results/b2-09-s3-throughput.json
```

The Nautilus combined report records two warm processes and six raw process-pool samples
per S2/S3 strategy with per-worker peak RSS. Rust uses two threads sharing prepared bars;
its isolated throughput samples exclude conversion and account-engine initialization.
The Rust RSS wrapper records the **whole process** (including its golden preflight), not a
per-thread peak, so that number cannot be compared to the Nautilus per-worker RSS. The
S3 input and independent checkpoint expectation are versioned as
`fixtures/b2-ma20-60-v1.json` and `fixtures/b2-ma20-60-expected-v1.json`.
These distinct scopes and worker models are
not comparable as engine-speed ratios; the registered adoption threshold cannot be evaluated
from them. Neither throughput nor S3's synthetic prices are investment-performance evidence.

### S2 matched remeasure tracer (ticket `poc-0-b2-matched-remeasure/02`)

`b2_matched.py` starts the Rust S2 golden preflight as a separate process, checks the Nautilus
S2 projection against the Rust projection (keeping ADR 0012's native order-lifecycle field
separate), and exposes the registered pure decision function. The Rust B2 CLI supports
`--mode serial|parallel`, `--workers`, `--runs`, and `--skip-golden-preflight` with the checksum
from that preflight. If the exact Nautilus wheel is absent or either correctness gate fails,
the report preserves the reason and no timing is used for a decision.

```bash
python3 poc/poc0-benchmark/capture-build-resource.py --profile release \
  --scope poc --action build --estimated-max-additional-bytes 2147483648 \
  --output poc/poc0-benchmark/results/b2-matched-s2-release-build-boundary-2026-09-28.json
.venv/bin/python poc/poc0-benchmark/b2_matched.py \
  --run-measurements \
  --binary target/release/quant-research \
  --build-record poc/poc0-benchmark/results/b2-matched-s2-release-build-boundary-2026-09-28.json \
  --output poc/poc0-benchmark/results/b2-matched-s2-2026-09-28.json
```

Use `--run-measurements` only after the registered ADR/prediction revision. The recorded S2
run used the registered safe fallback: each Nautilus worker converts and caches QuoteTicks
once, then constructs a fresh engine and strategy per Run. Rust workers start and warm before
their timer; both paths exclude process/thread startup, warmups, serialization and checksum.
The Nautilus worker result contains only run identity, elapsed time, checksum and RSS; every
checksum must match the independently checked Nautilus projection before samples are accepted.
Nautilus parallel workers each process three of the six measured Runs; their completion timestamp
is captured before projection serialization/checksum and before result IPC is collected. The
report verifies both initialized workers contributed a measured batch.
The 2026-09-28 S2 report applies the unchanged ticket 09 gates to S2 only and records `adopt`;
it does not decide S3, 64×252 robustness or overall B2. Engine-reset parity remains unverified.
The S2 sample medians were 11,042 ns (Rust) vs 842,146 ns (Nautilus) serial latency,
56,338 vs 1,225 Runs/s across five alternating 2-worker × 6 Run groups, and 11,255,808 vs
147,046,400 bytes peak RSS (Nautilus worker-sum upper bound). The report embeds every raw
sample and links its preflight, build record, and standalone measurement JSON files.

The report's `maintenance_cost_proxies` section records descriptive implementation and test
counts, incremental direct dependencies, the installed transitive dependency closure reachable
from the pinned Nautilus distribution, and known semantic differences with citations. Rust LOC
excludes inline `#[cfg(test)]` modules; Python LOC excludes blank and comment-only lines. The
Rust count covers the shared POC benchmark module and CLI, so it includes neighboring candidate
code in that shared module. The Nautilus count covers `nautilus_adapter.py` and excludes the
coordinator and measurement wrappers. Test counts cover four named Rust B2 CLI cases and all
test methods in the listed B2/Nautilus Python test modules. The current Nautilus adapter adds no
Cargo crate; its direct Python requirement is `nautilus_trader==2.0.0rc5`. The dependency
closure is read from the measurement Python environment's installed metadata with active
environment markers, and the report saves each resolved package version. The snapshot is not a
complete Python lock because only the direct Nautilus requirement is pinned. If that pinned
environment, an active declared dependency, or a version satisfying its requirement is
unavailable—or requirement metadata cannot be parsed—its count is `Unknown`, never zero. These
proxies are recorded only; the decision function does not read them.
