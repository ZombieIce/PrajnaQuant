# 11: B3 真实策略与并行边界

**What to build:** 策略开发者可以对比 S2、S3 在 Rust Native、Python 逐 bar 回调及可批处理候选下的正确性和吞吐，确定 Python 接口在什么负载下成为瓶颈。

**Blocked by:** 09 B2 Momentum Rotation、MA20/60 与吞吐结论；10 B3 Rust 与 Python on_bar() 基础对照。

**Status:** ready-for-agent

- [ ] S2 和 S3 的 Rust/Python 信号、决策、账本和 PortfolioResult 对拍；批处理候选若可行，也须产生相同结果。
- [ ] 单 Run 与多个独立 Run 使用固定输入、预热、重复次数、线程/进程数；报告原始样本、中位数、p95、Runs/s 和峰值 RSS。
- [ ] 报告区分跨语言调用、策略计算、账户更新及 GIL/并行方式的影响。
- [ ] 预先登记目标负载与有意义门槛，给出逐 bar Python 的适用边界及 `adopt / defer / reject / unresolved` 结论。
- [ ] 报告将 Python 环境与 Rust 构建时间/空间单列工程成本；回调吞吐只在相同 release/依赖/线程条件下比较，资源阻断的规模保持 `unresolved`。
