# 产品路线：多市场研究与回测平台

状态：2026-09-26 起的主路线。目标架构见 [`ARCHITECTURE.md`](../ARCHITECTURE.md)，当前已实现/未验收能力见 [`STATUS.md`](STATUS.md) 和 [`architecture.md`](architecture.md)。原 A 股日频 + Web 初级产品路线留存在 [`legacy-ashare-roadmap.md`](legacy-ashare-roadmap.md)，不再决定新平台 MVP 的交付顺序；已有代码、页面与数据不因此失效。

## 产品目标

以 Rust 为研究执行与绩效权威，构建支持 A 股股票/ETF、美股、Crypto CEX、订单簿/永续型 DEX/CLOB 与跨 Venue 策略的多市场平台。当前优先高吞吐并行研究和回测；未来 Paper/Live 沿用一致的策略身份、能力和运行语义。市场规则按资产与 Venue 特化，不以单一市场模型抹平差异。

## 交付阶段与验收

| 阶段 | 交付 | 进入下一阶段的证据 |
| --- | --- | --- |
| POC-0 | 可复跑的性能/一致性 harness；B1 列式布局、B2 Fast Event 对 Nautilus、B3 Rust 对 Python callback | 固定输入/环境/版本、独立金标准、原始测量与结论；未测项目保持 unresolved |
| MVP-0 | 自有 Domain Core、不可变 Raw、版本化 Dataset、Parquet/Arrow 契约 | 一份合成数据从来源/hash 到版本化输入可重建；当前 DuckDB 发布链边界仍可追溯 |
| MVP-1 | Factor Registry、Factor Cache、Vector Engine | 相同因子定义/输入复用缓存，缺失/可用时刻口径正确；S2 Momentum Rotation 与独立金标准一致 |
| MVP-2 | Experiment、Run 身份、ResultLevel、Rayon 参数扫描 | 固定代码/数据/配置/seed 可重放；Summary 扫描不写完整事件，吞吐和资源有实测 |
| MVP-3 | Portfolio、Virtual Portfolio、Fast Event L1 | 开工前由项目负责人审阅 POC-0 B2 正式结论与适用范围；实现验收为 S1/S2/S3 逐日现金＋持仓＝权益、费用可追溯，并与 Vector 在共同语义下对拍。B2 `adopt` 仅适用于登记合成负载，不等于生产 Engine 选型。 |
| MVP-4 | Nautilus Adapter 与 Accurate Backend | Backend 隔离于自有 Domain；固定金标准与 Fast Event 的差异可解释，转换成本实测 |
| MVP-5 | Python 研究/策略入口与 PyO3 | Python 定义可驱动 Rust 执行；逐 bar callback 的适用规模由 B3 结果约束 |
| MVP-6 | 真实 ETF Rotation 纵向验收 | 历史 Universe、分红/价格口径、交易状态可知性与逐日账本证据满足当前 P0 门槛，结果可重放 |
| 后续 | Crypto/Cross Venue、美国市场；再到 Paper/Live | 各 Venue 的数据、执行、风险和账户契约单独验收 |

阶段编号表示依赖，不要求所有 POC 工作串行，也不表示当前 ETF 代码须重写。POC 可使用合成数据推进；真实历史业绩对外解释仍受 [`priorities.md`](priorities.md) 的 P0 门槛约束。已实现的 React 页面可继续维护，但 Web、新实盘、分布式、GPU、Kubernetes、微服务、完整 DSL、L2/L3/L4、AMM/MEV 均不作为此轮平台 MVP 验收项。

## 最近的一个开发任务

按 [`POC-0 Benchmark Spec`](poc-0-benchmark-spec.md) 扩展已跑通的 B1 标量读取切片：锁定 S2 的合成价格面板、缺失/窗口/排序规则和独立期望输出，再补 Polars 表达式、Parquet 扫描、排名/TopK、组合收益、转换与内存测量。任何历史 ETF 结果继续保留现有风险标注。B2/B3 在完整 B1 harness 可复跑后接入。
