# 08: 64×252 S2 跨候选差异诊断

**What to build:** 项目负责人能从一个可人工验算的最小用例得知：64×252 S2 上 Rust 与 Nautilus 的订单和成交差异（Rust 351 / Nautilus 348；首个差异为 ETF038，Rust 2025-04-08 成交，Nautilus 2025-04-09 成交）究竟属于 Fast Event 错误、Adapter 错误，还是不可消除的语义差异；并按 [ADR 0014](../../../docs/decisions/0014-b2-correctness-failure-attribution.md) 给出该负载的归因结论。参见 [spec](../spec.md)。

**Blocked by:** None (can start immediately)

**Status:** resolved

- [x] 从固定的 `poc0.b3.s2-scale-64x252.v1` 数据中取出首个差异附近的事件序列，按项目时间模型逐步写出：T 日收盘可观测 → 形成信号 → 下一可用日 08:50 状态可用 → 09:30 open 执行。覆盖 ETF038 在 2025-04-07 至 04-09 的 bar 是否存在、执行状态、日历、UNKNOWN/HALTED/缺 bar 规则，以及当日持仓与现金约束。
- [x] 把首个差异缩成一个手算、可人工验算的小 fixture：只保留必要的 instrument 和交易日，期望值独立写出，不由任何被测 Engine 生成。Rust Fast Event 与 Nautilus Adapter 分别运行该 fixture，并与期望值逐字段对拍。
- [x] 结论只能取以下三种之一，每种都附证据：
  - Fast Event 违反独立期望 → 按 ADR 0014 判 `reject`，写明违反的规则；
  - Adapter 违反独立期望 → 修正 Adapter，新增回归测试，并按原协议重跑票据 04 的 S2 稳健性负载（正确性门以及通过后的性能采集）；
  - 不可消除的语义差异 → 记录差异与依据，该负载维持 `unresolved`。
- [x] 统计 351 与 348 的差额里有多少可由首个差异解释；若还有其他独立的差异类别，逐类列出或另开票据，不得用首个差异推断其余差异。
- [x] 不修改 B3 固定输入、ADR 0012 排除范围、T 收盘信号 → 下一可用 open 的执行时序，也不修改任何门槛。
- [x] 更新票据 04、POC-0 票据 09、POC-0 综合报告、STATUS 与 HANDOFF 中的 64×252 S2 状态；结论须注明依据 ADR 0014 判定。

## 诊断与独立期望（2026-09-28）

**结论：Adapter 错误，非 Fast Event `reject`。**固定输入未修改：Dataset Content SHA-256 `2ae75e889e3d65a28974f3f467794532621b34c6b58c31c6f37bc75392358dff`，fixture 文件原始字节 SHA-256 `5e806babd29091858c63c5a04f7e1165d61599d03f26726f5132a587caac9593`。日历含 2025-04-07/08/09（index 65/66/67）；ETF038 三日 close 为 102.7/104/95.2，每日均有 bar、没有 missing-bar 或状态 override。默认状态有来源 `poc0-fixture-default`，08:50 可用，`TRADABLE`；09:30 open 为 100。04-07 15:00 使用截至当日 close 的 20/60 动量与波动窗口，Top-5 信号含 ETF038；04-08 09:30 才可执行。若状态 `UNKNOWN`/`HALTED`/无来源或缺 open，则当日零数量拒单或缺 open 记录，继续等下一可用开盘，不以零价成交；本三日均不触发。04-09 的 ETF038 bar 存在，但不应成为该信号的首次买入日。

最小诊断 fixture 在 Python 与 Rust 测试中从上述固定数据裁取：只保留 04-08 开盘前实际持有的 ETF003/007/012/025/054 和当日目标 ETF038/041/046/051/055，以及从头到 04-09 的 68 个 session（含 60 日指标窗口）；状态/缺 bar 同步按符号和日期裁取。独立的手算期望：初始现金 100000；此前五只旧仓各 100 股，04-08 开盘先以 99.9 卖五笔，最低佣金每笔 100，得到现金 `49450 + 5 * (100 * 99.9 - 100) = 98900`；再以 100.1 买入五只新标的各 100 股，佣金每笔 100，现金 `98900 - 5 * (100 * 100.1 + 100) = 48350`。ETF038 订单/Fill 是 `decision=04-07, execution=04-08, BUY 100, fill_price=100.1, commission=100`。五个目标 04-08 收盘价分别为 104、103.7、104.8、95.8、103.8；`NAV = 48350 + 100 * (104 + 103.7 + 104.8 + 95.8 + 103.8) = 99560`。Rust 与修正后 Nautilus 的缩减用例分别通过订单、Fill 数、现金、持仓数、NAV 断言；完整固定负载的逐字段 correctness gate 亦通过。成交成本仍按原费率/滑点，未以信号日 close 成交。

Adapter 原先让每只标的的 open Quote 按反向符号顺序相隔纳秒到达；ETF038 的 tick 早于旧持仓的 SELL tick，因此当时仓位约束挡住 BUY，卖出完成后当天已没有该标的 Quote。修正为同一 09:30 开盘批次再给 S2 一轮相同开盘报价，让卖出后买入；状态阻断在同日只记一次。同步把同批次订单的字段比较改为多重集（保留重复次数），避免把跨证券排列当作经济差异。原归档两次对拍中订单/Fill 各有 Rust-only 133、Nautilus-only 130（净差 3），修复后两侧均 351，字段、逐日现金/持仓/NAV 与成本全部相等；无剩余独立差异类别。ADR 0012 排除的原生订单生命周期仍单列，不据此判定。

[修正后完整原协议报告](../../../poc/poc0-benchmark/results/b2-robustness-64x252-parity-diagnosis.json)包含两次独立正确性对拍、20 个串行样本、五组交替 2-worker × 6 Run 与 worker RSS：S2 中位延迟 Rust/Nautilus 19,105,375 / 260,615,500.5 ns，并行中位 61.89 / 6.70 Runs/s，峰值 RSS 90,685,440 / 302,743,552 bytes（后者为 worker 峰值和上界）。S3 也通过。两负载数值门槛均通过；但 `cached_conversion_new_engine` 的 reset parity 尚未验证，报告将其标为 exploratory，combined decision **仍为 `unresolved`**，不是正式 `adopt`。按 ADR 0014，此负载归因为 Adapter 错误，修正后 correctness 通过，不满足 Fast Event `reject` 条件。旧归档报告不改写。
