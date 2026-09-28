# 04: 稳健性负载：64×252 S2/S3

**What to build:** 研究者在 B3 已固定的 64×252 S2/S3 数据集上运行同口径比较，看到 Nautilus 固定开销被摊薄后的比例；最终 B2 结论按预登记的稳健性映射给出。参见 [spec](../spec.md)。

**Blocked by:** 03

**Status:** ready-for-agent

- [ ] Rust B2 CLI 可按 Dataset Version 选择 `poc0.b3.s2-scale-64x252.v1` 与 `poc0.b3.s3-scale-64x252.v1`；账户投影与 B3 已登记的 stress checksum 一致。
- [ ] Nautilus Adapter 可运行 64 instrument 的 S2（Top-5，每 5 个 eligible session 调仓）与 S3；参数在报告中与判定负载分开列出。
- [ ] 该负载的正确性门是"Nautilus 共同子集投影 = Rust Fast Event 投影"；报告注明该负载没有独立手算金标准。
- [ ] 若数据集含停牌状态，按 ADR 0012 分列"Adapter 项目事件"与"Nautilus 原生提交"，不拖累其余字段。
- [ ] 判定函数应用稳健性映射：判定负载达标而稳健性未达标 → `defer`；稳健性负载因资源或适配原因未运行 → 保留判定负载结论并标"稳健性未验证"；两条路径都有单元测试。
