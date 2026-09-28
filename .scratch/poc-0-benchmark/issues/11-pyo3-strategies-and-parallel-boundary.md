# 11: B3 真实策略与并行边界

**What to build:** 策略开发者可以对比 S2、S3 在 Rust Native、Python 逐 bar 回调及可批处理候选下的正确性和吞吐，确定 Python 接口在什么负载下成为瓶颈。

**Blocked by:** 09 B2 Momentum Rotation、MA20/60 与吞吐结论；10 B3 Rust 与 Python on_bar() 基础对照。

**Status:** resolved — user accepted the implementation and evidence; performance conclusion: reject for the registered target workload

2026-09-28 启动说明：用户已验收票据 10 的实现范围；其 S1 微型样本性能结论仍 unresolved，作为票据 11 的基线限制，不外推为规模/GIL 结论。票据 09 的 S2/S3 正确性基线已通过；其 Fast Event/Nautilus 吞吐比较因口径不一致仍 unresolved，不把该跨引擎数字作为本票 Rust/Python 测量输入。本票直接在 Rust Native 与 Python 间使用相同 release、输入、测量边界和并行度建立新对比；保留票据 09 的原状态与判定范围。

**预登记测量协议（release 样本执行前）：**目标负载固定为 64 instruments × 252 sessions；S2 使用 momentum 20/60、volatility 20、Top-5、每 5 个 eligible session 调仓，S3 使用 MA20/60。两策略都跑同一 Rust Native / Python per-bar / Python batch 候选；单 Run 1 次预热、5 次样本；2 个 worker 跑 6 个独立 Run、并行组重复 5 次。逐事件 Python 仅在两策略均满足“并行组 callback 中位延迟 ≤ Rust Native 的 2 倍，且包含 Rust 账户回放的 Runs/s ≥ Rust Native 的 80%”时判定适用于该目标负载。逐 bar 未过而 batch 两策略均过则 `defer`；两种 Python 方式均未同时通过两策略则 `reject` 该目标负载。正确性为硬门槛。峰值 RSS 报告为整个 benchmark 进程高水位；嵌入式 Python 与候选共用进程，无法归因到单候选，因此列为限制，不用作胜负门槛。结论只适用于此合成负载与本机，不外推普遍规模拐点。

- [x] S2 和 S3 的 Rust/Python 信号、决策、账本和 PortfolioResult 对拍；Python batch 候选产生相同结果。固定小样本通过独立金标准，放大样本按完整决策序列与 Rust 账本 checksum 对拍。
- [x] 单 Run 与多个独立 Run 使用固定输入、预热、重复次数、线程数；报告原始样本、中位数、p95、Runs/s 和进程峰值 RSS。
- [x] 报告分别列出空回调边界、策略回调、初始化、Rust 账户回放和端到端耗时；说明 Python per-bar 的 GIL 争用及 batch 每 Run 一次 GIL 进入方式。
- [x] Release 计时前登记目标负载和判据。对 64×252 target，逐 bar 和 batch Python 均未达到两策略的预登记延迟/吞吐门槛，结论为 `reject` 该目标负载。
- [x] Rust/Python 使用相同 release、输入和两 worker 条件；Rust release 构建/target 变化及 Python 版本/环境占用单列。峰值 RSS 报为共享进程高水位，明确不作候选内存胜负比较。

2026-09-28 release 结果：[原始报告](../../../poc/poc0-benchmark/results/b3-pyo3-strategies-2026-09-28.json)。两策略固定 golden 与 Python per-bar/batch 的信号、账户均通过。目标负载均为 64 instruments × 252 sessions、约 16.1k bar events；S2 为 Momentum 20/60 + volatility 20 + Top-5 + 每 5 eligible sessions 调仓，S3 为 MA20/60。

| 策略 | Rust Native Runs/s | Python per-bar Runs/s | Python batch Runs/s |
| --- | ---: | ---: | ---: |
| S2 Momentum Rotation | 126.04 | 6.67 | 14.48 |
| S3 MA20/60 | 472.64 | 11.58 | 144.38 |

双 worker、6 Run 并行组；Rust账户回放包含在 Runs/s 中。Python batch 虽改善 S3 吞吐，但低于预登记的 Rust 80% 目标；两种 Python 候选也都超过 2× callback 中位延迟门槛。空回调在约 16.1k events 上测得 Python-Rust callback 中位差约 6.8–6.9 ms；两 worker 下 Python per-bar 空回调中位耗时扩大约 24×，体现本机该负载的 GIL/线程争用。结论只覆盖此合成负载和机器，不是通用 Python/GIL 结论。

全 benchmark 进程峰值 RSS 为 91,078,656 bytes，含内嵌 Python 与所有依次测量候选，不能归因到单候选。Python 3.12.2 环境占用 162,369,399 bytes；PyO3 0.29.0。最终 warm release feature 构建 19.51 s，target 逻辑大小变化 -960 bytes（共享缓存变化，不表示清理或负构建产物）。资源证据见 [release build](../../../poc/poc0-benchmark/results/b3-strategies-release-build-final-2026-09-28.json)。

验证：B3 策略 golden 测试 1/1、B3 callback 集成测试 1/1、workspace 测试 16 + 65 + 11 passed / 1 ignored、B3 feature Clippy 与 workspace Clippy、`cargo fmt --all -- --check`、Python recorder `py_compile` 通过。用户表示本任务不需要 independent review，因此未进行独立 review；未提交。

2026-09-28 验收：用户确认验收 ticket 11。验收范围是实现、正确性与登记负载测量证据；`reject` 表示 Python 候选未达到该合成目标负载的预登记性能门槛，不代表正确性失败或通用技术选型。票据 09 跨引擎吞吐结论和票据 10 微型样本规模结论仍各自保持 unresolved。
