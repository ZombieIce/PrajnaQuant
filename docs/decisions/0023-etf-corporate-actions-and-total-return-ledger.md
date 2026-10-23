# ADR 0023：ETF 公司行动、顺序总回报指数与权威账本

- 状态：Accepted as target architecture，2026-10-14（spec [#171](https://github.com/ZombieIce/PrajnaQuant/issues/171) P19～P22，设计票据 [#173](https://github.com/ZombieIce/PrajnaQuant/issues/173)）。#180 实现合成验证的数据层 v2 结构化事件表与顺序 TRI；#181 接入 action-aware PIT Fast Event/Vector 账本、非投资应收权重、覆盖/不完整声明与逐日合成对拍。#204 增加合成验证的 `szse-etf-frozen@5`：新浪 qfq/hfq 精确因子与 TDX 参考价、cninfo 结构化行动三方核对，按 Catalog 生命周期处理复用代码，并将缺少发放日/清算金额列入覆盖缺口。研究侧回放按 Catalog 在行动日期映射生命周期身份。**未用真实新浪/行情/公告数据验收，公告完整性证明和真实收益验收仍未实现**。扩展 [ADR 0019](0019-fast-event-ledger-and-vector-parity.md) 的权益恒等式与 Vector 共同语义，不改写旧价格收益入口。

## 背景与边界

原始价格收益会将除息/折算机械变化当损益，前复权序列则会因未来事件改写历史。[ADR 0022](0022-historical-etf-catalog-and-derived-execution-status.md) 定义历史身份、状态与可用时刻，本 ADR 定义公司行动如何进入研究和账户。Rust 保持因子、成交、成本与绩效权威，不在 Python 或浏览器建立第二套量化核心。

6a 只做价格收益工程验收，不解释为策略历史业绩；6b 加分红、折算、清算和总回报对拍。来源为 cninfo 官方公告及附件，不能从行情跳变反推一条“已确认事件”。[#172 审计](../research/mvp-6-source-gate-audit.md)未证明公告完整性、控制样本事件链或许可，故本设计不放行 ingestion、真实总回报或备份/重放验收。

MVP-6b 的合成实现要求 qfq 与 hfq 派生事件完全一致，并将每份现金/份额比例直接从新浪字段精确解析；TDX 除权参考变化、因子事件和 cninfo 对应公告须三方匹配。v5 的验证仅说明冻结合成原文可以经同一规范化路径重放，不构成真实行情、来源完整性或公告字段完整性的证据。

## MVP-6 v7 date-only announcement handling (#248)

V7 preserves a closed-day announcement as a date-only fact. Its
`announced_at` stays null because no Session timestamp exists for that date;
`available_at` remains the close of the first later open Session, and
publication fails if the frozen calendar has no later Session. An open-day
announcement retains the existing Session-close timestamp. This uses a new
Normalizer configuration identity and leaves v4, v5, and v6 identities intact.
V6 continues to publish corporate-actions schema v1. V7 publishes schema v2
with nullable `announced_at`; readers resolve the expected table schema from
the Dataset manifest, preserving the v6 schema contract.

## 决定

### 事件、时钟与身份（P19、P21）

Corporate Action 绑定生命周期 InstrumentId，保存公告身份/原文 hash、事件类型、发布时间/可用时刻、生效日、登记/除息/发放或折算/清算日期、金额/比例、币种、适用规则与规范化版本；公告修订/撤销须可追溯、去重且产生新 Dataset Version，不能原地覆盖。只有日期的公告按 P6 最早用于下一交易日收盘决策，不能把生效日期或本地 `observed_at` 当发布时间。

研究可用性与账本生效是两条轴：Strategy/Factor 只见当时已公告且已可用的内容；账本按登记、除息、发放等实际权益时点重建，不因日期级公告假设把现金延后一个 Session。若迟到/缺失公告使历史权益虽可事后重建、但当时信号不可证明，则相关 Factor 为未知，不能用重建结果倒填历史 As-of View 或改写已发布结果。修订是新 DSV/Run，不是改旧 Run。

公司行动是独立结算事件，不伪造零数量 Order/Fill，不产生交易佣金或换手成本。真实买卖仍用 raw open 与显式成本模型；前复权价、TRI 均不能作为成交价。

### Total Return Index 与动量（P19）

TRI 从固定基点顺序累积，时点 t 的增量只纳入截至 t 已生效且已公告、按可用时刻允许使用的公司行动；未来公告不能重写此前值。一般单区间示例：上期每份变为本期 `r` 份，本期每上期份额取得现金权利 `d`，原始价格为 `P[t-1]`、`P[t]`，则

`TRI[t] = TRI[t-1] × (r × P[t] + d) / P[t-1]`。

无事件时 r=1、d=0；折算比和分红单位先按公告统一到同一份额基准。同日多事件按公告的权益基准和生效顺序组合，不能不分先后直接相加；公告不能确定顺序/基准时 UNKNOWN。除息确认分红权利后，发放不再重复给 TRI 加一次分红。

TRI 是理论总回报研究序列，链式累积隐含将分配权益继续计入投资回报；**不是实际账户的可用现金或 NAV**。实际账户发放日前不能再投资，应收部分不赚取标的价格收益；因此不能简单把 TRI 收益乘组合总权重冒充现金账本。动量方向为高值优先，输入为 TRI 和推导状态，Session 窗口、HALTED 持平、UNKNOWN `missing_input`、最晚依赖可用时刻按 ADR 0022。已知折算/除息即使停牌也须维持一致权益基准，无法解释则 UNKNOWN，不将机械除权当动量损失。

### Dividend Entitlement、应收与发放（P19）

权益登记日收盘的**实际持仓**锁定 Dividend Entitlement，按 VP 归属保存数量与公告每份金额；不是 Decision 目标权重，也不是除息日新买数量。锁定权利后卖出不取消该权利，除息日买入不获得本次权利。

除息时将权利确认为应收分红并计入权益，不增加可用现金。发放日将该应收转入现金，不再产生收益；按公告有明确到账时刻则用该时刻，只有日期时保守在该日收盘确认可用，不用于当日 open 预支交易。此日期级规则由 owner 在 #173 实现会话中确认，须进入 Run Spec / Engine 语义版本，不能隐式选择开盘到账。发放本身不触发调仓，下一正常调仓才按现金预算再投资。自 #274 起，未支付分红与清算应收不计入正常 parity full-target 的投资预算（可投资权益），但仍计入总 NAV；lot 的既有现金限量规则不在该扩展内。

个人分红税默认 0、可配置且随 Run Spec 保存；分开保存 gross entitlement、税额及 net receivable，权益中只计可归属净额，不能把税藏入价格或忽略。精确扣缴时点/规则必须有依据；无法映射的税规则不能假装支持。定点运算沿用 Domain scale 18 / half-even；币种或公告指定结算舍入须显式版本化，不静默补差。

在 VP 与 Trading Account 两层扩展恒等式：

`equity = available_cash + Σ(quantity × raw_valuation_price) + dividend_receivables + liquidation_receivables`。

账户现金/数量/各类应收分别等于 VP 子账合计（现金另加 Unallocated Capital），账户权益等于子账权益加 Unallocated Capital。登记权利未在除息前重复计入权益；应收不是现金，不参与下单预算。自 #274 起，正常 parity full-target 的可投资权益显式排除应收，且量化可行性证明不再重复扣除。每次事件、open 后和 close 均核验定点恒等式；估值来源与陈旧程度仍须记录。

### 折算、非整手与终止清算（P20）

按公告比例计算应得数量后取整数份（长仓向下取整），不补现金；保存原数量、比例、理论数量、实际数量和残余，并以 `corporate_action_rounding` 记录其按同一估值基准计量的权益差额，不把损失藏为价格收益或佣金。公司行动后允许非 100 整手持仓；新买仍按 lot 100，整仓风险退出须能卖出现有非整手余量，不能因整手取整永远残留。

登记时已锁定的分红权利不能被后续折算重新计算；若公告规定不同基准，按其显式顺序和数量基准转换。不能按“现在数量 × 旧每份分红”再次计算权利。

Risk Exit 未成功的终止持仓不消失，也不按末日行情强制平仓。按正式清算公告，在确定权利/金额时将证券权益转为清算应收，付款时转现金；若公告同日明确支付可直接结算现金，但须有独立事件轨迹。金额未知时结果标为**不完整**，列出未决头寸/权利与估值限制，不宣称完整最终总回报，不将未知清算款设为零或无限沿用末价冒充终值。多次清算派付逐项冲减权利，不能重复确认收益。

Within the research-only MVP-6 TRI semantics selected by `mvp6-market-driven@1`, `etf-total-return-index@3` deliberately excludes liquidation rows from the TRI and derived action ledger (#254). This does not change P20's account-settlement requirement: terminal positions are handled by #255, which also owns disclosure that settlement does not use actual liquidation proceeds or dates.

`mvp6-terminal-settlement@1` 是显式选择的研究估值语义：当 Catalog 生命周期在运行窗口内终止时，最后一个有 Bar 的 Session 开盘仍允许正常执行；若收盘时仍持有该标的，则按原始收盘价将持仓直接转为现金。该账户事件保留 Session、数量、价格与 proceeds，不生成 Order/Fill，不加收结算佣金或滑点；它不读取公告清算金额/日期，也不宣称实际清算款或完整最终总回报。Run Spec 与结果须披露“未按实际清算款与清算日期结算”，并说明最后 Bar 开盘交易仍按正常规则执行，收盘时按 Catalog 终止结算剩余持仓。若运行窗口内终止前没有可用 Bar close，运行显式失败，不以零价结算。此语义仅对 `terminal_settlement` 显式启用：Fast Event lot 继续使用 `fast_event@3`，Fast Event `vector_parity` 与全部 PIT Vector 使用 `fast_event@6` / `vector@6`（#263 共享量化现金可行计划；#274 起为 `fast_event@9` / `vector@9`）。未启用时既有行为和身份不变。启用该语义的 MVP-6 运行部分替代本节 P20 基于官方清算事实的终止清算路径；未启用时，P20 既有路径不变。该估值不代表官方清算权利，也不替代 P20 关于其他公司行动的账本要求。

### 公司行动完整性（P21）

每 ETF、生命周期及覆盖期间报告：公告类别、分页去重/计数与附件覆盖、窗口前资料、已解析事件、字段缺失、冲突/修订、价格不连续报警和逐项解释。无检索结果或“没检测到跳价”均不证明无公司行动。

以价格不连续检测辅助发现漏报，不以报警自动生成分红/折算。总回报运行遇疑似未解释除权除息时标相关输入 UNKNOWN；若影响持仓权益/声明范围完整性则报错，不能丢弃该 ETF 后继续输出成功形状的完整总回报。价格异常阈值/检测版本须固定并随输入身份保存，不能调阈值迎合验收。

### Fast Event 权威与 Vector 对拍（P22）

主运行为 Fast Event `lot + full_target`，其现金、持仓、费用、应收、折算与清算账本是绩效权威。成本沿用 P16 并保存（初始 100 万、lot 100、佣金 3bp/最低 5 元、双边滑点 2bp、税 0）；TRI 或 Corporate Action 不绕过成本配置。Accurate 仅作 Full workflow 冒烟，不取代平台账本。

另建 Vector / Fast Event `vector_parity + full_target` 对拍：共享 PIT Universe、Eligible Set、Risk Exit、执行状态、原始估值点和公司行动时间轴；继续采用比例成本、无最低佣金与无整手的共同语义，不拿主运行 lot 结果要求逐值相同。Vector 在权重层显式分离投资部分、可用现金和应收非投资部分；除息记权利，发放只转桶，下一正常调仓才再投资。正常 full-target 目标与 F8 候选费用按可投资权益计算（#274），实际权重与报告分母仍为总 NAV。公司行动本身不计换手，非投资部分不随标的价格漂移。

比例对拍允许连续数量、无份额取整；实际整数折算及 `corporate_action_rounding` 由 lot 权威账本单独验收，不静默混入 Vector 共同模式。Vector 不因此获得订单/逐笔账本或精确账户守恒证明。逐日比较相同 open 后与 close 的权益/NAV、投资/现金/应收比例、状态/延期轨迹及成本；延续 ADR 0019 的已登记数值容差，不能扩大容差掩盖重复分红或提前再投资。需独立手算/Python oracle，不能仅依靠共享代码自证。

## 事件顺序与可人工验算用例

每个 Session：生效公司行动/除息入应收、折算 → 同时点已有应收按到账证据转现金 → 下一开盘执行 pending/Risk Exit → open 后估值 → close 估值及登记日权利锁定 → 当时可用信息形成后续 Decision。日内有精确时刻按实际顺序；日期级发放延至 close，不能在 open 提前使用。公告可知与风险触发按 ADR 0022，不提前使用未来信息。

以下是**合成设计预期**，不是已运行测试；忽略成本、税为 0：

| 场景 | 手算预期 |
| --- | --- |
| 分红 | 登记日 close 持 100 份、价 10、现金 0，权益 1000；每份分红 0.1。除息日 raw open=9.9，应收 10、证券 990、现金 0，权益仍 1000；TRI 从 100 到 100。 |
| 发放与卖出 | 上例除息日卖出 100 份得现金 990，应收仍 10；发放时现金变 1000、应收 0，权益不变。除息日新买者无本次权利，不能复领。 |
| 应收不再投资 | 未卖出且发放前价格从 9.9 涨至 10.89，权益为 1089+10=1099，而不是将全部 1000 投资所得 1100；Vector 应匹配 1099，不直接用 TRI 当组合 NAV。 |
| 折算 | 101 份 × 1.5=151.5，实际 151 份；原价 9、新参考价 6，市值由 909 到 906，`corporate_action_rounding` 为 -3，无现金补偿。连续数量 parity 模式为 151.5 份、市值 909，须标明模式差异。 |
| 清算 | 100 份最后估值 1，正式公告每份清算 0.8：转证券 0、清算应收 80，并显式确认 -20 差额；支付后现金 80、应收 0。若金额未知，只能输出未决与不完整结果。 |

合成 3 ETF × 10 Session 金标准现覆盖分红登记/除息/发放、应收非投资收益、份额折算、终止清算、Vector/Fast Event 逐日对拍、现金/持仓/应收/NAV 恒等式、未解释价格跳变和未知清算金额；另以一份 lot-mode 用例验证折算取整差额。无公司行动样本与价格收益回归通过。真实 159919/159522（折算另含 159560）仍仅是待官方证据链确认的本地控制样本，不能把 spec 给定日期当已验证事实。

## Considered Options

- 以未复权价格收益冒充总回报，或成交使用前复权价：分别遗漏现金/数量权益、生成不可成交价格，拒绝。
- 前复权序列作为 PIT 动量输入：后续事件改写历史；采用顺序 TRI，新事件不回写旧时点。
- 除息日直接加可用现金、发放日再记一次收益、按除息日仓位发分红：分别提前再投资、重复收益、错配权利，拒绝。
- 折算后补现金或静默丢尾数、终止时删除头寸/按末价强卖：与公告及 P20 不符，拒绝。
- Vector 对实际 lot 账本无条件逐值相同、将全部权益按 TRI 复利：掩盖取整、最低佣金和应收非投资差异，拒绝；限定共同模式并独立验收真实账本。
- 价格跳变自动推断公司行动、缺公告但附风险说明就声称 P0 完成：不能替代官方权益及完整性证明，拒绝。

## 后果与后续验收

事件规范化与 TRI 由 #180 提供；#181 为 Fast Event 账户增加分红/清算应收和转换账本，为 Vector PIT 增加现金/应收非投资组件，并将 Return Basis、税率、覆盖报告与不完整原因写入 Run Spec/Summary，Standard/Full 保存非空的公司行动事件行。Summary 不含逐事件明细；缺失的 Vector 账本不能解释为零。`Complete` 覆盖只接受显式输入声明；从 TRI 映射的 `observed_raw_set_only` 不构成公告完整性证明。当前 synthetic API 与 ResultLevel 不替代真实 cninfo 解析、真实事件链、来源授权或 P0-2 验收。

6b 基准为沪深300全收益指数，需声明来源、版本和日期对齐，不与价格指数混用。按 P24/P25 冻结本地授权原文、数据/规则/代码/环境与离线备份，两次独立执行所有 ResultLevel logical hash 一致；仓库只保存合成金标准、hash/身份、覆盖与守恒摘要、命令日志，不发布真实行情、公告 PDF 或逐日价格账本。#172 的许可、规则、公告与覆盖未决项均须先解决，设计采纳不等于真实验收通过。


### #275：正常 lot full-target 投资预算

#275 完成 #273 的 lot 预算迁移，复用 #274 的可投资权益（现金＋证券、排除应收），保留发放日 close 转桶、下一正常调仓投资及总 NAV 会计/报告。Lot 保留整手、实际费用、最低佣金和现金限量；F8 继续严格量化可行性规则。normal/action/terminal 使用 `fast_event@10/@11/@12`，旧 revision 保留旧行为。完整预算、版本边界和两票覆盖说明见 [ADR 0019 的 #275 修订](0019-fast-event-ledger-and-vector-parity.md#275正常-lot-full-target-投资预算)。不构成 #234/#235 或 MVP-6 验收通过。
