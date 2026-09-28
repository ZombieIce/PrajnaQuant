# 04: 稳健性负载：64×252 S2/S3

**What to build:** 研究者在 B3 已固定的 64×252 S2/S3 数据集上运行同口径比较，看到 Nautilus 固定开销被摊薄后的比例；最终 B2 结论按预登记的稳健性映射给出。参见 [spec](../spec.md)。

**Blocked by:** 03

**Status:** resolved

**票据 08 归因与重测（2026-09-28）：**首差是 Adapter 逐证券开盘 Quote 的顺序造成的同日卖出后无法买入；[手算缩减用例与证据](08-s2-64x252-parity-diagnosis.md)将其归因于 Adapter，按 ADR 0014 不构成 Fast Event `reject`。修正后两个固定负载 correctness 均通过，S2 订单/Fill 为 351/351，原 133/130 条双向差异全部消失。按原协议收集的[新报告](../../../poc/poc0-benchmark/results/b2-robustness-64x252-parity-diagnosis.json)显示 S2/S3 数值门槛通过，但 reset parity 尚未验证，fallback 测量只属 exploratory；combined decision 保持 `unresolved`。下方描述及旧报告为修正前归档证据，不作为当前结论。

**ADR 0014 修订（2026-09-28）：**本票按 ADR 0013 原映射把 S2 对拍失败判为 `reject`。该负载没有独立金标准，无法归因到哪一方；按 [ADR 0014](../../../docs/decisions/0014-b2-correctness-failure-attribution.md) 改为 `unresolved`（`attribution_required`），归因由 [票据 08](08-s2-64x252-parity-diagnosis.md) 给出。归档报告文件仍记 `reject`，不改写。

- [x] Rust B2 CLI 按 Dataset Version 选择两份 64×252 stress fixture；Rust 账户投影 checksum 与 B3 已登记值、Dataset Content SHA-256 均一致。
- [x] Nautilus Adapter 在报告中分别记录 S2 Top-5/每 5 个 eligible session 调仓和 S3 MA20/60 参数；S3 完成共同子集及 release 测量。
- [x] 正确性门逐字段比较 Nautilus 与 Rust 投影，并注明该负载没有独立手算金标准；不通过的 S2 未进入性能采样。
- [x] HALTED 生命周期项按 ADR 0012 单列并排除；其余订单、成交、现金、持仓、成本、NAV 仍参与 correctness gate。
- [x] 稳健性映射及未验证/失败路径均有测试。S2 出现可复现的共同子集对拍失败，原按 spec 映射为 `reject`，现按 ADR 0014 为 `unresolved`；S3 的数值速度与 RSS 门槛通过，但不抵消 S2 correctness 失败。

2026-09-28 实施完成。复跑命令、全量原始样本和判定见 [稳健性报告](../../../poc/poc0-benchmark/results/b2-robustness-64x252-2026-09-28.json)，构建空间/耗时记录见 [release build record](../../../poc/poc0-benchmark/results/b2-robustness-release-build-final-2026-09-28.json)。两 fixture 的 Dataset Content SHA-256 分别为 `2ae75e889e3d65a28974f3f467794532621b34c6b58c31c6f37bc75392358dff`（S2）和 `d5384e1e27162713df2cd020dcdd655d5838f1d417d3848ac0eaca8f0a002e5a`（S3），与 B3 注册 workload 相同。

S2 的两个独立 Nautilus 对拍均产生相同投影 checksum，且相同六个非排除字段失败（项目订单、Fill、逐日现金/NAV、持仓、账户现金/持仓、成本）；Rust 为 351 个订单/Fill，Nautilus 为 348。报告保留首个日期错位例子：Rust 在 2025-04-08 对 ETF038 下单/成交，Nautilus 于 2025-04-09 才下单/成交；逐日账本首个差异也列出。HALT override 在 2025-07-02 对 ETF002 的 Rust/Nautilus 项目订单与 Fill 数均为零，ADR 0012 的原生生命周期排除字段独立保留，排除它不改变其他失败。S2 性能样本跳过。S3 correctness 通过，Rust/Nautilus 串行中位延迟为 2,455,667 / 53,476,979.5 ns，五组 2-worker Runs/s 中位为 535.84 / 34.21，进程 RSS 为 90,996,736 / 234,848,256 bytes（Nautilus 两 worker 峰值和上界）；S3 数值速度/RSS 门槛通过。整体 robustness 与 combined mapping 在归档报告中为 `reject`，按 ADR 0014 改为 `unresolved`（待票据 08 归因），范围仅限这些固定合成输入、本机与 Nautilus 2.0.0rc5。该结论不验证真实市场数据或生产适用性。独立 review 和最终验证记录见当前 HANDOFF。
