# 08: 64×252 S2 跨候选差异诊断

**What to build:** 项目负责人能从一个可人工验算的最小用例得知：64×252 S2 上 Rust 与 Nautilus 的订单和成交差异（Rust 351 / Nautilus 348；首个差异为 ETF038，Rust 2025-04-08 成交，Nautilus 2025-04-09 成交）究竟属于 Fast Event 错误、Adapter 错误，还是不可消除的语义差异；并按 [ADR 0014](../../../docs/decisions/0014-b2-correctness-failure-attribution.md) 给出该负载的归因结论。参见 [spec](../spec.md)。

**Blocked by:** None (can start immediately)

**Status:** ready-for-agent

- [ ] 从固定的 `poc0.b3.s2-scale-64x252.v1` 数据中取出首个差异附近的事件序列，按项目时间模型逐步写出：T 日收盘可观测 → 形成信号 → 下一可用日 08:50 状态可用 → 09:30 open 执行。覆盖 ETF038 在 2025-04-07 至 04-09 的 bar 是否存在、执行状态、日历、UNKNOWN/HALTED/缺 bar 规则，以及当日持仓与现金约束。
- [ ] 把首个差异缩成一个手算、可人工验算的小 fixture：只保留必要的 instrument 和交易日，期望值独立写出，不由任何被测 Engine 生成。Rust Fast Event 与 Nautilus Adapter 分别运行该 fixture，并与期望值逐字段对拍。
- [ ] 结论只能取以下三种之一，每种都附证据：
  - Fast Event 违反独立期望 → 按 ADR 0014 判 `reject`，写明违反的规则；
  - Adapter 违反独立期望 → 修正 Adapter，新增回归测试，并按原协议重跑票据 04 的 S2 稳健性负载（正确性门以及通过后的性能采集）；
  - 不可消除的语义差异 → 记录差异与依据，该负载维持 `unresolved`。
- [ ] 统计 351 与 348 的差额里有多少可由首个差异解释；若还有其他独立的差异类别，逐类列出或另开票据，不得用首个差异推断其余差异。
- [ ] 不修改 B3 固定输入、ADR 0012 排除范围、T 收盘信号 → 下一可用 open 的执行时序，也不修改任何门槛。
- [ ] 更新票据 04、POC-0 票据 09、POC-0 综合报告、STATUS 与 HANDOFF 中的 64×252 S2 状态；结论须注明依据 ADR 0014 判定。
