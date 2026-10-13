# A16 corrected ADR 0013 timing

Issue #152 repairs #133 A16 without changing ADR 0013 or the closed #142
conclusion. **Record only; no performance thresholds or Engine selection.**
The original [v1 report](../README.md), raw samples and logs remain immutable
execution-only evidence. New `mvp4.cost@2` samples include per-Run Rust
research and replace v1 latency, throughput and RSS series for A16. They
must never be combined with v1 samples. The A15 gate remains separate.

## Boundary mapping

| ADR 0013 work | Corrected main Run |
| --- | --- |
| Strategy instance creation | `Workload::portfolios` creates a fresh native `VectorStrategy`, Factor definitions and `PortfolioRun` after the first timer read |
| Factor/signal computation | `VectorStrategy::decide_panel` runs on the canonical Panel every Run, including the complete uncached RotationScore DAG for S2 and trailing MA20/60 gaps/crossover signals for S3; no FactorCache or computed Decision reuse |
| Event processing | New real rc6 engine/strategy, cached Backend object registration, Session/Order/Fill lockstep |
| Account advancement | Rust shared execution state, sizing/costs and scale-18 Trading Account / Virtual Portfolio ledger use actual Backend feedback |
| Result projection | `run_adapter` completes Domain Fill and per-Session result construction before the final timer read |
| Serialization/checksum | Full projection, independent Decision checks, equality and SHA-256 run after the timer stops |

S1, including hand S1 fixtures, has only a fresh first-Session equal-weight
allocation; it has **no rolling factor workload**. There is no invented
factor load. The hand S2 fixtures do define Momentum Rotation: their real
rolling factors/signals are recomputed, not treated as manual tapes.

One-time work: read/normalize/publish/load immutable bars and statuses, parse
configuration, build the canonical RunPacket, generate independent expected
Decisions/execution, spawn workers, convert Backend instruments/quotes/statuses,
and warm up. The oracle tape is only an untimed independent expected value,
never the input to a measured execution.

Main includes a separately marked research segment, engine creation, execution,
Domain result generation, disposal and the worker RSS acknowledgement.
Secondary timing also starts from canonical in-memory bars and recomputes
research, then writes the packet/config, decodes/converts Backend objects,
initializes and executes to Domain Fill/results. It is separately reported,
not production CLI time. Conversion has an additional Backend clock segment.
Normalization/publication/loading and process startup remain outside.

Actual engine mode is `cached_conversion_new_engine`, following production:
each Run creates/disposes an engine and shim strategy. Reset is not certified;
no rc6 reset failure is asserted. Worker processes persist only in this cost
harness; production still starts a process per Run.

## Deterministic regression and correctness

The Run-boundary Rust tests use a controlled clock and observe actual native
research invocations: the first timer read must precede calculation, subsequent
reads require exactly one new invocation per Run. Moving calculation outside
the timed interval or reusing a prior Run fails without any machine-speed
threshold. A second Run with reversed S2 momentum weights must change the
independently known first target from C to B. CI runs these feature-gated example
tests without Nautilus; they are **not** Backend correctness evidence.

Every measured Run independently compares Decision dates/targets/rank ordering
exactly and scores at absolute tolerance `1e-12`, then compares the complete
execution projection with Fast at exact scale 18. These comparisons and
checksums remain outside timing. The separate real locked rc6 A15 test must
first pass all 13 fixed Runs, with zero Adapter defects or unclassified
differences; fake/skip/performance samples cannot replace it.

Two warmups; 20 main and 20 secondary serial Runs; five groups of two native
worker processes times six Runs. The controller starts the group timer before
releasing the execution barrier and stops after both workers finish all Runs.
Summary validation retains the check that group wall time encloses each
worker's summed timed execution. Group wall excludes startup, preparation,
warmup, post-Run checksums and teardown. All raw timings and group walls remain
available; throughput is 12 divided by group wall, not summed worker durations.

## Reproduce

Use Python 3.12 and the pinned [rc6 requirements](../../../backends/nautilus/requirements.txt)
in a dedicated environment; do not upgrade the archived rc5 environment.
From the recorded source revision:

```sh
python poc/mvp4-cost/measure.py run \
  --out /tmp/prajna-mvp4-cost-adr0013/raw-measurements.json
python poc/mvp4-cost/measure.py summarize \
  --input /tmp/prajna-mvp4-cost-adr0013/raw-measurements.json \
  --out /tmp/prajna-mvp4-cost-adr0013/recomputed-summary.json
cargo test -p prajna-experiment --features nautilus-integration \
  --example measure_accurate --locked --offline
python -m unittest tests.test_mvp4_cost tests.test_mvp4_fixture_prepare -v
python -m unittest discover -s poc/mvp4-cost -v
```

To recompute archived evidence, decompress its raw gzip into a new temporary
file and pass that file to `summarize`; v1 backward compatibility preserves its
summary shape. Keep command logs beside each newly produced raw record.

## Resources and limitations

The same 10 GiB free-space gate, conservative 6 GiB release-build allowance and
11 GiB active-command stop cushion apply. No shared cache is cleared; warm
build cost is not cold build cost. Per-worker RSS retains `ru_maxrss` raw
value/unit (bytes on macOS, KiB on Linux), normalized bytes and pre-conversion
post-import baseline. Two worker peaks are summed as an **upper bound**, not
simultaneous RSS. Rust controller/orchestrator and A15 process are excluded;
Rust factor computation is therefore not included in worker RSS. This remains
Backend worker memory attribution, not full platform memory consumption.
Maintenance proxies retain the original LOC/test/dependency scopes and rules;
they are not labor estimates.

S2 64x252 keeps its original Instrument tick `0.01`. Unrepresentable synthetic
slippage quotes remain **missing**, not rounded or replaced by a precision
variant; neither its cost nor robustness is passed. S3 64x252 is synthetic
robustness/cost evidence without an independent hand gold standard.
Linux runtime, cold build, reset throughput, production per-Run process startup,
real-market execution, PIT and total return remain Unknown/unverified.

## Recorded outcome

The [raw measurements](raw-measurements.json.gz),
[recomputable summary](summary.json), [real A15 rerun](correctness.json.gz)
and [command logs](command-logs.tar.gz) preserve the corrected evidence.
Measured source is clean revision
`59ea2ff1dff5aeb0e56673cd834c306a352cecfc`; later evidence/documentation commits
do not change measured runtime. Machine: Apple M1, 8 logical cores, 16 GiB,
macOS 26.2 ARM64; Python 3.12.2, Rust 1.98.1, rc6, release profile.
Exact source/binary/lock/fixture hashes, effective parameters, DSV, command
identities, space snapshots and raw units are in the record.

A15 passed before any cost sample: 13 fixed Runs, no Adapter defect or
unclassified difference; its two Rust test functions passed with zero ignored.
Every successful timed Run reproduced the independently expected Decisions
and exact Fast execution projection. The 64x252 check remains weaker
robustness evidence, not a hand gold standard.

| Fixture / strategy | Main median ms | Research median ms (within main) | Secondary median ms | Backend conversion median ms (within secondary) | 2-worker median Runs/s | Worker RSS sum upper bound MiB |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| hand-v1 / S1 | 1.156 | 0.001 | 6.050 | 3.067 | 1426.97 | 195.64 |
| hand-retry-v1 / S1 | 1.703 | 0.001 | 7.013 | 3.445 | 1000.49 | 196.72 |
| hand-rebalance-v1 / S2 | 4.553 | 2.089 | 10.068 | 3.480 | 391.29 | 197.52 |
| hand-fee-only-v1 / S2 | 3.998 | 1.892 | 9.410 | 3.430 | 429.65 | 197.08 |
| dataset-v1 / S1 | 2.617 | 0.001 | 8.742 | 3.915 | 595.86 | 198.27 |
| dataset-v1 / S2 | 5.383 | 2.170 | 11.202 | 3.931 | 337.83 | 198.06 |
| dataset-v1 / S3 | 2.093 | 0.010 | 8.069 | 4.090 | 752.64 | 195.20 |
| b2-ma20-60-v1 / S3 | 25.784 | 1.565 | 41.274 | 13.039 | 72.10 | 207.39 |
| 64x252 / S3 | 622.153 | 112.697 | 1051.449 | 395.009 | 2.89 | 422.22 |
| 64x252 / S2 | Missing | Missing | Missing | Missing | Missing | Missing |

The full summary also retains median, nearest-rank p95, minimum/maximum and
all serial latency, research segment and parallel group wall/throughput values.
Worker empty baseline sums range from 138.73 to 138.94 MiB, without subtraction
from peaks. Maintenance proxies are unchanged in their recorded scopes:
Rust 1144 / Python 486 non-test LOC, 27 Rust / 8 Python test definitions,
6 direct + 189 transitive Rust crate/version dependencies, and 2 direct +
0 active transitive Python packages. No timing conclusions are drawn from
these proxies.

The guarded warm release build took 215.389 s and honored the existing shared
`CARGO_TARGET_DIR`; no cache was cleared. Minimum protocol free space was
30.597 GiB. The expected S2 mapping failure is preserved in `command-29.log`
as missing, not counted as measured or estimated. No other formal command
failed. Public `summarize` reproduced this summary byte-for-byte and also
reproduced the immutable v1 summary. Different host load/cache states and
different boundaries prohibit interpreting old/new latency changes as a
performance improvement.
