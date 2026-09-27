# 06: B1 参数扫描、缓存与布局结论

**What to build:** 研究者可以在三种布局上运行相同的 S2 参数扫描，对比因子缓存命中与未命中的端到端吞吐，并得到 Custom SoA 是否值得维护的有条件结论。

**Blocked by:** 05 B1 Parquet 与超内存数据路径。

**Status:** resolved

- [x] 固定 `top_n ∈ {1, 5, 10}` × `rebalance_every ∈ {1, 5}`；因子缓存键含输入内容 hash、布局、因子版本、窗口、评分权重和 trend 标记，不含 Top-K/调仓参数。6 个组合在三布局的 miss/hit checksum 逐 Run 相同。
- [x] 预登记 64 instruments × 252 sessions 和 20% 吞吐门槛；报告逐参数单 Run median/p95、顺序/两线程并行 Runs/s、冷热缓存、36 个 Run 原始重复样本、六组完整扫描样本和全进程峰值 RSS。
- [x] 六轮布局首位轮转、冷热条件顺序交替；报告 Apple M1/macOS/8 logical CPUs/16 GiB、Rust 与 lockfile 身份、转换计时边界及 Custom SoA 额外维护负担。
- [x] 给出有证据的 `defer`：SoA 并行吞吐相对最佳替代在 cache miss 为 +8.99%、cache hit 为 +0.32%，均未达到预登记的 20% 门槛。
- [x] 运行时使用同一 release 二进制/依赖配置。共享 `quant-research` dev/release warm build 分别为 5.582 s / -142,598,341 target bytes、9.675 s / +53,203 bytes；负增量是共享缓存的实测变化，不是负构建成本。冷构建与每布局构建成本因共用一个 crate/target、无法单独归因而明确记为 Unknown，不混入 Runs/s。

原始报告：[B1 sweep](../../../poc/poc0-benchmark/results/b1-sweep-64x252-2026-09-27.json)、[process RSS](../../../poc/poc0-benchmark/results/b1-sweep-64x252-2026-09-27.rss.json)、[dev build](../../../poc/poc0-benchmark/results/b1-sweep-build-dev-2026-09-27.json)、[release build](../../../poc/poc0-benchmark/results/b1-sweep-build-release-2026-09-27.json)。报告记录 Git revision、lockfile 与变更路径，但因工作树包含未跟踪文件，Git diff 身份不完整；完整复现须使用同一源码。最终布局结论限于确定性合成工作负载；不是现实市场形状、PIT 或实盘证据。
