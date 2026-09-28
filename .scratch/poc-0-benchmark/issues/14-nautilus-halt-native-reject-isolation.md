# 14: Nautilus 停牌原生拒单隔离实验

**What to build:** 项目负责人能从一个独立小样例的可复跑证据得知：固定版本 Nautilus 2.0.0rc5 在收到 `InstrumentStatus(action=HALT)` 后，对停牌期间提交的市价单是否由撮合引擎原生拒单。据此把 ADR 0012 顶部的 `Unknown` 更新为已证实或已证伪，为 MVP-4 Nautilus Adapter 决定停牌门槛是否仍需由 Adapter 层合成。

**Blocked by:** None (can start immediately)

**Status:** resolved

**Conclusion:** 已证实原生拒单，见[证据报告](../../../poc/poc0-benchmark/results/nautilus-halt-native-probe-2026-09-28.md)；2026-09-28 独立 Standards/Spec review 通过。

- [x] 样例只含单个 instrument、一条 `QuoteTick`、一条 `InstrumentStatus(HALT)` 和一次 `submit_order()` 市价单，使用固定 Nautilus 2.0.0rc5 的公开 `BacktestEngine` 接口。不复用、也不改动 S1/S2/S3 fixture。
- [x] 事件顺序固定为：QuoteTick 可观测 → HALT 生效 → 在停牌期间提交订单。每个事件的时间戳在样例里显式写出，Adapter 的项目层状态门槛不参与；订单直接交给 Nautilus。
- [x] 断言并记录订单的原生结局，从以下几类中选一：触发 `on_order_rejected`（记录拒单原因）/ 被接受但挂单未成交 / 成交 / 其他（例如被拒绝或因引擎报错而未进入订单生命周期）。任何结局都不能靠推断得出，必须以回调或订单状态为证据。
- [x] 对照样例：同一样例去掉 HALT 事件，确认订单能在同一 QuoteTick 上成交，以证明失败是 HALT 造成的，而不是样例本身的配置问题。
- [x] 输出一份小型证据报告：Nautilus 版本、Python 版本、代码 revision、事件序列、订单事件流和结论。复跑命令写入 POC README。
- [x] 按结果更新 ADR 0012：顶部状态与"后续比较边界"首条的 `Unknown` 改为已证实或已证伪，并链接证据。该节已登记的 B2 判定范围（计入的共同子集与排除维度）保持不变。
- [x] 在 STATUS 与 HANDOFF 中记录结论。若证实会原生拒单，写明 MVP-4 可考虑改由 Nautilus 原生状态处理停牌；若证伪，写明 Adapter 层门槛需保留。两种情况都不改变现有 A 股/ETF 的执行时序。
- [x] 不新增构建依赖；只使用已有的 `.venv` 与固定 Nautilus 版本。固定版本不可用时，测试跳过，结论保持 `Unknown`。
