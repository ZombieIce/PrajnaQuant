# MVP-4 conversion cost and worker RSS

Issue #142 / #133 A16. **Record only, no thresholds.** These synthetic execution
measurements do not select a production Engine, establish real-market performance,
prove PIT/total return, or replace the independent A15 correctness acceptance.

## Reproduce

Use a dedicated Python 3.12 environment with the existing
[rc6 requirements](../../backends/nautilus/requirements.txt). Do not upgrade the
POC-0 rc5 environment or clear shared caches. From a committed checkout:

```sh
python poc/mvp4-cost/measure.py run --out /tmp/prajna-mvp4-cost/raw-measurements.json
python poc/mvp4-cost/measure.py summarize \
  --input /tmp/prajna-mvp4-cost/raw-measurements.json \
  --out /tmp/prajna-mvp4-cost/recomputed-summary.json
python -m unittest tests.test_mvp4_cost tests.test_mvp4_fixture_prepare -v
python -m unittest discover -s poc/mvp4-cost -v
```

`run` builds the feature-gated `measure_accurate` release example and A15 test,
then **reruns real A15 first**. Failed A15 blocks all timing. The exact commands,
exit codes, logs, monotonic elapsed times, free-space samples, source/lock/binary
hashes, Backend pin, machine, toolchain, cost parameters, fixture hashes and DSV
are saved. `CARGO_TARGET_DIR` is honored; default is the current worktree target.
Use a new output location and preserve its command logs.

Builds require a conservative 6 GiB additional allowance above the 10 GiB
reserve. Runtime commands require 64 MiB above it. Active commands are stopped at
11 GiB free (a cushion above the reserve), with partial records preserved.
Blocked or aborted measurements are **missing**, never estimated. No cache is
removed, and warm build time is not cold-build cost. The initial development
bootstrap is not a formal cold-build measurement.

## Measurement boundary

The Rust harness uses the public `run_adapter` with the real pinned worker,
not a Python execution rewrite or pre-generated Fast order/fill replay. Rust
still sizes every Order from actual Backend feedback and owns costs and the
two-layer ledger. Each sample verifies the same exact projection as A15 against
Fast after stopping the timer (orders, fills, attempts, both valuation marks and
valuation sources). The 64x252 projection check is **robustness only**, without
an independent gold standard.

- **Main:** each worker has a prepared canonical packet and converted Backend
  instruments, QuoteTicks and statuses. Timed work includes cloning the Decision
  tape, a new engine/strategy, data registration, closed-loop execution, Domain
  Fill/result construction, disposal and the worker usage acknowledgement.
- **Secondary end-to-end:** starts with the same canonical in-memory RunPacket,
  writes its Parquet/config packet, decodes/converts Backend objects, initializes
  and executes the engine, and returns Domain results. Backend conversion is
  additionally recorded as its own `perf_counter_ns` segment.
- **Excluded:** fixture/oracle generation, normalization/publication/loading,
  process startup, projection serialization, equality checks and checksum.
  Factors/signals are precomputed **Decision tapes**, as in A15 execution
  acceptance. This is an Adapter/closed-loop execution cost protocol, not full
  strategy-generation throughput or production Experiment CLI end-to-end time.
- **Actual mode:** `cached_conversion_new_engine`, matching the production
  new-engine lifecycle. `reset()` is not benchmarked; no rc6 reset failure is
  asserted. The persistent Python process is benchmark-only; production
  `WorkerCommand` still starts one process per Run.

There are two warmups, 20 main samples, 20 secondary samples, then five groups of
two native Python worker processes × six Runs. Rust transport threads drive those
processes; they are not Rayon Backend workers. Group wall time excludes pool
startup/preparation/warmup and post-run verification/serialization/teardown, but
includes protocol/RSS acknowledgement overhead. Per-worker full results stay in
the Rust controller until untimed verification. No Fast speed ratio is reported.
Serial latency has median, nearest-rank p95 and range; parallel Runs/s is
12 divided by group wall time, with all five values retained.

## RSS and maintenance proxies

`ru_maxrss` is whole-process high-water RSS, raw bytes on macOS / KiB on Linux
converted to bytes. Every worker reports its post-import, pre-conversion empty
baseline and sample peaks. Two worker peaks are summed and explicitly labelled
an **upper bound** (peaks need not be simultaneous). The Rust controller,
orchestrator and separate A15 process are excluded. Serial RSS includes both hot
and conversion samples and is not an incremental Engine-memory claim. OS caches
and unrelated host load are not controlled.

The raw record saves exact paths and rules for nonblank/non-comment LOC:
Rust Adapter source plus production Rust process transport; Python worker and
packet reader; tests and benchmark-only code excluded. Rust inline `cfg(test)`
suffixes are excluded, attributes/docstrings count. Test counts are static
`#[test]` / Python `test_*` definitions, not runtime case/Run counts.
Rust dependency counts use `cargo tree -p prajna-adapter --edges normal`, with
crate/version pairs distinguished; the wider Experiment host graph is not
attributed to the Adapter. Python direct requirements and installed dependency
closure are separate; inactive extras are excluded. These are maintenance
**proxies**, not labor estimates or a production dependency audit.

## Recorded outcome and limitations

The [raw measurements](raw-measurements.json.gz),
[summary](summary.json), [command logs](command-logs.tar.gz) and
[A15 prerequisite rerun](correctness.json.gz) preserve the local evidence.
Machine: Apple M1, 8 cores, 16 GiB RAM, macOS ARM64, Python 3.12.2,
Rust 1.98.1, `nautilus_trader==2.0.0rc6`, release profile. Exact source identity
and all numeric series are in the raw record; evidence commits do not alter the
measured runtime.

Measured source: `ca9a233a19290b20276b4ba0381479e97efc8412` (clean).
Formal warm release build: 0.509 s; cold build remains Unknown.
Minimum free space during the cost protocol: 34.866 GiB.

| Fixture / strategy | Main median ms | Secondary median ms | Backend conversion median ms | 2-worker median Runs/s | Worker RSS sum upper bound MiB |
| --- | ---: | ---: | ---: | ---: | ---: |
| hand-v1 / S1 | 1.177 | 5.935 | 3.113 | 1286.57 | 196.11 |
| hand-retry-v1 / S1 | 1.514 | 6.629 | 3.257 | 1003.51 | 196.78 |
| hand-rebalance-v1 / S2 | 1.981 | 7.438 | 3.361 | 780.93 | 197.72 |
| hand-fee-only-v1 / S2 | 1.623 | 6.682 | 3.263 | 963.58 | 196.91 |
| dataset-v1 / S1 | 2.615 | 8.745 | 3.905 | 597.82 | 198.27 |
| dataset-v1 / S2 | 2.562 | 8.456 | 3.825 | 620.29 | 198.36 |
| dataset-v1 / S3 | 2.084 | 7.896 | 3.834 | 740.82 | 194.98 |
| b2-ma20-60-v1 / S3 | 23.587 | 39.094 | 12.812 | 78.02 | 207.67 |
| 64x252 / S3 | 499.560 | 924.019 | 386.603 | 3.52 | 422.59 |
| 64x252 / S2 | Missing | Missing | Missing | Missing | Missing |

The two-worker empty baseline sums are 138.73–139.20 MiB; every individual
baseline/peak is retained. Maintenance proxies: Rust 1144 / Python 486 LOC;
27 Rust test definitions / 8 Python tests; Rust 6 direct + 189 transitive
crate/version dependencies, Python 2 direct + 0 active transitive packages.
LOC, tests and dependency scopes follow the rules above, not arbitrary whole
workspace counts. RSS is not corrected by subtracting the baseline.

Eight fixed cases and S3 64x252 are measured. **S2 64x252 is missing:** the
original Instrument tick is 0.01, while its 10 bps synthetic prices are not
always tick-aligned. The real worker reports `unrepresentable price`; prices,
costs and Instrument precision are not changed. The owner explicitly chose
missing data over rounding or a precision variant. S3's original prices are
representable, so its 64x252 costs can be recorded; this does not make it
independent correctness evidence. No estimate fills the S2 gap and no conclusion
is extrapolated to it.

Linux runtime, cold build cost, reset throughput, production per-Run process
startup cost, full strategy-generation cost and real-market performance remain
Unknown. Existing process-group cleanup warnings in the separate A15 rerun are
preserved, not suppressed. Verification results and environment limits belong
in the PR Handoff and review conclusions in PR comments.
