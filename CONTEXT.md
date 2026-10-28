# Prajna Quant

量化研究、实验与交易模拟的共同词汇。此文件定义领域概念；实现状态和技术选择见 `docs/STATUS.md` 与 `ARCHITECTURE.md`。

## Market and data

**Venue**:
提供行情或执行交易的市场场所；同一资产可在不同 Venue 有不同规则。

**Session**:
一个 Venue 在本地交易日内的开闭市区间，使用 UTC 纳秒时间戳表示边界，且 `ts_open < ts_close`。

**Shared Session Axis**:
多 Venue 研究面板共用的交易日序列；参与 Venue 的 Session 日期集合与每个日期的开收盘时刻必须完全一致，不一致是结构性错误，不自动补齐，也不把缺一侧日历当作停牌。
_Avoid_: 把各 Venue 日历取并集、按行情日期推断休市，或用伪造 Venue 伪装成单一 Session 轴。

**Venue Scope**:
一次 Run 允许的 Venue 集合；多 Venue Run 共用一条 Shared Session Axis 和一套现金/持仓/应收账本，不按 Venue 拆账或净额化。
_Avoid_: 静默选第一个 Venue，或为每个 Venue 建独立账户。

**TimestampNs**:
带显式时区的 RFC 3339 时间规范化成的 UTC Unix 纳秒时间戳；没有时区的时间不能构造。

**BarSpec**:
行情 Bar 的长度与区间锚点，例如 `1d@session` 或 `1d@+08:00`；session 锚定区间使用对应的 Session，固定偏移区间按该偏移的本地日期切分。

**BarData**:
按 D7 字段名组织的 Bar 输入值；只有经 OHLC、数量、时间与区间校验后才构造成 Bar。

**Bar**:
按 Instrument、Bar Spec 和交易日标识的 OHLCV 行情区间；`available_at` 表示数据可用时刻，缺失表示未知，不从 `ts_close` 推定。

**Instrument**:
可被观察、评分、持有或交易的具体金融标的，身份须能区分 Venue 与资产类型。
_Avoid_: 仅用裸 symbol 代表跨市场唯一身份。

**InstrumentSpec**:
对一个 Instrument 的经校验描述：种类、币种、乘数、到期日、价格与数量最小变动单位、交易时区；语义只从它读取，不从 `{SYMBOL}.{VENUE}` 形式的 ID 解析，同一 ID 不得对应不同 Spec。
_Avoid_: 用 ID 字符串推断标的是现货、永续还是交割。

**Universe**:
某个决策时点按明确来源、版本和成员可知性规则得到的候选 Instrument 集合。
_Avoid_: 把今天的静态名单称为历史可投资全集。

**Static Universe**:
不随决策时点变化、带版本的显式 Instrument 列表；没有成员可知性证明，不是 point-in-time Universe。

**Point-in-time Universe**:
按固定筛选规则作用于交易所全量名录，在每个决策时点只依据当时已可知的上市、终止与属性信息得到的 Universe；成员可知性超出证据范围时不得成立。
_Avoid_: 把今天挑选的名单加上市/退市过滤称为 Point-in-time Universe。

**Eligible Set**:
一个决策 Session 中从 Point-in-time Universe 按历史长度、流动性、数据可用性和执行状态筛出的可评分及可新买 Instrument 集合；不合格不改变 Instrument 的历史成员身份。
_Avoid_: 因当日缺数据或不可交易而把 Instrument 从 Point-in-time Universe 删除。

**Historical Instrument Catalog**:
由交易所名录、披露公告和历史行情证据交叉重建、带版本与覆盖说明的曾存续 Instrument 集合；来源冲突或遗漏范围必须显式记录。
_Avoid_: 把交易所当前在市列表当作历史完整名录。

**Derived Execution Status**:
按版本化规则从官方停复牌公告与多个独立行情来源推导的 Instrument/Session 开盘可交易状态，取值为 TRADABLE、HALTED 或 UNKNOWN；证据缺失或冲突时为 UNKNOWN。
_Avoid_: 用缺少行情单独证明停牌，用当天有成交证明开盘可交易，或把 UNKNOWN 当作 TRADABLE。

**Corporate Action**:
由发行人公告、改变持有人现金或持有数量的事件，如分红、份额折算和终止清算；其可用时刻是公告发布时刻，而非生效日期。

**Return Basis**:
结果所声明的收益口径：价格收益只反映成交价变化，总回报另计入 Corporate Action 带来的现金与数量变化。
_Avoid_: 用复权价回测结果冒充总回报。

**Total Return Index**:
按时间顺序累积、每个时点只含此前已生效且已公告 Corporate Action 的价格序列；之后的事件不改写其历史值。
_Avoid_: 前复权价（每次新事件都会改写历史）。

**Dataset Version**:
可追溯到原始资料、规范化规则和内容身份的一版研究输入；修订产生新版本。

**Raw Object**:
按 SHA-256 内容寻址、写入后不可改的原始字节，附只追加的来源记录；同一 hash 的字节不同即为损坏，不覆盖。
_Avoid_: 把规范化后的表或本地采集时间 `observed_at` 当作原文或历史发布时间。

**Normalizer**:
按 `(id, version)` 登记、把 Raw Object 确定性地转换为规范化表的版本化规则；规则修订是新版本，并产生新的 Dataset Version。

**Logical Hash**:
对一张规范化表按主键排序后的值（而非 Parquet 文件字节）计算的 SHA-256，与 schema 指纹共同判定“逻辑相同”。
_Avoid_: 用 Parquet 文件字节 hash 判断两次重建是否相同。

## Research and execution

**Factor**:
带版本的研究定义，声明种类、参数、输入字段、窗口、方向、缺失规则与可用时刻规则；本身不绑定 Universe 或 Dataset Version。

**Factor Values**:
一个 Factor 应用于特定 Dataset Version 与 Universe 得到的、带可用时刻的数值；同一 Factor 在不同输入上产生不同的 Factor Values。
_Avoid_: 把计算结果直接称为 Factor。

**Factor Cache**:
按产生 Factor Values 的完整输入身份保存的不可变 Factor Values，可跨 Run 与进程复用。

**Availability Assumption**:
输入可用时刻未知时，Run 显式声明并随结果保存的替代规则；未声明时可用时刻未知的值不得用于决策。

**Strategy**:
依据可用信息产生目标权重或交易意图的版本化规则；其所需数据与执行能力由 Strategy Capability 声明。

**Decision**:
Strategy 在一个决策 Session 收盘时依据当时可用信息给出的目标权重集合；之后在下一可用时刻由 Engine 执行，本身不含订单、成交或账户状态，是各 Engine 共同消费的策略输出。
_Avoid_: 把 Decision 当作已成交的仓位或订单。

**Risk Exit Decision**:
已知 Instrument 进入可能终止上市或基金合同终止的程序后，为停止继续暴露而产生的目标退出 Decision；它由风险门禁触发，不是 Strategy 的择时信号，也不等待普通调仓周期。

**As-of View**:
为一个决策 Session 提供给 Strategy 的只读信息集，只含在该 Session 收盘时已可用的数据与 Factor Values，以及 Universe 在该时点的成员；不含账户状态，也不含此后才可用的任何值。
_Avoid_: 把截至某日期的数据切片等同于 As-of View，而忽略可用时刻与 Universe 可知性。

**Strategy Capability**:
策略运行所需的输入粒度与交互语义，用于判断其能在哪类 Engine 上执行。

**Engine**:
按明确时间、成本和成交语义执行 Strategy 并产生结果的计算层；Vector、Fast Event 与 Accurate Event 是不同层级。

**Accurate Event Engine**:
借助外部 Backend 复核其他 Engine 结果的事件层级；其账户与组合仍由平台账本记账并作为权威，Backend 自身的账户只作对照。
_Avoid_: 把 Accurate Event 等同于 NautilusTrader，或把 Backend 账户当作结果权威。

**Backend**:
在某个 Engine 之后实际执行撮合等计算的外部实现，例如 NautilusTrader；其类型不进入平台公共 Domain，其身份与锁定版本属于 Run 输入。

**Adapter**:
Domain 对象与 Backend 对象之间的转换边界；负责双向映射，并显式报告无法映射或由 Adapter 自行合成的事件，不得将其冒充为 Backend 原生行为。

**Order**:
请求按指定方向和数量交易 Instrument 的意图；提交不保证成交。

**Fill**:
Order 实际成交的一次记录，包含数量、价格、时间与费用依据。

**Dividend Entitlement**:
持有人在权益登记日收盘时的持仓所决定的分红权利；在除息时计为应收分红并计入权益，发放日才成为可用现金。
_Avoid_: 把除息日当作现金到账日。

**Fee-only Settlement**:
只有费用现金变化、没有成交或持仓数量变化的可追溯结算事件。
_Avoid_: 为了记费用而伪造零数量的 Order 或 Fill。

## Capital and experiments

**Investable Equity**:
投资预算，等于可用现金加证券估值，排除未支付分红应收与清算应收；正常 full-target 的目标权重、F8 drift、候选费用基数和扣费后目标数量使用它。
_Avoid_: 把可投资权益当作总 NAV，或从应收中预支交易；总 NAV 仍包含应收并作为实际权重、收益、成本率与换手率的分母。

**Trading Account**:
持有实际现金、负债和仓位，并作为成交与清算账本权威的账户。

**Virtual Portfolio**:
在 Trading Account 内分配给一个或多个策略的资本和风险视图；其合并不能重复计算账户资产。每个 Fill 只归属一个 Virtual Portfolio，账户现金等于各 Virtual Portfolio 现金之和加 Unallocated Capital，账户每个标的数量等于各 Virtual Portfolio 该标的数量之和。
_Avoid_: 把 Virtual Portfolio 当作独立账户或可单独清算的资产主体。

**Unallocated Capital**:
Trading Account 中未分配给任何 Virtual Portfolio 的现金；计入账户权益，但不属于任何策略的结果。

**PortfolioResult**:
在声明的估值、成本和执行假设下得到的组合表现及可用明细；不同 Engine 可提供不同粒度。

**Experiment**:
由固定研究问题、Strategy、Dataset Version、Universe、成本、Engine 与 Parameter Space 构成的一组可比较 Run；定义相同即为同一 Experiment，重跑须与已保存结果一致。
_Avoid_: 把每次执行当作新的 Experiment。

**Parameter Space**:
Experiment 中允许变化的 Strategy 参数组合，以网格或显式列表给出并按规范化后的组合去重；成本、数据、Universe 与 Engine 不属于 Parameter Space。

**Run Spec**:
决定一个 Run 输入的规范化描述：Dataset Version、Universe、Strategy 及参数、成本、Availability Assumption、Engine 及其语义版本、使用 Backend 时的 Backend 身份与锁定版本，以及仅当 Engine 使用随机性时的随机种子；相同 Run Spec 在相同代码上必须得到相同结果。
_Avoid_: 把代码修订当成 Run Spec 的一部分；为不使用随机性的 Engine 填写种子。

**Run**:
按一个 Run Spec 的一次执行；结果须可追溯到 Run Spec 与执行时的代码修订，工作树不干净时标为不可完全复现。

**Experiment Execution**:
一个 Experiment 在确定代码修订、工作树差异与编译环境上的一次执行；相同 Execution 的重放须逐 Run 一致，代码变化产生新的 Execution，可按 Run Spec 与旧 Execution 对比。
_Avoid_: 把不同代码上的结果覆盖或合并到同一 Execution。

**ResultLevel**:
Run 持久化结果的详细程度：Summary、Standard 或 Full；省略的明细表示未保存。Engine 本身不产生的明细（如 Vector 的订单与成交）另行标注，与“未保存”不同，都不表示零。
