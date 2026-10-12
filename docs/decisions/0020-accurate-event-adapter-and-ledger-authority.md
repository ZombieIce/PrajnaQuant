# ADR 0020：Accurate Event 闭环 Adapter 与账本权威

- 状态：Accepted as target architecture，2026-10-12（MVP-4 设计，spec [#133](https://github.com/ZombieIce/PrajnaQuant/issues/133)，设计文档票据 [#134](https://github.com/ZombieIce/PrajnaQuant/issues/134)）；尚未实现，锁步与精度可行性待验证

[`ADR 0009`](0009-engine-capabilities-and-backend-boundary.md) 确立三级 Engine 与自有 Domain 的边界，但未固定 Accurate 层的订单反馈与账户权威。POC-0 的 Nautilus Python 探针是固定合成样本的比较工具，不是平台 Accurate Engine：它在 Python 侧重写数量与调仓，未接入平台账本与 Run Spec。MVP-4 选择由 Rust 执行决策、外部 Backend 撮合、平台账本反馈下一次决策的闭环，避免两套执行逻辑和账户权威。

## 决定

### 定位与账本权威（#133 A1–A3）

Accurate Event Engine 是平台的复核层级，不等同于 NautilusTrader。MVP-4 使用首个 Backend NautilusTrader 重新执行与 Fast Event 相同的基于 Decision 的策略，范围为 bar、市价单、L1；不新增策略能力。`OrderBookDriven`、限价单、Paper/Live 只约束未来扩展边界，不是本阶段验收项。Backend 类型不进入平台公共 Domain，Adapter 负责 Domain 与 Backend 对象的双向映射，并显式报告无法映射或由 Adapter 合成的事件。

Backend 成交转为 Domain `Fill`，记入 `prajna-account` 的 Trading Account / Virtual Portfolio 定点账本；平台账本与成本模型为权威，Nautilus 自身账户与费用仅作对照。沿用 [`ADR 0019`](0019-fast-event-ledger-and-vector-parity.md) 的 scale 18 / half-even 核算，VP 与账户两层精确检查现金、持仓与权益恒等式，不以浮点容差代替账本守恒。每笔 Fill 只归属一个 VP，订单不跨 VP 净额化；Nautilus 账户只在 Trading Account 层对照，偏差须分类登记。

### 闭环 Order / Fill / Reject 边界（A4、A7）

每个 Session open，Rust 从已记入 Backend 成交的平台账本出发，依据共用 `ExecutionState`（可交易性、[`ADR 0005`](0005-defer-rebalance-on-unavailable-exit.md) 整次延期、pending、`unfilled_entry`）及 `fast_event@1` 数量规则计算订单。Adapter 边界是 **Order 出 / Fill 或 Reject 入**；Backend 只接受、成交或拒绝这些订单并回传结果，不负责平台执行或数量逻辑。

闭环事件序列为：T 日 close 形成 Decision → 下一可用 Session open 的平台执行状态与账本决定 Order → Backend 返回 Fill 或 Reject → 平台按实际 Fill 入账 → 后续订单依据更新后的账本计算。不得在 T 日 close 同时成交，不得从 Fast Event 预生成整段订单后开环重放。

可交易性门槛由共用执行层负责，停牌标的不下单；Adapter 同时发送 `InstrumentStatus(HALT)` 给 Nautilus 作交叉校验。平台判定可交易而 Backend 原生拒单时，登记为已分类差异。继续遵守 [`ADR 0012`](0012-poc0-nautilus-status-gate.md) 的比较边界，分列 Adapter 项目事件（含合成事件）与 Backend 原生事件计数，不能把平台拦单冒充 Backend 原生行为。

### Worker 与锁步失败策略（A5、A6）

MVP-4 使用独立 Python worker 进程运行锁定版本的 `nautilus_trader` wheel；Rust Adapter 契约不依赖进程模型，为未来替换纯 Rust `nautilus-*` crates 保留边界。本阶段不使用 PyO3 嵌入（属 MVP-5）；纯 Rust 路径仅由限时 spike 记录可行性，替换需另行决策。

若 Nautilus 不能直接按 Session 锁步推进，允许两种形态而无需重新决策：

1. Rust 通过多次 `run(end=…)` 推进 Backend。
2. Python 策略 shim 经管道同步向 Rust 请求订单（控制反转）；决策、数量和入账仍在 Rust。

两种形态都不可行时停止并交项目负责人决策，不得静默退回 Python 重写执行逻辑或开环重放。以上是允许的设计路径，不是已经验证可用的 API 承诺。

### 成本与合成滑点报价（A8）

入账费用由平台成本模型按 Backend 实际成交数量与价格计算，使用 [#112 F8](https://github.com/ZombieIce/PrajnaQuant/issues/112) 的 `lot` 口径：佣金为 `max(费率 × 成交额, 最低佣金)`，税为 `费率 × 成交额`，滑点成本为 `|成交价 − raw open| × 成交数量`。滑点已包含在成交价中；单列滑点成本用于解释，不再次从现金扣除。

Adapter 合成 open 报价：`ask = open × (1 + 买方滑点)`，`bid = open × (1 − 卖方滑点)`，使 Backend 成交价可直接比较。这是登记的 Adapter 构造，不是 Backend 原生滑点模型或真实市场报价。Nautilus 使用按同一费率表构造的自定义 `FeeModel`，其费用只作对照，不取代平台成本计算。

### 数据包与线上数值编码（A9）

每个 Run 一次性下发 Backend 中立的 Arrow IPC / Parquet 数据包，包含 bars、执行状态和 Instrument 规格；逐 Session 的订单、成交与拒单通过 stdio 上的 JSON lines 交换。线上所有价格、数量、费用使用十进制字符串，不经 f64，必须通过 scale 18 往返测试。协议精确往返不等于 Backend 原生精度也达到 scale 18；无法映射的值或精度差异必须显式报告。

### Backend 身份与 Experiment 接入（A10、A11）

MVP-4 使用独立 requirements 文件，精确锁定首个票据开工时最新的 `2.0.0` 预发布版（#133 记录当前为 `2.0.0rc6`）；POC-0 工具继续锁定 `2.0.0rc5` 以复现归档证据。本 ADR 不更新依赖或把 spec 中的版本记录当作安装验证。

`prajna-experiment` 将新增一等 Engine `accurate_event`，语义版本为 `accurate_event@1`。扩展 [`ADR 0018`](0018-run-spec-experiment-execution-identity.md) 的 Run 输入：使用 Backend 时，Run Spec 包含身份 `{name: nautilus_trader, version: <锁定版本>}`；Backend 版本变更即身份变更，必须改变 Run Spec，而不是只记录为 Execution 环境变化。代码修订继续属于 Experiment Execution，ResultLevel 仍不进入身份。

Accurate 走同一 CLI、Run Spec 与 ResultLevel（Summary / Standard / Full，口径沿用 #112 F11，并增加 Adapter 合成事件与 Backend 原生事件分列）。并行单位是 worker 进程池，不是 Rayon 线程；本阶段不设扫描吞吐目标。

## 被拒绝的备选方案

- **Python 侧重写执行或数量逻辑**：会产生第二套延期、pending、数量与成本规则，失去共用执行层和平台账本反馈的保证。
- **开环重放 Fast Event 订单**：Backend 的实际 Fill / Reject 不影响后续订单，无法复核真实闭环，也无法解释现金与持仓路径差异。
- **Nautilus 账户与费用为权威**：与平台 Trading Account / VP 和成本口径形成双重权威；Backend 应提供成交事实，自身账户只用于诊断。
- **PyO3 嵌入**：MVP-4 采用独立 worker，避免把 Adapter 契约绑到嵌入式生命周期；Python 绑定属于 MVP-5，不以此绕过锁步验证。
- **MVP-4 直接采用纯 Rust `nautilus-*` crates**：Rust 暴露面（含费用模型与 `InstrumentStatus`）尚未核实；先用锁定 wheel 验证边界，Rust-only 可行性留给限时 spike，替换须另行决策。

## 后果与未决验证

- 本文记录目标设计，不证明 Accurate 已接通、金标准通过或 Backend 已锁步运行；不改写路线图 MVP-4 验收。固定金标准、差异分类与转换成本测量仍按 #133 A14–A16 执行；fake Backend 不得充当真实 Nautilus 验收证据。
- Nautilus 原生价格精度可能低于 scale 18，合成 bid/ask 未必可精确表示；worker 票据须核实并登记。Backend CASH 账户可能因自身对照费用与余额原生拒单，须按 #133 A15.3 分类：Adapter 缺陷须修复、Backend 原生语义差异须附证据、容差内差异须登记；未分类差异不通过。
- Session 锁步或 shim 同步回调、`reset()` 与每 Run 新建引擎的实际模式、worker 平台覆盖和 Adapter crate 划分仍待后续票据验证，不在本票宣称已解决。
- 合成样本结论不构成真实市场、PIT、分红总回报或生产 Engine 选型证据；Accurate 继承 Fast Event 未建模规则的声明（T+1、涨跌停、货币单位舍入、跨 VP 净额化）。
