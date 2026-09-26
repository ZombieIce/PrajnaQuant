# 07: B2 Fast Event 跑通 Buy & Hold

**What to build:** 研究者可以用统一入口让最小 Fast Event 原型运行 S1 Buy & Hold，并检查单 Venue、长仓、市价单、L1 成交和固定成本下的完整交易账本。

**Blocked by:** 01 固定数据集与统一基准入口。

**Status:** ready-for-agent

- [x] T 收盘产生的决策在下一可用 open 执行；缺 bar、不可成交状态和末日未执行目标按独立预期处理。
- [x] 订单、Fill、数量、成交价、佣金、滑点、现金、持仓及逐日 NAV 可追溯，且 `NAV ≈ cash + Σ(quantity × 估值价)`。
- [x] 同一输入可以重复生成相同账本和 checksum；正确性通过后才保存独立的初始化、事件处理与端到端计时。
- [x] 报告限定原型的市场与撮合语义，不把现有 ETF 回测直接称为完整 Fast Event Engine。

正确性范围说明：基准 fixture 覆盖 B2 的下一可用 open、B 停牌拒单及重试、B 末日缺 bar 的沿用估值和账本恒等式。另有 CLI 变体移除 A 的所有后续 bars 并启用非零买入税，独立断言末日 `no_future_execution_session` 待执行目标和现金非负；由于修改了固定输入，原 S2 金标准预期失败是有意的，B2 自身检查仍需通过。

## Comments

2026-09-27：已在统一 `benchmark-poc0` 报告加入 `b2_fast_event_buy_hold`；JSON 包含账本 checksum 和分列计时。全量 workspace 测试与 CLI golden/变体检查通过，具体验证记录见 `HANDOFF.md`。
