# 12: POC-0 综合结论

**What to build:** 项目负责人可以从一份汇总报告追溯 B1、B2、B3 的固定输入、正确性、原始测量和环境条件，并据此决定后续 MVP 的技术选择。

**Blocked by:** 06 B1 参数扫描、缓存与布局结论；09 B2 Momentum Rotation、MA20/60 与吞吐结论；11 B3 真实策略与并行边界；13 POC-0 构建资源基线与轻量边界。

**Status:** resolved

- [x] 每项结论均标为 `adopt / defer / reject / unresolved`，说明登记门槛、速度、内存、转换/维护成本和语义差异。
- [x] 从[综合报告](../../../poc/poc0-benchmark/POC-0-SYNTHESIS.md)追溯复跑命令、Dataset Version/hash、参数、成本、代码/依赖版本、原始样本和 checksum；每个比较注明 provenance 的已知限制。
- [x] 区分已实现、合成正确性、性能测量与真实市场验证；未测量处保留 Unknown/Unresolved。
- [x] 记录剩余不确定性与后续决策证据；声明本 POC 不等于生产 Engine 或历史 ETF 无偏业绩验收。
- [x] 各候选的 warm 构建耗时、target 变化、缓存/依赖身份、资源限制与运行时吞吐分开报告；cold build Unknown 未解释为零或应用冷启动。

## Answer

POC-0 综合报告已完成：[POC-0-SYNTHESIS.md](../../../poc/poc0-benchmark/POC-0-SYNTHESIS.md)。B1 Custom SoA 对登记目标负载结论为 `defer`；B1 Parquet 读取/受限内存裁剪路径可用，但布局性能仍 unresolved。B2 共同子集正确性通过，Fast Event/Nautilus 性能选型因测量边界不同而 unresolved；停牌原生订单生命周期仍按 ADR 0012 unresolved。B3 Python per-bar 与 batch 对 64×252 登记负载为 `reject`，其他规模仍 unresolved。构建成本独立于运行时比较；冷构建与无法归因项保留 Unknown。

该综合结论不批准生产 Engine 选型，不证明真实市场 PIT、ETF 总回报或可实盘性。唯一建议下一步：为 B2 统一输入准备、初始化、计时和并行/RSS 口径后复测，再判断是否达到预登记门槛。

独立 review 起初提出两项 P2 证据呈现缺口：综合报告应直接列出 Parquet 子进程/cgroup RSS，以及 B2 双方 RSS 数值并澄清 Adapter 维护成本未量化。已补入相应数值、测量范围与 `Unknown` 标记；review 未发现其余数值或链接问题。
