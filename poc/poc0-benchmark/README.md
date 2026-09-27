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

## B2 Nautilus adapter comparison (ticket 08)

The Nautilus candidate is selected through the same correctness-first command. The default
Python executable is the repository `.venv/bin/python`; install the pinned wheel first:

```bash
.venv/bin/python -m pip install -r poc/poc0-benchmark/requirements-nautilus.txt
cargo run -p quant-research --no-default-features --locked --offline -- \
  benchmark-poc0 --backend nautilus \
  --output target/poc-0/nautilus-report.json
```

`--backend nautilus` first writes the Rust reference report, then adds the adapter projection
to that same JSON file. The pin is `nautilus_trader==2.0.0rc5`; this run used Python 3.12 on
macOS ARM64. The Adapter maps each project instrument to a Nautilus `Equity`, converts daily
bars into separate synthetic open/close `QuoteTick`s, submits a market order only when the
next open quote arrives, and maps Nautilus fills into the project report fields. At each close,
the report also reads cash and net positions directly from Nautilus `Portfolio`; this direct
account snapshot is checked separately from the project ledger reconstructed from fills. The
Python process owns all Nautilus types and engine lifecycle.

Nautilus 2.x has no native next-bar-open mode for bar-only data, so the QuoteTick adapter is
the measured timing seam. All six fixed-fixture checks pass: exact project order attempts and
reasons, fill quantity/price/commission, direct account cash/positions, daily cash/holdings/NAV,
and total cost. The Adapter reads the fixture's 08:50 status at the 09:30 open. B's Jan 13
`HALTED` status produces a project rejection with zero quantity; no B order is submitted to
Nautilus that day. The next tradable open creates a new Nautilus order and fills on Jan 14.
This is an Adapter status gate, not a native Nautilus matching-engine rejection. Synthetic
quotes, spread-based slippage and fixed per-fill commission remain documented comparison
boundaries. One conversion/initialization/event/end-to-end timing sample is a correctness
probe, not throughput evidence. See the [status-gated comparison](results/nautilus-adapter-comparison-status-gated-2026-09-27.json)
and the preserved [initial lifecycle mismatch](results/nautilus-adapter-comparison-2026-09-27.json).

The first restricted pip attempt failed DNS; the fixed wheel was then installed successfully
with the available approved network path. The space gate remained above 10 GiB. Install and
build identity are in [`install/build resources`](results/nautilus-install-attempt-2026-09-27.json),
with raw shared-target dev and release records in [`dev build`](results/nautilus-adapter-build-dev-2026-09-27.json)
and [`release build`](results/nautilus-adapter-build-release-2026-09-27.json). No target cache
was cleaned.
