# ADR 0010：组合账户为权威，Vector 保留矩阵计算路径

- 状态：Accepted as target architecture，2026-09-26；尚未实现。Fast Event L1 的账本数值、Virtual Portfolio 子账与 Vector 对拍口径由 [`ADR 0019`](0019-fast-event-ledger-and-vector-parity.md) 细化

多策略和跨 Venue 资金管理必须以同一 Trading Account 的现金、仓位、成本和风险为权威，Virtual Portfolio 只是受分配资本的策略视图，合并时不得重复计算资产。Fast/Accurate Engine 由 Order → Fill → Position → Account 更新账本，并逐日核对权益与现金及持仓市值。Vector Engine 直接按权重与收益计算，以保持大规模研究吞吐；它必须报告对成交、成本、现金、杠杆和再平衡的简化假设，不伪造逐笔 Fill 或账户恒等式证据。

三级引擎共同输出带能力/明细可用性标记的 `PortfolioResult`、`Metrics` 与 `ExperimentResult`。一致性测试使用同一日历、信号、估值、成本与可执行性子集；模型不同时记录可解释差异。现有 ETF 账户与成本口径由 [`ADR 0005`](0005-defer-rebalance-on-unavailable-exit.md)、[`ADR 0006`](0006-instrument-pnl-attribution.md)、[`ADR 0007`](0007-execution-status-gate.md) 继续约束，本 ADR 不改变其已实现时序。
