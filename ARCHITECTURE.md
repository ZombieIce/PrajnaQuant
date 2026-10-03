# Prajna Quant 目标架构基线

状态：**目标架构决策**，2026-09-26。本文记录本轮 Grill、调研与 POC 讨论达成的方向；不是现有功能清单。当前代码与验证状态以 [`docs/architecture.md`](docs/architecture.md)、[`docs/STATUS.md`](docs/STATUS.md) 和测试为准。下文“已确定”表示设计方向已确定，不表示已经实现或经真实数据验证。

## 目标与当前起点

目标是 Rust-first 的多市场、多策略研究、并行回测与未来实盘平台，覆盖 A 股股票和 ETF、美股、Crypto CEX，以及 Hyperliquid、Aster、edgeX 一类订单簿 / 永续合约 Venue，并最终支持跨 Venue 策略。核心价值是大规模回测吞吐、统一研究流程、可复现结果、回测到实盘的策略接口连续性和市场扩展能力。微秒级 HFT 延迟不是当前优化目标。

当前仓库是 A 股日频仓库和 ETF Rotation 纵向场景：Rust 已有日线入库、不可变发布、有限因子评价、ETF 日频回测、参数网格、结果 API；React 已有网页；Python `warehouse.py` 只是早期入库原型。尚无本文定义的三级引擎、通用 Factor Registry/持久 Cache、跨市场 Domain、Python 策略绑定、Nautilus Adapter 或 Live Runtime。已有 ETF 结果仍受历史 Universe、分红总回报及真实执行状态证据约束。

## 已确定的目标边界

### 计算所有权

Rust 拥有数据写入、因子执行、回测、组合账户、成本、绩效、实验和并行运行时，并保留未来 Live Runtime 的计算核心。Python 负责研究、Notebook、因子/策略/实验定义、优化和可视化；Python 逐 bar 回调可作为 MVP 能力，其吞吐边界由 POC 测量。Rust 是账户、成本和绩效报告的权威。现有 [`ADR 0001`](docs/decisions/0001-computation-ownership.md) 仍描述当前实现。

平台维护自己的 Domain Core。NautilusTrader 是 Accurate Event Engine 和未来 Live 的首个 Backend 候选，由 Adapter 接入；Barter、LEAN、WonderTrader、VeighNa 是特定领域的参考，不决定平台 Domain。Arrow、Parquet、Polars、Rayon、Tokio、PyO3 是目标技术角色或待评估依赖，不等于当前仓库已接入。

### 三级引擎与能力路由

| 层级 | 目标用途 | 核心计算 | 策略能力 |
| --- | --- | --- | --- |
| Vector | 因子研究、ETF 轮动、股票截面、参数扫描 | 价格/因子/信号/权重/收益矩阵；不要求订单和逐笔成交对象 | `Vectorizable` |
| Fast Event | 保留交易语义的高吞吐验证 | Rust bar → 决策 → 市价单 → L1 模拟成交 → 持仓/账户 | `Vectorizable`、`EventDriven` |
| Accurate Event | 高精度模拟与未来 Live 过渡 | 通过 Adapter 使用 Accurate Backend，首选候选为 NautilusTrader | 支持该 Backend 的能力，包括 `OrderBookDriven` |

策略声明所需能力；路由在运行前拒绝不支持的引擎组合。`Vectorizable` 表示策略可表达为矩阵式信号/权重；`EventDriven` 需要逐事件状态和订单；`OrderBookDriven` 需要订单簿语义。一个策略可声明多个能力，但不要求每个策略都运行在所有引擎。典型研究漏斗为大量参数先经 Vector，少量候选经 Fast Event，再由 Accurate 验证；100,000 → 1,000 → 50 → 5 只是容量示例，不是吞吐承诺。

统一的是 Strategy 身份、参数、生命周期、能力声明、时间和订单语义，以及 Backtest / Paper / Live 的使用方式。事件型策略的候选接口包括 `initialize`、`on_bar`、`on_quote`、`on_trade`、`on_timer`、订阅、下单/撤单、持仓和余额查询；Vector 策略可使用声明式因子与目标权重接口，不强制承担订单回调。统一报告契约为 `PortfolioResult`、`Metrics`、`ExperimentResult`；引擎内部状态与运算布局各自独立。具体 API、能力判定和跨引擎转换尚未冻结。

### 市场与账户语义

`InstrumentId`、`VenueId`、symbol、currency、价格和数量精度是跨市场共享身份与基础属性。Equity/ETF、Spot、Perpetual、Future 等类型保留各自的交易日历、T+1、涨跌停、funding、杠杆和清算规则。当前 Crypto 范围优先 CEX 与订单簿 / 永续型 DEX/CLOB；AMM、MEV、链上交易模拟和完整 Blockchain Domain 不进入平台 MVP。

Portfolio 是资金与风险管理的一等对象：Trading Account 可分配资本给多个 Virtual Portfolio 和 Strategy；实际现金、持仓、成交、费用及跨币种估值须可追溯。Fast/Accurate Engine 由成交驱动账户与组合。Vector Engine 可直接计算权重与收益，但其结果必须标出简化的成交、成本、现金、杠杆和再平衡假设；没有逐笔账本时不能声称通过账户守恒验证。跨引擎比较只在共同且显式的语义子集上做数值一致性检查。

### 数据与研究对象

目标 Data Lake 分为不可覆盖的 `raw/`、可版本化重建的 `normalized/`、`features/` 和 `results/`。原始字节、来源、采集时间及 hash 保留；Normalizer 修订产生新的 Dataset Version，不覆写旧版。目标使用 Parquet 持久化、Arrow 作列式交换契约、Polars 作研究与 ETL；Rust 热路径是否采用 Custom SoA 待基准结果决定。此处是目标分工，现有 `data-core/` / DuckDB / Parquet 发布链继续按当前 ADR 0008 运作。

Factor 是版本化对象，至少记录 ID、定义、参数、依赖、Universe、频率、输入字段、窗口、观察/可用时刻、方向、缺失规则、Dataset Version、来源及输出 schema。MVP 目标含 Factor Registry 与按完整输入身份键控的 Factor Cache，使共享因子可跨 Run 复用。未来收益标签只用于评价。当前固定信号目录及一次网格内评分复用不能称为这一目标已完成。

Experiment 包含策略与版本、Universe、数据版本、参数空间、成本模型、引擎、种子和多个 Run。目标 Run 身份由规范化有效配置、引擎语义版本、Dataset manifest、成本模型和 seed（仅随机性 Engine）确定，代码修订与编译环境决定 Experiment Execution 身份（[ADR 0018](docs/decisions/0018-run-spec-experiment-execution-identity.md)）；结果应能回答“什么代码、数据与参数生成了它”。脏工作树需记录差异摘要或明确标为不可完全复现。`ResultLevel` 为 `Summary`（扫描默认）、`Standard`（曲线、交易等）和 `Full`（事件、订单、成交、持仓、诊断）；每级明确数据可用性，避免把省略字段解释成零或无事件。当前实验 UUID 与 JSON 报告尚不满足该目标身份契约。

单机优先：以 Run 为 CPU 并行单位，Rayon 执行；Tokio 用于 I/O。数据读取按证券、日期和列裁剪，并用流式/分块路径处理大于内存的数据集。是否需要分布式、何时拆分单次回测或采用其他运行时由测量决定。

## 暂定方案

- Python MVP 同时允许 Python `on_bar()` 与 Rust Native Strategy；有限 DSL/Strategy IR 在后续版本仅覆盖真实需要的 Factor、Rank、Filter、TopK、Rebalance、Weight 与简单风险操作。
- NautilusTrader 是 Accurate/Live Backend 第一候选；其 Adapter 的映射成本、版本锁定和语义覆盖由 POC 验证。Barter 是否成为直接依赖同样待源码与基准证明。
- Custom SoA 仅在目标热路径实测有净收益且维护成本合理时采用；Arrow 仍是交换契约。
- Hyperliquid、Aster、edgeX 的具体 Connector、交易规则和数据许可均需逐 Venue 验证。

## POC 与交付顺序

先执行 [`POC-0 Benchmark Spec`](docs/poc-0-benchmark-spec.md)：(1) Polars / Arrow / Custom SoA，(2) 自研 Fast Event / Nautilus，(3) Rust Strategy / PyO3 Python callback。首批参考策略为 Buy & Hold、Momentum Rotation、MA20/60，其中 Momentum Rotation 贯穿因子、排序、TopK、调仓、组合、参数扫描与缓存。金标准数据和语义对拍从第一个引擎 POC 开始。

依赖顺序：POC-0 基准工具 → Domain Core / 版本化 Data Lake → Factor Registry / Cache / Vector → Experiment / Rayon 扫描 → Portfolio / Fast Event L1 → Nautilus Adapter 与一致性 → Python / PyO3 → 真实 ETF Rotation → Crypto / 跨 Venue / 美股 → Paper / Live。阶段序列是目标路线；具体进入条件和与现有 A 股产品路线的关系以产品路线文档为准。

平台 MVP 暂不把 Kubernetes、GPU、微服务、自研数据库、完整 DSL、L2/L3/L4、AMM/MEV、实盘、Web 前端或分布式执行列为验收项。现有 Web 已实现的功能继续按 `docs/STATUS.md` 记录；“不列为新 MVP 验收项”不表示移除已有代码。

## 待实验决定

1. Arrow 与 Custom SoA 的热路径收益和 Vector 内部布局。
2. Fast Event 对比 Nautilus 的吞吐与语义映射成本。
3. Python 逐 bar 回调的性能边界。
4. Barter 直接依赖的净收益、Nautilus Adapter 的转换成本。
5. 有限 Strategy IR 的实际覆盖范围；分布式技术栈在单机瓶颈出现后再选。

任何真实回测仍遵守 [`docs/time-model.md`](docs/time-model.md) 与已有 P0 数据/交易证据门槛；POC 的合成吞吐成绩不构成无偏历史业绩或生产交易可用性证据。

## 参考资料

- [NautilusTrader 回测接口](https://nautilustrader.io/docs/latest/concepts/backtesting/)与 [Rust API](https://nautilustrader.io/docs/latest/concepts/rust/)：Backend 候选能力；官方注明 Rust API 仍会变化。
- [Barter 项目](https://github.com/barter-rs/barter-rs)：Rust 事件系统与数据导向状态管理的参考。
- [Apache Arrow 列式格式](https://arrow.apache.org/docs/format/Columnar.html)与 [Apache Parquet](https://parquet.apache.org/)：交换格式与磁盘格式的设计依据。具体性能仍须在本项目负载上测量。
