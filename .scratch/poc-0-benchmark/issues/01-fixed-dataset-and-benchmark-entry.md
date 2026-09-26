# 01: 固定数据集与统一基准入口

**What to build:** 研究者可以通过一条命令，以固定种子、版本和配置运行 POC-0 的首个参考场景，并取得可追溯的正确性报告。独立给定的 3 ETF × 10 日预期结果作为后续候选共用的金标准，包含缺 bar、不可成交状态、非零成本和 T 收盘信号到下一可用 open 的时序。

**Blocked by:** None (can start immediately).

**Status:** resolved

- [x] 固定输入覆盖交易日历、OHLCV、可用时刻、缺失/状态规则；报告记录 Dataset Version、内容 hash、seed、有效配置与参考策略版本。
- [x] 独立预期结果列出信号、目标、成交、现金、持仓、成本和逐日 NAV；末日未执行目标、缺价与不可成交状态有明确预期。
- [x] 命令输出机器可读的正确性状态、差异和结果 checksum；不通过金标准时不产生可供架构决策的性能结论。
- [x] 报告记录代码修订或脏工作树差异摘要、依赖/编译/机器环境，以及原始测量样本所需的共同字段；无法取得的信息显式标为未知。
- [x] 金标准说明 Vector 简化收益与事件账本的不同假设，未来收益只用于评价，不进入策略决策。

## Comments

- Implemented the `benchmark-poc0` CLI and pinned the three-ETF/ten-session dataset and independent ledger. The report records input/content hashes, strategy and cost configuration, provenance, correctness differences, checksum, and five raw reference-run timings only after the golden passes. Peak RSS remains explicitly unknown in this ticket.
- Verified: `cargo fmt --all -- --check`; `cargo test --workspace --locked --offline` (81 passed, 1 ignored); `cargo clippy --workspace --all-targets --locked --offline -- -D warnings`; and `cargo run -p quant-research --release --locked --offline -- benchmark-poc0` (correctness passed).
- Accepted on 2026-09-27 at the user's request. Fresh verification: `cargo fmt --all -- --check`; `cargo test --workspace --locked --offline` (82 passed, 1 ignored); `cargo clippy --workspace --all-targets --locked --offline -- -D warnings`; release `benchmark-poc0` (correctness passed, no differences/accounting/time errors, five raw reference samples). The wrong-golden CLI case exits unsuccessfully and skips timing. Peak RSS remains explicitly unknown; the tiny fixture does not support an architecture performance decision.
