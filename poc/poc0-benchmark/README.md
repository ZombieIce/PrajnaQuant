# POC-0 benchmark entry

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

The input fixture records its seed identity, ten-session calendar, three instruments, generated OHLCV rules, one missing bar, per-session execution status and availability time, strategy parameters, costs, and time model. Its expanded content hash is pinned by the independent expected file. The expected projection separately lists each score/rank and target, the UNKNOWN and HALTED order rejections, fills, cash/holdings/NAV by date, and aggregate costs. Float comparisons use a fixed absolute tolerance of `1e-8`; discrete fields compare exactly.

The command currently runs the existing Rust ETF backtest as a reference candidate. It checks the independent projection and account identity before recording one warmup and five raw timing samples. The samples cover the backtest call only; fixture parsing and report projection are outside the timer. Peak RSS is currently reported as unknown. Machine, CPU, memory, Rust version, lockfile hash, Git revision, tracked diff hash/summary, changed paths, and dirty-worktree completeness are recorded when available. An untracked worktree is explicitly marked incomplete because untracked file contents are not included in the Git diff hash.

This fixture is an event ledger. Future Vector candidates use a separately declared weight-times-return model and should only be compared under shared assumptions; their simplified returns are not asserted to reproduce event-account NAV. This single small correctness fixture establishes the CLI contract; its timings do not support an architecture or performance decision. The seed is a stable fixture identity; its hand-authored prices are not random samples.
