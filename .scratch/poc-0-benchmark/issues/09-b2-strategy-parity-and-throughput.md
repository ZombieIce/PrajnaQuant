# 09: B2 Momentum Rotation、MA20/60 与吞吐结论

**What to build:** 研究者可以在 Fast Event 和 Nautilus 候选中运行 S2 Momentum Rotation 与 S3 MA20/60，核对共同交易语义，并判断自研 Fast Event 的实际收益。

**Blocked by:** 02 B1 SoA 完整运行 Momentum Rotation；08 B2 Nautilus 跑通同一场景。

**Status:** unresolved

**Note:** 停牌/执行状态导致的 Nautilus 原生订单生命周期一致性已在 [ADR 0012](../../../docs/decisions/0012-poc0-nautilus-status-gate.md)「后续比较边界（供票据 09 及以后引用，无需重新论证）」一节划定为永久排除维度，不计入本票的 `adopt / defer / reject` 判据；触碰停牌的场景沿用票据 08 的"Adapter 项目事件 vs Nautilus 原生提交"分列方式即可，无需重新论证或重新验证该边界。

- [x] 两种候选接收相同事件和策略参数；S2 的因子、排名、TopK、调仓目标与已固定语义一致，S3 的 MA20/60 窗口和状态转换有独立预期。
- [x] 在共同子集逐项对拍信号、订单、Fill、现金、持仓、成本、每日 NAV 和 PortfolioResult；语义差异单独列明。
- [x] 固定规模、预热、重复次数和线程数，报告单 Run 延迟、并行 Runs/s、中位数、p95、峰值 RSS、转换成本和原始样本。
- [x] 预先登记收益门槛，给出 `adopt / defer / reject / unresolved` 结论；不可比时不以不同 Sharpe 判断实现优劣。
- [ ] Fast Event 与 Nautilus 的构建耗时/空间单列工程成本；events/s 和 runs/s 只比较共同 release 配置与已对齐语义，资源门槛阻断的候选保留 `unresolved`。

2026-09-27 证据：[复跑说明](../../../poc/poc0-benchmark/README.md#b2-s2s3-registered-decision-protocol-ticket-09)、[原始对拍](../../../poc/poc0-benchmark/results/b2-09-comparison-2026-09-27.json)、[Rust S2](../../../poc/poc0-benchmark/results/b2-09-s2-throughput.json) / [S3](../../../poc/poc0-benchmark/results/b2-09-s3-throughput.json) 两线程样本、[warm-cache release 构建](../../../poc/poc0-benchmark/results/b2-09-release-build-2026-09-27.json)。独立 MA20/60 金标准、完整窗口前不下单、交叉上/下后的次日 open、S2 的 `UNKNOWN`/`HALTED`、逐日账户恒等式均有固定预期与测试。两候选的 S2/S3 共同子集全部通过；S2 停牌时项目事件与 Nautilus 原生提交计数不同，按 ADR 0012 排除在本次判定外。预登记门槛为两策略的 Fast Event 中位单 Run 延迟及并行 Runs/s 均至少 2 倍优于 Nautilus，峰值 RSS 不高于 Nautilus。Rust 两线程复用已准备 bars、Nautilus 两个 warm 进程每 Run 转换/初始化，Rust RSS 为含预检的全进程、Nautilus 为单 worker；因此吞吐/RSS 口径不可比，不套用门槛，结论 `unresolved`。本结论基于上述共同子集成立；停牌场景下的原生订单生命周期语义仍 `unresolved`，不构成本次结论的一部分。

独立 review 发现 S3 预期只在测试中，已补版本化 golden 并对拍；关于缺少 Nautilus S2/S3 或阈值的发现经报告与 README 核查不成立。剩余问题：冷构建未知、Python 传递依赖未完整锁定、目标负载较小；当前机器的 Python 仓库测试因缺少兼容的 `duckdb` 包无法加载 `test_warehouse`。**唯一建议下一步：**统一双方单 Run 的数据转换、策略计算、引擎初始化及 worker/RSS 口径，再在 release 下重新运行有资格判定的双策略基准。
