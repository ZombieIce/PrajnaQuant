# ADR 0019：Fast Event L1 以定点账本为权威，并在显式参数下与 Vector 对拍

- 状态：Accepted，2026-10-09（MVP-3 设计，spec #112）；尚未实现

[`ADR 0010`](0010-portfolio-accounting-and-result-boundary.md) 要求 Fast Event 以 Order → Fill → Position → Account 更新账本并逐日核对恒等式，同时要求它与 Vector 在共同语义子集上对拍。MVP-3 确定以下边界：

- **账本数值**：现金、数量、价格与费用使用 Domain 定点十进制（i128，scale 18），乘除结果按 half-even 舍入到 scale 18，暂不做货币单位舍入。"现金＋Σ数量×估值价＝权益"在 Virtual Portfolio 与 Trading Account 两层都要求精确相等。与 f64 金标准（POC-0、Vector、Python）对拍时使用登记的容差。
- **Virtual Portfolio 是子账**：Run 开始时为各 Virtual Portfolio 分配固定资本，余额为 Unallocated Capital。每个 Virtual Portfolio 有独立现金与持仓，订单不跨 Virtual Portfolio 净额化，每个 Fill 只归属一个 Virtual Portfolio。恒等式分三条：账户现金 = Σ Virtual Portfolio 现金 + Unallocated Capital；账户每个标的数量 = Σ Virtual Portfolio 该标的数量；账户权益 = Σ Virtual Portfolio 权益 + Unallocated Capital。MVP-3 中一个 Virtual Portfolio 只运行一个 Strategy。
- **共用执行决策**：Fast Event 与 Vector 共用同一执行决策层（可交易性、[`ADR 0005`](0005-defer-rebalance-on-unavailable-exit.md) 整次延期、pending 替换），Fast Event 只在其后生成订单并记账。由于共享逻辑无法自证，对拍须有独立 Python 金标准作第三方裁判。
- **口径差异显式化**：数量规则 `sizing`（`vector_parity` / `lot`）、调仓规则 `rebalance`（`full_target` / `entry_only`）与未成交买腿策略 `unfilled_entry`（`skip` / `retry`，默认 `skip`）都是 Run Spec 参数。`vector_parity` + `full_target` 精确复刻 [`ADR 0017`](0017-vector-engine-semantics.md) 的成本与权重口径：成本以 open 前权益 × 权重换手为基数，open 后持仓等于目标权重 × 扣费后权益，Fill 价为 raw open，滑点作为费用单列。这是与 Vector 对拍的唯一模式。`lot` 引入整手、最低佣金与含滑点的成交价。`entry_only` 只在 Fast Event 中实现，仅用于复现 POC-0 金标准，不作为通用调仓语义。

## Considered Options

- 账本用 f64 并以容差检查恒等式：只能证明近似守恒，无法区分舍入误差与记账错误；被拒绝。
- 对拍时 Fast Event 按自然账本方式计算数量，并登记一个约 1e-6 的容差：差异来自成本二阶项，容差会掩盖真实口径漂移；被拒绝。
- Vector 模拟整手与最低佣金：ADR 0017 已拒绝；会让 Vector 承担账本职责。
- Fast Event 独立实现执行决策：两套延期/可交易性逻辑会产生需要额外解释的差异，"共同语义"失去保证。
- 只保留 `full_target`，POC-0 S2 金标准降级为参考：会丢失 B2 已对拍过的逐笔证据。改为以 `entry_only` 复现，并限定其用途。
- 订单在账户层跨 Virtual Portfolio 净额化：更贴近实盘，但会使 Fill 归属与费用分摊需要额外规则；留待后续阶段。

## Consequences

`fast_event@1` 的语义由本 ADR 与 spec #112 固定，任何改变数量、调仓、估值或费用口径的修改都必须提升 Engine 语义版本。Vector 增加 `unfilled_entry` 与 `BuyAndHold`、`MaCrossover` 策略形态后，默认值下的现有 S2 Run Spec 身份不得改变，否则须提升 `vector@1`。T+1、涨跌停、货币单位舍入与跨 Virtual Portfolio 净额化属于后续阶段；在实现前，Fast Event 结果须声明这些规则未建模。
