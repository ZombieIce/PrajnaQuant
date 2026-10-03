# ADR 0017：Vector Engine 在权重层复现调仓延期并采用 Session 窗口与比例成本

- 状态：Accepted，2026-10-02；M11 决策阶段已实现，执行与收益计算尚未实现

Vector Engine 没有订单与现金账本，但 MVP-3 需要它与 Fast Event 在共同语义上对拍。MVP-1 决定：因子窗口按 Venue Session 计数，窗口内任一所需 bar 缺失则该值为 `missing_input`，不跨缺口取更早的 bar；T 日收盘信号在下一可用 Session 的 open 执行；可交易性只取自 D7 `execution_status`，无记录即 UNKNOWN 且不可交易；需卖出的旧持仓不可交易时，按 [`ADR 0005`](0005-defer-rebalance-on-unavailable-exit.md) 在权重层延期整次调仓、沿用上一期权重并记录延期轨迹；成本为换手 × (commission_rate + slippage_bps)，不模拟最低佣金与整手；排序平局按 `instrument_id` 升序。调仓决策日从首个至少一个标的综合分数为 `ok` 且其 `available_at` 不晚于该 Session 收盘的 Session 起算，此后每 k 个 Venue Session 一次；已知但收盘后才可用的分数不得进入当次排名。最后 Session 的目标无执行日，保持待执行。持仓标的在估值 Session 缺 open 时沿用最近可得 open、当期收益记 0 并标注估值延续，下一个 open 出现时一次计入跨缺口收益；缺价不视为零价。

## Considered Options

- 沿用旧 [`strategy.rs`](../../crates/quant-research/src/strategy.rs) 与 POC B1 按标的已观测 close 计数并跳过缺失 bar：缺 bar 会静默拉长窗口的日历含义，被拒绝。结果与旧 ETF 回测及 POC 期望值不同，需在对比时披露。
- Vector 专用“仅冻结被阻塞标的”语义：更简单，但与 Fast Event 产生需额外解释的系统性差异。
- 引入名义资金以模拟最低佣金/整手：使 Vector 承担账本职责，与 [`ARCHITECTURE.md`](../../ARCHITECTURE.md) 的分层冲突。

## Consequences

Vector 结果须列出简化假设（比例成本、无整手与最低佣金、raw open-to-open 价格口径、Static Universe 非 PIT），且不得声称通过现金/持仓守恒验证。与 Fast Event 的对拍只在上述共同语义子集上进行。
