# 05: 维护成本代理指标

**What to build:** 项目负责人在 B2 比较报告中看到 Fast Event 与 Nautilus Adapter 的维护成本代理指标，与速度/RSS 一起阅读；这些指标只记录，不参与判定。参见 [spec](../spec.md)。

**Blocked by:** 02

**Status:** ready-for-agent

- [ ] 报告新增维护成本节，列出 Fast Event POC 路径与 Nautilus Adapter 各自的非测试代码行数和测试数。
- [ ] 列出新增直接依赖数（Cargo crate / Python 包）与 Python 传递依赖解析数，并注明统计方法与范围。
- [ ] 列出已知的不可消除语义差异数，每项引用出处（例如 ADR 0012 的停牌生命周期）。
- [ ] 判定函数不读取该节；有测试证明改动代理指标不改变判定。
- [ ] 无法统计的项标 `Unknown`，不按零处理。
