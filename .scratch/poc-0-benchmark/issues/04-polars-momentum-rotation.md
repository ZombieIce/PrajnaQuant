# 04: B1 Polars 完整运行同一策略

**What to build:** 研究者可以切换到 Polars 表达式候选，对同一固定数据和 S2 参数获得与 SoA 相同的策略结果及同口径性能记录。

**Blocked by:** 02 B1 SoA 完整运行 Momentum Rotation。

**Status:** ready-for-agent

- [ ] Polars 候选以表达式完成因子、排名、TopK、权重和简化组合收益的端到端计算，而非仅测标量读取。
- [ ] 对窗口边界、缺失、平局及收益日期逐项对拍；结果与独立预期一致后才纳入性能比较。
- [ ] 统一报告保留原始重复样本、checksum、计算耗时和必要的转换耗时，测量规模与 SoA 相同。
