# 02: 贯通 S2 3×10 同口径比较

**What to build:** 研究者用一条命令，在 S2 Momentum Rotation 3×10 golden 上跑通 Fast Event 与 Nautilus 的同口径比较，得到一份含正确性门、原始样本、RSS 归因和登记判定的比较报告。报告目前只覆盖 S2。参见 [spec](../spec.md)。

**Blocked by:** None (can start immediately)

**Status:** ready-for-agent

- [ ] 编排入口以独立子进程完成 golden 预检；预检失败时不采集该负载的性能，报告标 `unresolved` 或 `reject` 并附原因。
- [ ] Rust B2 CLI 新增串行/并行模式、worker 与 Run 数、跳过进程内预检等输入；3×10 checksum 与现有 golden 一致。
- [ ] Nautilus 在 worker 内一次性完成转换并把数据加入引擎，每 Run 先重置再加入新策略实例；同一 worker 内连续 Run 的投影 checksum 与新建引擎一致，否则自动改用"缓存转换、每 Run 新建引擎"，并在报告中记录所用模式与原因。
- [ ] 双方计时区间符合主边界，每 Run 只回传序号、耗时和 checksum；单 Run 延迟取单 worker 串行样本，并行 Runs/s 取 2 worker 墙钟。
- [ ] 测量进程的 RSS 分离采集：Rust 为单进程峰值，Nautilus 为 2 个 worker 峰值之和（不含编排进程）；报告同时列出每 worker 值，并把合计标为上界。
- [ ] 判定纯函数的单元测试覆盖：`adopt`、速度或 RSS 未达门槛的 `defer`、正确性失败的 `reject`、测量缺失的 `unresolved`、恰好 2× 与 RSS 相等两个边界，以及 ADR 0012 排除维度不一致但其余字段通过的情形。
- [ ] 缺少固定 Nautilus 版本时，集成测试跳过且报告标 `unresolved`；不断言绝对耗时。
