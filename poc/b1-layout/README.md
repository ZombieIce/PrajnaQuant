# POC-0 B1: scalar factor access slice

This first slice measures the same momentum calculation over custom SoA, Arrow `Float64Array`, and Polars `Series`. It checks an independent two-series hand case, then checks all output counts and checksums before reporting timings. Dependencies are pinned in this crate's lockfile (Polars 0.55.2, Arrow 60.0.0). It isolates scalar column access; it does **not** measure Polars lazy expressions, Parquet decoding, ranking, portfolio returns, or an out-of-core path. Those remain B1 work under [`docs/poc-0-benchmark-spec.md`](../../docs/poc-0-benchmark-spec.md).

```bash
cargo run --manifest-path poc/b1-layout/Cargo.toml --release --offline -- 128 4096 20 10
python3 poc/b1-layout/capture.py poc/b1-layout/results/local-smoke.json
```

Arguments: `symbols days lookback repetitions`. The first pass for each candidate is discarded as warmup. The binary prints input SHA-256 and one JSON object per candidate with raw sample durations, median/p95 microseconds, evaluated factor values, and checksum. It uses deterministic generated prices with a fixed missing-value pattern. Candidates run in a fixed order, and CPU model was unavailable in the first local run; the saved result is a smoke measurement, not an architecture decision. Do not compare timings from separate hosts or builds without recording the environment.

The capture command saves input/build provenance and the raw timing samples. The first saved smoke output is [`results/2026-09-26-local-smoke.json`](results/2026-09-26-local-smoke.json). Complete B1 must add equivalent Polars expression, Parquet scan, ranking/TopK, portfolio return, conversion, memory and out-of-core workloads before choosing a hot-path layout.
