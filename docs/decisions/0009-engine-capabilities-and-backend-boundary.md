# ADR 0009：以策略能力路由三级引擎，保留自有 Domain Core

- 状态：Accepted as target architecture，2026-09-26；尚未实现

现有 ETF 日频回测能验证一个纵向场景，却无法同时满足大规模参数扫描、事件交易语义和未来高精度/实盘扩展。把每次研究都放进完整事件框架会拖慢扫描；把全部策略压成矩阵又无法表达订单簿与跨 Venue 事件。因此平台采用 Vector、Rust Fast Event、Accurate Event 三层引擎，以 Strategy Capability 选择可用层级。Vector 不必建立 Order/Fill 对象，Fast Event 的 MVP 只实现 bar、市价单、L1 成交、成本、持仓、现金与组合；Accurate 首选 NautilusTrader，但通过 Adapter 连接自有 Domain Core。

策略身份、参数、时间/执行假设和结果契约由平台定义；Backend 类型不进入平台公共 Domain。不同 Engine 的结果仅在共同的假设和能力范围内比较，跨层推广候选时记录假设差异。Nautilus、Barter 等外部项目的具体采用方式仍由 POC 性能与映射成本决定。当前 [`ADR 0001`](0001-computation-ownership.md) 与 [`ADR 0002`](0002-current-event-time.md) 继续约束现有代码；本 ADR 不声称三级引擎已经落地。
