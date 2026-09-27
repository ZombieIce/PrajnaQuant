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

## B2 Fast Event Buy & Hold prototype

The report also emits `b2_fast_event_buy_hold`, an independent, deliberately small L1 event path. It creates an equal-weight Buy & Hold target from the fixture's Jan 12 close, submits market buys at subsequent opens, rejects the halted B order on Jan 13, and retries it at the next available open. It marks an existing B holding at its last observed close when the Jan 16 B bar is missing. Orders, fills, fixed costs, cash, holdings, daily NAV, and a stable projection checksum are included. The harness checks repeat-run equality, next-session ordering, the NAV identity, the halt rejection, stale marking, and non-zero costs before collecting one warmup and five timing samples. It separately records day-index initialization, event processing, and end-to-end samples. The CLI test also removes A's later bars and enables buy tax to check the final unexecuted target and non-negative cash.

This is a prototype and a correctness smoke on one tiny fixture. It does not establish production Fast Event semantics, broad benchmark performance, or an architecture decision; Nautilus comparison remains a separate ticket.
