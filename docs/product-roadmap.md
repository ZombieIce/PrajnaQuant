# 产品路线：多市场研究与回测平台

状态：2026-09-26 起的主路线。目标架构见 [`ARCHITECTURE.md`](../ARCHITECTURE.md)，当前已实现/未验收能力见 [`STATUS.md`](STATUS.md) 和 [`architecture.md`](architecture.md)。原 A 股日频 + Web 初级产品路线留存在 [`legacy-ashare-roadmap.md`](legacy-ashare-roadmap.md)，不再决定新平台 MVP 的交付顺序；已有代码、页面与数据不因此失效。

## 产品目标

以 Rust 为研究执行与绩效权威，构建支持 A 股股票/ETF、美股、Crypto CEX、订单簿/永续型 DEX/CLOB 与跨 Venue 策略的多市场平台。当前优先高吞吐并行研究和回测；未来 Paper/Live 沿用一致的策略身份、能力和运行语义。市场规则按资产与 Venue 特化，不以单一市场模型抹平差异。

## 交付阶段与验收

| 阶段 | 交付 | 进入下一阶段的证据 |
| --- | --- | --- |
| POC-0 | 可复跑的性能/一致性 harness；B1 列式布局、B2 Fast Event 对 Nautilus、B3 Rust 对 Python callback | 固定输入/环境/版本、独立金标准、原始测量与结论；未测项目保持 unresolved |
| MVP-0 | 自有 Domain Core、不可变 Raw、版本化 Dataset、Parquet/Arrow 契约 | 一份合成数据从来源/hash 到版本化输入可重建；当前 DuckDB 发布链边界仍可追溯 |
| MVP-1 | Factor Registry、Factor Cache、Vector Engine；含 D7 执行状态补充（[ADR 0016](decisions/0016-factor-cache-identity.md)、[ADR 0017](decisions/0017-vector-engine-semantics.md)） | 相同因子定义/输入复用缓存，缺失/可用时刻口径正确；S2 Momentum Rotation 与独立金标准一致 |
| MVP-2 | Experiment、Run 身份、ResultLevel、Rayon 参数扫描 | 固定代码/数据/配置/seed 可重放；Summary 扫描不写完整事件，吞吐和资源有实测 |
| MVP-3 | Portfolio、Virtual Portfolio、Fast Event L1 | 开工前由项目负责人审阅 POC-0 B2 正式结论与适用范围；实现验收为 S1/S2/S3 逐日现金＋持仓＝权益、费用可追溯，并与 Vector 在共同语义下对拍。B2 `adopt` 仅适用于登记合成负载，不等于生产 Engine 选型。 |
| MVP-4 | Nautilus Adapter 与 Accurate Backend | Backend 隔离于自有 Domain；固定金标准与 Fast Event 的差异可解释，转换成本实测 |
| MVP-5 | Python 研究/策略入口与 PyO3 | Python 定义可驱动 Rust 执行；逐 bar callback 的适用规模由 B3 结果约束。逐 bar callback 依 [ADR 0021](decisions/0021-python-strategy-entry-and-decision-boundary.md) 推迟；验收见 [#154 P16](https://github.com/ZombieIce/PrajnaQuant/issues/154)。 |
| MVP-6 | 固定名单真实 ETF 工程验收（按 #171 P27 及后续裁决收窄） | #235 第二轮六项工程 pass；#182 有保留签收，流程 fail 和适用限制保留，不证明完整历史 Universe 或无偏业绩 |
| 后续 | Crypto/Cross Venue、美国市场；再到 Paper/Live | 各 Venue 的数据、执行、风险和账户契约单独验收 |

阶段编号表示依赖，不要求所有 POC 工作串行，也不表示当前 ETF 代码须重写。POC 可使用合成数据推进；真实历史业绩对外解释仍受 [`priorities.md`](priorities.md) 的 P0 门槛约束。已实现的 React 页面可继续维护，但 Web、新实盘、分布式、GPU、Kubernetes、微服务、完整 DSL、L2/L3/L4、AMM/MEV 均不作为此轮平台 MVP 验收项。

## 当前阶段与后续选择

POC-0 至 MVP-6 的阶段交付已形成代码、合成验证与有限真实工程证据；MVP-6 milestone 已关闭，签收依据为 [#182 裁决](https://github.com/ZombieIce/PrajnaQuant/issues/182#issuecomment-6406038887)，不表示 POC 中所有候选被采纳或未知项已解决。Factor/Cache、实验身份、三级受限引擎及 Python 入口的当前能力见 [STATUS](STATUS.md)。

下一阶段尚未据本文件授权实施。可由负责人选择研究闭环（基准、样本外验证、参数/成本敏感性）或新市场接入；任何选择需先明确数据、执行和验收契约，再开票。Crypto/Cross Venue、美国市场及 Paper/Live 仍是后续路线，不因 MVP 收口自动成为已实现能力。
