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
`setrlimit` failed before launching the child, so the constrained-memory acceptance remains
unresolved. The unrestricted 10-million-row run and failure evidence are recorded in
[`ticket 05`](../../.scratch/poc-0-benchmark/issues/05-parquet-and-out-of-core.md).

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

## B2 Fast Event Buy & Hold prototype

The report also emits `b2_fast_event_buy_hold`, an independent, deliberately small L1 event path. It creates an equal-weight Buy & Hold target from the fixture's Jan 12 close, submits market buys at subsequent opens, rejects the halted B order on Jan 13, and retries it at the next available open. It marks an existing B holding at its last observed close when the Jan 16 B bar is missing. Orders, fills, fixed costs, cash, holdings, daily NAV, and a stable projection checksum are included. The harness checks repeat-run equality, next-session ordering, the NAV identity, the halt rejection, stale marking, and non-zero costs before collecting one warmup and five timing samples. It separately records day-index initialization, event processing, and end-to-end samples. The CLI test also removes A's later bars and enables buy tax to check the final unexecuted target and non-negative cash.

This is a prototype and a correctness smoke on one tiny fixture. It does not establish production Fast Event semantics, broad benchmark performance, or an architecture decision; Nautilus comparison remains a separate ticket.
