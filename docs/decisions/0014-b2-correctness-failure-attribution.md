# ADR 0014：B2 正确性失败须先归因再判 `reject`

- 状态：Accepted，2026-09-28
- 修订对象：[ADR 0013](0013-poc0-b2-matched-remeasure.md)「预登记门槛与判定映射」中的 `reject` 一条；速度、吞吐、RSS 门槛与其余映射不变。

## 背景与披露

ADR 0013 规定：ADR 0012 排除范围以外若有可复现的共同子集 correctness 失败，即判 `reject`。票据 04 在 64×252 S2 上观察到可复现的跨候选差异（Rust 351 / Nautilus 348 个订单与 Fill；首个差异为 ETF038，Rust 在 2025-04-08 成交，Nautilus 在 2025-04-09 成交），并按这一条把 B2 判为 `reject`。

该负载没有独立手算金标准。Rust 投影与 B3 stress checksum 一致，只说明 Rust 与它自己早先的输出一致。因此这次失败无法说明是 Fast Event 错、Adapter 错，还是两边存在不可消除的语义差异。B2 的 `reject` 指"不采用自研 Fast Event"，而 Adapter 一侧的错误不能作为不采用 Fast Event 的理由。POC-0 spec 也要求先把无法对齐的语义单独列出，不得凭跨引擎差异断定某个引擎有 Bug。

**本 ADR 在看到票据 04 的结果之后制定。**按 ADR 0013「测量前保存规则」，受影响的结论须标注为未按原预登记映射判定。本修订只涉及失败的归因要求，不移动任何性能门槛。

## 决定

1. 自动化 correctness gate 发现跨候选差异（负载无独立金标准，或 Nautilus 投影偏离已由独立金标准验证过的 Rust 投影）时，判定为 `unresolved`，并标记 `attribution_required`。该负载不采集性能样本。
2. 只有诊断证据显示 Fast Event 违反独立期望时，才能判 `reject`。独立期望指手算、可人工验算的小用例或独立金标准，而且期望值不能由被测 Engine 生成。这一结论由诊断票据给出，自动化 gate 不会直接输出 `reject`。
3. 诊断若认定错在 Adapter，修正后按原协议重跑受影响负载；若认定属于不可消除的语义差异，则记录差异和依据，该负载维持 `unresolved`。两种情况都不构成 `reject`。
4. 各门槛数值、`adopt` / `defer` 映射、ADR 0012 的排除范围和 ADR 0013 的其余内容不变。

## 受影响结论

- 票据 04 的归档报告 `b2-robustness-64x252-2026-09-28.json` 按 ADR 0013 原映射记为 `reject`。按本 ADR，改为 `unresolved`（`attribution_required`），等待诊断票据给出结论。归档文件不改写。
- POC-0 票据 09 与综合报告中的 B2 结论恢复为 `unresolved`。
