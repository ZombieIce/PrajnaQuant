# 04: B1 Polars 完整运行同一策略

**What to build:** 研究者可以切换到 Polars 表达式候选，对同一固定数据和 S2 参数获得与 SoA 相同的策略结果及同口径性能记录。

**Blocked by:** 02 B1 SoA 完整运行 Momentum Rotation。

**Status:** resolved

- [x] Polars 候选以表达式从观测收盘序列计算动量、样本波动率和复合评分，并完成截面排序、TopK、等权权重和简化组合收益。
- [x] 对窗口起点、缺失 bar、符号零平局及收益日期与 SoA 对拍；加入独立手算窗口/收益断言，golden 或独立输入未通过时跳过计时。
- [x] 统一报告保留原始重复样本、checksum、因子/排序权重/收益分阶段耗时和 DataFrame 转换耗时；重复数与 SoA 相同。

## Comments

2026-09-27 用户确认验收。Polars 等权分母按当日实际入选数量计算，并有 top_n 大于可选标的数的覆盖用例；全工作区 89 项通过、1 项按条件忽略，Clippy、格式、release CLI correctness 检查通过。固定 3×10 fixture 不支持布局/性能决策；Parquet 扫描、内存和 out-of-core 对比仍由后续票据覆盖。
