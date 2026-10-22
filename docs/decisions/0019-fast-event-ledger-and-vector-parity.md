# ADR 0019：Fast Event L1 以定点账本为权威，并在显式参数下与 Vector 对拍

- 状态：Accepted，2026-10-09（MVP-3 设计，spec #112）；账本与 L1 Run seam、#122 合成 full_target 对拍已实现，Experiment/ResultLevel 接入与合成 Summary 扫描由 #123 实现；验证范围见后文

MVP-6 扩展（2026-10-15）：[#181](https://github.com/ZombieIce/PrajnaQuant/issues/181) 按 [ADR 0023](0023-etf-corporate-actions-and-total-return-ledger.md) 增加 action-aware PIT Fast Event 应收/转换账本与 Vector 非投资现金/应收权重，并纳入事件账本和完整性结果。完全合成 3 ETF × 10 Session 对拍验证通过；不改变旧价格收益入口，也不构成真实公司行动覆盖或收益验收。

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

### Fast Event Experiment / ResultLevel 接入（#123）

Experiment 接受 `fast_event@1` 与三种封闭策略；旧 `s2_rotation` / `vector@1`
默认 skip 的 Run Spec 不变，显式 skip 不追加身份字段，retry 则显式进入身份。
Fast Event Run Spec 追加规范化账户币种/初始资本、VP ID/固定资本分配、sizing、rebalance、
POC entry budget、lot size 来源及 DSV/显式解析值、最低佣金；线程数与 ResultLevel 不入身份。
此 CLI 的多个 VP 使用同一 Run 策略/参数，不提供异构 VP 策略定义。

Summary 内仍验证每 Session 两点/两层恒等式，失败 Run 中止并标 failed，其他 Run 继续；
成功 manifest 标记 accounting_verified，摘要后释放 Run 明细。
Standard 保存账户和 VP 双估值点（含 exact decimal、估值价来源）；Full 保存提交 Order、
阻塞/跳过尝试、整手/现金裁减原因及前后数量、Fill 全部费用分项，并额外保存 `fee_settlements` 以保证 F8 fee-only 现金可重放。
JSON payload 封装为 row_id 寻址的 Parquet 表，logical hash、等级提升与已有结果重放沿用 ADR 0018。
Fast Event E11 使用账户 daily close NAV，turnover/cost 按 raw-open pre-trade equity 归一；
Vector 保持 open NAV，跨引擎数值比较仍须按前文对齐估值点。
具体输入、表映射与局限见 [Experiment 契约](../../crates/prajna-experiment/README.md)。

### #177 PIT 方向门禁的共同 sizing 边界

`s2_pit_rotation` 的 Vector 与 Fast F8 共用实际货币/数量 sizing，不先在归一权重上量化成本。
顺序为实际现金与 raw-open 持仓估值 → drift → 每腿费用基数 → 分别量化佣金、税和滑点 →
扣费权益 → 量化目标金额 → 按 raw price 量化目标数量；以数量差判断方向，只有数量差严格为零
且费用非零时才回退到 fee-only side。不得用 epsilon 消除合法数量 quantum。
PIT Vector 必须显式携带 `vector_pit.initial_capital`，纳入 Experiment/Run 身份，并保留精确现金/数量
作为后续 sizing 输入，不能从 f64 NAV/权重重建。该最小状态只用于 sizing，Vector 的收益摘要仍独立；
Fast `TradingAccount` 的订单、成交、fee settlement 与双层账本仍是 canonical，旧 Vector 路径不变。

### Full-target 显式量化修正（#263）

`vector_parity + full_target` 的候选数量、佣金/税/滑点与 fee-only 费用仍按原 F8
scale-18 half-even 算法计算，不因后续现金修正重算。共享计划在已成交卖出结算后，
先按实际现金（不含分红/清算应收）预留所有尚未支付的买腿及 fee-only 费用，
不重复预留已付卖腿费用；再按 InstrumentId 升序分配买腿本金预算。
若某买腿按候选数量计算的 checked 实际定点成交额超过剩余本金预算，只把该腿数量
降到不超过候选值、且按实际定点成交额可支付的最大 representable 数量，
并以 checked 定点运算保存候选/实际数量、候选/实际成交额、预算、预留费用和现金余量。
不得跨过当前数量改变方向，不得增加卖腿筹资、跳过、重试或重归一目标；
调整后仍执行原有方向与可交易性门禁，保留 fee-only settlement 契约。现金结算在
这些门禁之后；被阻塞卖腿或跳过买腿不会因资金检查提前失败，只有实际会执行的计划才报不足。

仅当未量化的同一计划仍能由实际现金支付时，才允许上述削减，从而证明缺口来自
scale-18 金额/数量量化；该比较使用 checked 定点数与 256-bit 整数乘积，不使用固定 epsilon。若应收账款、冻结/保留持仓或
真实资金不足使未量化计划本身不可行，则明确失败，不把应收当现金，也不以 epsilon、
clamp、资本补差或减费掩盖。PIT Vector 与 Fast 的非 retry `vector_parity`（含非 PIT）
消费同一计划；lot、独立 retry 与旧权重式 Vector 保持原规则。

受影响路径使用 Fast Event `fast_event@4`（公司行动 `@5`，终止结算 `@6`）和
Vector PIT `vector@4`（公司行动 `@5`，终止结算 `@6`）；lot 仍为 `fast_event@1/@2/@3`，
旧 Static Universe `vector@1` 不变。规范化 Run Spec 与结果版本一致；历史资产不覆盖，
旧行为由旧提交复现，不提供双模式 API。
