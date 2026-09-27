# 10: B3 Rust 与 Python on_bar() 基础对照

**What to build:** 策略开发者可以通过统一入口，在同一事件流和 Rust 账户解释下比较 Rust Native 与 PyO3 Python `on_bar()` 的空回调和 S1 Buy & Hold，看到固定跨语言开销。

**Blocked by:** 07 B2 Fast Event 跑通 Buy & Hold；13 POC-0 构建资源基线与轻量边界。

**Status:** ready-for-agent

- [ ] 两种回调收到相同顺序的事件并产生相同决策；S1 的账本、成本和最终 PortfolioResult 对拍通过。
- [ ] 空回调与实际 S1 回调分别报告调用次数、原始重复样本、单 Run 延迟和端到端耗时。
- [ ] Python 仅表达策略决策，账户、成交和绩效由同一 Rust 逻辑解释。
- [ ] Python 环境或依赖无法运行时保留具体失败证据与复跑条件，标为 `unresolved`。
- [ ] 新增 PyO3/Python 依赖前按票据 13 检查 10 GiB 当前及预计余量；复用 POC 构建缓存并记录增量/release 构建耗时、target 增量及 Python 环境占用，资源不足时停止新增构建并保留 `unresolved` 证据。
