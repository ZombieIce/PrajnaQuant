# 09: B2 Momentum Rotation、MA20/60 与吞吐结论

**What to build:** 研究者可以在 Fast Event 和 Nautilus 候选中运行 S2 Momentum Rotation 与 S3 MA20/60，核对共同交易语义，并判断自研 Fast Event 的实际收益。

**Blocked by:** 02 B1 SoA 完整运行 Momentum Rotation；08 B2 Nautilus 跑通同一场景。

**Status:** ready-for-agent

- [ ] 两种候选接收相同事件和策略参数；S2 的因子、排名、TopK、调仓目标与已固定语义一致，S3 的 MA20/60 窗口和状态转换有独立预期。
- [ ] 在共同子集逐项对拍信号、订单、Fill、现金、持仓、成本、每日 NAV 和 PortfolioResult；语义差异单独列明。
- [ ] 固定规模、预热、重复次数和线程数，报告单 Run 延迟、并行 Runs/s、中位数、p95、峰值 RSS、转换成本和原始样本。
- [ ] 预先登记收益门槛，给出 `adopt / defer / reject / unresolved` 结论；不可比时不以不同 Sharpe 判断实现优劣。
