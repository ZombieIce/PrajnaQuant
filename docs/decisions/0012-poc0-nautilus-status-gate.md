# ADR 0012：POC-0 Nautilus Adapter 的执行状态门槛

- 状态：Accepted for the fixed POC-0 S1 fixture only，2026-09-27；跨引擎原生订单生命周期仍 Unresolved

POC-0 的 Rust Fast Event 参考路径在有价格 bar 但执行状态为 `HALTED` 时记录零数量拒单，并在下一可交易日重试。Nautilus 候选使用合成开盘/收盘 QuoteTick；仅省略停牌日行情会让既有订单保持待成交，无法代表 Rust 的拒单语义。

## 决定与事件序列

1. T 日 15:00 收盘行情可观测后，S1 形成目标；不得在该收盘价成交。
2. 下一交易日 08:50 已可用的证券执行状态进入 Adapter。这里的 `available_at` 是 fixture 中显式给出的历史可用时刻；本地 `observed_at` 不能替代它。
3. 09:30 合成开盘 QuoteTick 到达时，Adapter 先检查该证券当日状态。不可交易则记录项目层零数量拒单，不向 Nautilus 提交订单；可交易才创建 Nautilus 市价单。
4. 未成交目标继续待执行；后续交易日开盘重新检查当日已可用状态，并在可交易时提交新订单。收盘估值使用已有可观测价格；停牌或缺行情不产生零价成交。

固定样本可手算：2026-01-12 收盘形成 A/B/C 各 300 股目标；2026-01-13 08:50 的 B 状态为 `HALTED`。当天 09:30 A、C 分别以 100.1 CNY 成交 300 股、各付 100 CNY 手续费，B 只记 `quantity=0, reason=HALTED`；现金为 `100000 - 2 × (300 × 100.1 + 100) = 39740` CNY。2026-01-14 B 恢复可交易，09:30 新订单成交 300 股后现金为 `39740 - (300 × 100.1 + 100) = 9610` CNY。

## 比较边界

该拒单由项目 Adapter 状态门槛产生，不是 Nautilus 撮合引擎的原生拒单。固定样本中 Adapter 项目事件为 4 条、Nautilus 实际提交为 3 条、Rust 参考尝试为 4 条。报告必须分别列出项目契约一致性和 Nautilus 原生生命周期差异；不得把 Adapter 合成的拒单计入 Nautilus 原生一致性。Fill、账户、成本与 NAV 可以在固定共同子集对拍；原生订单生命周期及跨引擎吞吐结论保持 `unresolved`。本 ADR 不改变现有 A 股/ETF 执行时序，也不确立生产 Accurate Backend 规则。
