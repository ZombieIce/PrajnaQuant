# 02: 贯通 S2 3×10 同口径比较

**What to build:** 研究者用一条命令，在 S2 Momentum Rotation 3×10 golden 上跑通 Fast Event 与 Nautilus 的同口径比较，得到一份含正确性门、原始样本、RSS 归因和登记判定的比较报告。报告目前只覆盖 S2。参见 [spec](../spec.md)。

**Blocked by:** None (can start immediately)

**Status:** completed (S2 matched fallback scope)

- [x] 编排入口以独立子进程完成 golden 预检；预检失败时不采集该负载的性能，报告标 `unresolved` 并附原因。
- [x] Rust B2 CLI 新增串行/并行模式、worker 与 Run 数、跳过进程内预检等输入；S2 3×10 checksum 与独立 golden 一致。
- [x] Nautilus 不复用尚未验证 reset parity 的引擎；采用预登记 fallback：每 worker 一次转换并缓存 QuoteTicks，每 Run 新建引擎/策略。两次预热和每 Run projection checksum 均对照独立通过的 Nautilus golden。reset→new-engine parity 仍是后续范围，不影响本票据的 fallback 对照。
- [x] 双方计时符合 fallback 主边界，包含每 Run 策略/因子/信号、账户推进、引擎初始化和 projection；转换、warmup、序列化/checksum 与进程/线程池初始化排除。采集 20 个串行样本和 5 组交替顺序的 2-worker × 6 Run 并行墙钟样本。
- [x] RSS 分离采集报告 Rust 单测量进程峰值、Nautilus 每 worker 峰值和标注为上界的合计；golden preflight 与测量进程分离。
- [x] 判定纯函数测试覆盖 `adopt`、速度/RSS 未达标 `defer`、正确性失败 `reject`、缺测/协议不匹配 `unresolved`、恰好 2×、RSS 相等，以及 ADR 0012 排除维度失败但其他字段通过。
- [x] 固定 Nautilus 版本不可用时不采集性能，测试按环境跳过，报告保留 unresolved 状态；不断言绝对耗时。

实现现状：Rust 与 Nautilus S2 golden correctness gate 通过（ADR 0012 的 native lifecycle 项排除）；S2 fallback matched release 报告见 [结果](../../../poc/poc0-benchmark/results/b2-matched-s2-2026-09-28.json)。串行 20 样本中位数为 Rust 11,042 ns / Nautilus 842,146 ns；五组 2-worker × 6 Run 并行吞吐中位数为 56,338 / 1,225 Runs/s；测量 RSS 为 11,255,808 / 147,046,400 bytes（Nautilus worker 峰值和，上界）。S2 本负载的登记门槛判定为 `adopt`，这不是 S3/64×252 或整体 B2 架构裁决。引擎 reset parity 尚未验证，作为后续范围。
