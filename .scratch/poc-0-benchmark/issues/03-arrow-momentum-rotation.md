# 03: B1 Arrow 完整运行同一策略

**What to build:** 研究者可以切换到 Arrow 列批次候选，对同一固定数据和 S2 参数获得与 SoA 相同的策略结果及同口径性能记录。

**Blocked by:** 02 B1 SoA 完整运行 Momentum Rotation。

**Status:** resolved

- [x] Arrow 候选完成因子、排名、TopK、权重和简化组合收益的端到端计算，而非仅测标量读取。
- [x] 对窗口边界、缺失、平局及收益日期逐项对拍；报告在正确性门槛通过前不记录计时。CLI/单测覆盖首个有效日、独立排名分数、符号升序平局、末日缺 bar 排除、下一日缺 bar 的空收益和手算收益标签。
- [x] 统一报告保留原始重复样本、checksum、计算耗时和 Arrow RecordBatch 转换耗时，测量规模与 SoA 相同。

## Comments

2026-09-27 用户确认验收。release 性能样本仍未生成；性能选型结论保持 Unresolved，后续按 [票据 13](13-build-resource-boundary.md) 的构建资源约定再测量，不补造 release 数值。
