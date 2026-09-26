# 02: B1 SoA 完整运行 Momentum Rotation

**What to build:** 研究者可以选择 Custom SoA 候选，在固定面板上完整运行 S2 Momentum Rotation，并从统一入口看到因子、选中标的、权重、简化组合收益和分段耗时。

**Blocked by:** 01 固定数据集与统一基准入口。

**Status:** ready-for-agent

- [x] 固定动量和波动率窗口、因子方向、排名平局、TopK、调仓日、权重及收益口径，输出可复核的中间结果和最终 checksum。
- [x] 窗口边界、null/NaN、缺日和不足窗口的行为与独立预期一致；T 日信号不读取未来收益。
- [x] 统一入口先给出正确性状态，再保存 release 运行的预热、重复样本、计算时间及规模；单次 smoke 不作为结论。
- [x] 报告明确此处使用权重收益模型，不能将其逐日 NAV 直接等同于事件账本。

- [ ] Add CLI selection for the SoA candidate and report per-stage timings (factor, rank/TopK/weights, return projection) as requested by the ticket summary.
