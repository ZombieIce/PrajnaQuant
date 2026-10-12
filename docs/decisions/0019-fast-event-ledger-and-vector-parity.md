# ADR 0019：Fast Event L1 以定点账本为权威，并在显式参数下与 Vector 对拍

- 状态：Accepted，2026-10-09（MVP-3 设计，spec #112）；账本与 L1 Run seam、#122 合成 full_target 对拍已实现，Experiment 接入仍待后续票据验证

[`ADR 0010`](0010-portfolio-accounting-and-result-boundary.md) 要求 Fast Event 以 Order → Fill → Position → Account 更新账本并逐日核对恒等式，同时要求它与 Vector 在共同语义子集上对拍。MVP-3 确定以下边界：

- **账本数值**：现金、数量、价格与费用使用 Domain 定点十进制（i128，scale 18），乘除结果按 half-even 舍入到 scale 18，暂不做货币单位舍入。"现金＋Σ数量×估值价＝权益"在 Virtual Portfolio 与 Trading Account 两层都要求精确相等。与 f64 金标准（POC-0、Vector、Python）对拍时使用登记的容差。
- **Virtual Portfolio 是子账**：Run 开始时为各 Virtual Portfolio 分配固定资本，余额为 Unallocated Capital。每个 Virtual Portfolio 有独立现金与持仓，订单不跨 Virtual Portfolio 净额化，每个 Fill 只归属一个 Virtual Portfolio。恒等式分三条：账户现金 = Σ Virtual Portfolio 现金 + Unallocated Capital；账户每个标的数量 = Σ Virtual Portfolio 该标的数量；账户权益 = Σ Virtual Portfolio 权益 + Unallocated Capital。MVP-3 中一个 Virtual Portfolio 只运行一个 Strategy。
- **共用执行决策**：Fast Event 与 Vector 共用同一执行决策层（可交易性、[`ADR 0005`](0005-defer-rebalance-on-unavailable-exit.md) 整次延期、pending 替换），Fast Event 只在其后生成订单并记账。由于共享逻辑无法自证，对拍须有独立 Python 金标准作第三方裁判。
- **口径差异显式化**：数量规则 `sizing`（`vector_parity` / `lot`）、调仓规则 `rebalance`（`full_target` / `entry_only`）与未成交买腿策略 `unfilled_entry`（`skip` / `retry`，默认 `skip`）都是 Run Spec 参数。`vector_parity` + `full_target` 精确复刻 [`ADR 0017`](0017-vector-engine-semantics.md) 的成本与权重口径：成本以 open 前权益 × 权重换手为基数，open 后持仓等于目标权重 × 扣费后权益，Fill 价为 raw open，滑点作为费用单列。这是与 Vector 对拍的唯一模式。`lot` 引入整手、最低佣金与含滑点的成交价。`entry_only` 只在 Fast Event 中实现，仅用于复现 POC-0 金标准，不作为通用调仓语义。

## Considered Options

- 账本用 f64 并以容差检查恒等式：只能证明近似守恒，无法区分舍入误差与记账错误；被拒绝。
- 对拍时 Fast Event 按自然账本方式计算数量，并登记一个约 1e-6 的容差：差异来自成本二阶项，容差会掩盖真实口径漂移；被拒绝。
- Vector 模拟整手与最低佣金：ADR 0017 已拒绝；会让 Vector 承担账本职责。
- Fast Event 独立实现执行决策：两套延期/可交易性逻辑会产生需要额外解释的差异，"共同语义"失去保证。
- 只保留 `full_target`，POC-0 S2 金标准降级为参考：会丢失 B2 已对拍过的逐笔证据。改为以 `entry_only` 复现，并限定其用途。
- 订单在账户层跨 Virtual Portfolio 净额化：更贴近实盘，但会使 Fill 归属与费用分摊需要额外规则；留待后续阶段。

## Consequences

### Fast Event L1 Run 边界（#120）

`prajna_research::fast_event::run_fast_event` 消费 Panel、D7 execution_status 和各 VP
的 close Decision、资本与规则；策略信号生成不属于此接口。它复用 `ExecutionState`，
在每个 Session open 先规划各 VP 交易，再在账户范围按 sell / buy、InstrumentId、VP ID
顺序结算，每笔 Fill 独立归属 VP；open 后及 close 两个快照都调用账本精确校验。
`FastEventConfig` 保存定点成本和显式 Instrument lot size，结果声明未建模的市场规则。
Run Spec / ResultLevel 的持久化仍由 #123 接入，不得把内存 Run seam 称为完整平台验收。

Panel 保留原有 f64 `open` / `close` 研究列，并新增 `open_decimal` / `close_decimal`
字符串列：从 D7 Decimal128 原值经 Domain Price 编码，不经 f64。Fast Event 只使用后者，
缺列显式报错，不回退到浮点价。有效 raw bar 即使 HALTED / UNKNOWN 仍可估值，
但不能因此成交；缺 bar 分别沿用最近有效 raw open / close，保存源 Session 和字段。
这遵循 #117 已确认的估值约定，不以昨收代替当日缺失 open。

`vector_parity` 仅接受 `full_target`，数量与费用按 F8；retry 按 F5 只买缺失腿，
不足现金时按 ID 削减预算。舍入后仍超现金的 retry 数量用有界整数搜索削减，
不改变费用口径或放宽账本非负现金/精确恒等式；full-target 不作隐式舍入差额补偿。
无法满足 Domain 范围、Fill 正数量、非负现金或两层精确估值的输入返回错误。

项目负责人在 #120 自查后确认：F8 不使用绝对权重 no-trade 阈值，所有可表示的非零
权重差都参与费用及目标数量计算。#117 oracle 的 `1e-12` 抑制规则与严格 F8 不一致，
已在 #122 经负责人批准统一：oracle 的 vector_parity 账本改用独立标准库 Decimal，
scale 18 / half-even，信号仍为 f64；schema v2 保存数量精确字符串，七份对应 goldens
显式重算，不放宽 F7 的金额/NAV 容差。所有可表示的非零差都计费，不使用 no-trade 阈值。
Rust 用真实策略生成 Decision，再对照第三方输出的逐 Session 估值、执行轨迹、
订单/Fill 数量和费用，覆盖 S1/S2/S3 3×10、S3 3×130 与四个手算场景，
包括不伪造零数量订单/成交的 fee-only 2×5 用例（新增两份对应 golden）。
Vector 行含下一 open 的估值收益，对齐本 Session open 后权益时用
`nav / (1 + gross_return)`，不把不同估值时点误当引擎差异。
若某腿 F8 费用为正而扣费后目标数量恰好不变，用 `ParityFeeSettlement` 可追溯地扣费，
不伪造零数量 Order / Fill。它保存 VP / 标的 / open 时间、费用基数与方向及分项，
在全部实际 Fill 后、open 快照前原子结算；账户对同一 VP / 标的 / 时间拒绝重复。
完整现金重放为初始现金 + Σ Fill cash_delta + Σ Fee-only Settlement cash_delta。
例：初始 1000、raw open=1、佣金 1%；先持 A 权重 0.4975，再调 A=0.5/B=0.4975，
E0=995.025、E1=990.049875，A 数量仍 495.0249375，但需独立支付 0.024875625 佣金。
单纯跳过该费用会违反 F8；拒绝整个合法调仓或伪造零数量成交也不被采用。

项目负责人在 #120 实施中确认 F5/F9 的显式区分：`lot` 的 POC 专用
`EntryOnly(FixedInitial | RemainingCash)` 分别使用初始 VP 资本均分预算（S1）与
卖出后现金按尚未持有目标数均分预算（S2）；继续持有数量不变，未入场目标（含现金受限）
仍 pending。此规则通过 `ExecutionState::retain_unentered` 保留缺失腿，close 新决策照常
替换；`full_target` 不调用该扩展，现金削减仍不重试。POC 逐笔复现由 #121 验证，
不能仅凭规则实现声称旧金标准已完全复现。

### 账本边界与 scale-18 恒等式（#119）

`prajna-account` 独立承载 Trading Account / Virtual Portfolio、Order / Fill 和估值快照，
仅依赖 `prajna-domain` 与 Serde；不依赖 Research、Data、Experiment、旧量化引擎或 Nautilus。
它负责固定初始资本分配、唯一 Fill 归属、全额成交、长仓/非负现金、checked 定点核算与精确守恒。
执行决策、Session 顺序、整手、费率、最低佣金计算与缺行情估值来源仍由后续 Engine 负责。
crate 划分是 #112 Fog 的本票据提案，由 PR 独立 review 审核，不代表 Fast Event 已接通。

VP 与账户均独立按数量 × 给定估值价做 scale-18 half-even 估值，并精确核对账户市值与 VP 市值之和。
由于舍入不满足分配律，两个 VP 各 `1e-18` 数量 × `0.5` 价格的市值均为 0，
聚合数量的账户市值为 `1e-18`，此时两层恒等式无法同时成立，账本返回恒等式错误。
本实现不放宽原有精确契约；未来若需舍入差额分摊，应先作 spec/ADR 决策。

Fill 记录 raw 成交额、实际成交额、比例佣金、最低佣金补足额、税与滑点。
现金变动 = 有向 raw 成交额 −（比例佣金＋补足额＋税＋滑点）。
`vector_parity` 滑点单列；`lot` 的滑点取两个已舍入成交额的差，保证与有向实际成交额
减佣金/补足/税精确等价，不重复扣款；不做货币单位舍入。

### 共用买腿重试（#116）

`VectorStrategy.unfilled_entry` 声明 `UnfilledEntry::Skip`（默认）或 `Retry`；
`VectorStrategy::execute` 将声明传给共用 `strategy::execute_with_policy`。
原 `strategy::execute` 保留为默认 `skip` 入口，既有执行事件与结果 JSON 不变。
Experiment 的 retry 参数及 Run Spec 接入仍由 #123 负责，目前 Experiment 构造的 S2 策略明确使用默认 skip。

实际引擎使用 `ExecutionState`：每个 open 向 `attempt` 提供**实际非零持仓**，
执行/数量削减后更新引擎持仓，close 才调用 `decide`；最后取 `pending_at_end`。
Vector 的 `run_vector_with_policy` 使用这一增量共用状态，并将现金削减后的持仓反馈到下一次尝试；
Experiment 默认 skip 也走这一入口。
批量 `execute_with_policy` / `VectorStrategy::execute` 仅提供未做现金/数量削减的目标层事件投影，
不能代替账本反馈；`run_vector` 保留为已生成事件的校验/重放入口。
零现金买腿不会成为持仓，不会错误阻塞后续决策。

Retry 策略的首次完整调仓仍使用原有 full-target 成本与扣费后目标权重口径。
其后只对因可交易性阻塞的正权重买腿重试：原目标权重乘以本次 open **扣费前权益**作为买入预算，
费用另从剩余现金扣除，按 `instrument_id` 升序削减现金不足的预算；
不卖出或重平衡已成交腿，也不因现金削减新增 pending。
已成交腿在重试扣费后仅改变归一化权重，不改变数量。
持仓阻塞仍按 ADR 0005 整次延期；open 尝试结束后才读取本 Session close 的新决策并替换 pending。

共用事件 `RetryableExecution.is_retry` 区分首次调仓和缺失腿重试；
`applied` 是本次可执行腿的原目标权重，`skipped_buys` 是仍因可交易性受阻的腿，
不代表账本已实际成交的数量。`PendingAtEnd.targets` 在重试阶段仅含缺失腿。
Vector 的 `entries_retried` / `retry_deferred` 轨迹与 `pending_at_end.retry_entries=true` 记录这一模式；
默认 skip 不输出新增 pending 字段。Vector 仍是权重摘要，不是精确现金/持仓账本。

`fast_event@1` 的语义由本 ADR 与 spec #112 固定，任何改变数量、调仓、估值或费用口径的修改都必须提升 Engine 语义版本。Vector 增加 `unfilled_entry` 与 `BuyAndHold`、`MaCrossover` 策略形态后，默认值下的现有 S2 Run Spec 身份不得改变，否则须提升 `vector@1`。T+1、涨跌停、货币单位舍入与跨 Virtual Portfolio 净额化属于后续阶段；在实现前，Fast Event 结果须声明这些规则未建模。

### Vector 封闭策略形态（#118）

`VectorStrategy` 为 `TopKRank(TopKRank) | BuyAndHold(BuyAndHoldParams) | MaCrossover(MaCrossoverParams)`。
TopKRank 保留原参数与计算，参数语义版本为 1；新参数有 `params_version=1`，未知版本拒绝。
`decide_panel` 对指定 Universe 生成决策；BuyAndHold 首个 Session close 等权决策、固定 retry。
MaCrossover 默认 20/60 Session raw close 均值差，每标的独立交叉状态，持有目标固定 1/N，默认 skip。
只在目标状态改变时输出完整目标；缺失/未知/收盘后可用 gap 中断前后 gap 比较，保留既有目标状态，
下一可用 gap 仅建立比较基点。`ma_crossover::gaps` 提供值、状态与最大输入可用时间诊断。
Vector 的新参数接入 Experiment/Run Spec 仍由 #123 负责；现有 S2 canonical Run Spec 不新增字段，身份不变。
