# 10: B3 Rust 与 Python on_bar() 基础对照

**What to build:** 策略开发者可以通过统一入口，在同一事件流和 Rust 账户解释下比较 Rust Native 与 PyO3 Python `on_bar()` 的空回调和 S1 Buy & Hold，看到固定跨语言开销。

**Blocked by:** 07 B2 Fast Event 跑通 Buy & Hold；13 POC-0 构建资源基线与轻量边界。

**Status:** resolved

**Conclusion:** 用户已验收实现；吞吐结论 `unresolved`。

- [x] Rust Native 与 PyO3 收到相同的 29 个 present-bar 事件，S1 按日期、symbol 和目标列表逐事件对拍；同一天错误 symbol 上发出的决策会被拒绝。Python 返回的目标通过同一 Rust 账户 runner 后，逐单、逐日账本、成本和 PortfolioResult 与独立固定 fixture 一致。
- [x] 空回调与实际 S1 回调各记录 1 次预热、5 个原始样本、调用次数、中位数/p95 和端到端耗时；初始化单独记录。
- [x] Python 只返回 S1 目标；账户、订单、成交、成本、估值和组合结果全部由 Rust 解释。
- [x] 固定依赖 `pyo3==0.29.0` 与 Python 3.12.2 可运行。失败的第一次调试报告和过严的浮点断言记录保留；浮点断言改为 `1e-8` fixture 容差后窄测试通过。
- [x] PyO3 构建前记录可用空间并使用 10 GiB 空间闸门与共享 `target/`。初始 warm dev/release 构建耗时 83.14/284.00 秒，target 逻辑字节增量 2,427,198,271/434,471,616；Python 环境占用 162,369,399 字节。最终源码 release warm rebuild 17.86 秒、target 变化 +1,022 字节。无构建触及保留线，未清理 target。

2026-09-28 实现记录：运行 `benchmark-poc0-b3`，只对比空回调及 S1 Buy & Hold。release 原始结果与可复跑方法见 [POC README](../../../poc/poc0-benchmark/README.md#b3-pyo3-per-bar-callback-comparison-ticket-10)；权威样本为修正过严浮点断言并加入事件身份显式检查后的 [review-fix callback JSON](../../../poc/poc0-benchmark/results/b3-pyo3-callbacks-review-fix-2026-09-28.json)。五个回调样本中位数为 Python empty 11.6µs、Python S1 12.7µs、Rust empty 0.042µs、Rust S1 0.209µs；Python/Rust 空回调端到端中位数为 108.0/3.6µs，S1 callback-plus-account 为 109.3/5.7µs。修复前的 [初始 callback JSON](../../../poc/poc0-benchmark/results/b3-pyo3-callbacks-2026-09-28.json) 仅作首次跑批留存，其 Rust S1 中位数（0.292µs）与上述权威样本略有差异，属该微型样本的亚微秒级噪声，不作为当前引用数字。仅是 3 ETF × 10 session 的 29-event 单机微型样本，结论仍 `unresolved`，不用于判断规模拐点、GIL 并行扩展或生产选型。

资源记录：[初始 dev](../../../poc/poc0-benchmark/results/b3-pyo3-build-dev-2026-09-28.json)、[初始 release](../../../poc/poc0-benchmark/results/b3-pyo3-build-release-2026-09-28.json)、[最终 release rebuild](../../../poc/poc0-benchmark/results/b3-pyo3-build-release-final-2026-09-28.json)、[最终 callback test](../../../poc/poc0-benchmark/results/b3-pyo3-test-final-2026-09-28.json)、review-fix 集成测试与 workspace 验证在 `results/*review-fix-2026-09-28.json` 中。初次调试中的 NAV 成功检测误判、exact-float assertion 失败和缺少 `PYO3_PYTHON` 的测试链接失败均修正并保留记录；历史错误记录不代表最终验证失败。
