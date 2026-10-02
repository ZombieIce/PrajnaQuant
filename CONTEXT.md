# Prajna Quant

量化研究、实验与交易模拟的共同词汇。此文件定义领域概念；实现状态和技术选择见 `docs/STATUS.md` 与 `ARCHITECTURE.md`。

## Market and data

**Venue**:
提供行情或执行交易的市场场所；同一资产可在不同 Venue 有不同规则。

**Session**:
一个 Venue 在本地交易日内的开闭市区间，使用 UTC 纳秒时间戳表示边界，且 `ts_open < ts_close`。

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
在指定观察与可用时刻、Universe 和数据版本上产生可比较数值的研究定义。

**Strategy**:
依据可用信息产生目标权重或交易意图的版本化规则；其所需数据与执行能力由 Strategy Capability 声明。

**Strategy Capability**:
策略运行所需的输入粒度与交互语义，用于判断其能在哪类 Engine 上执行。

**Engine**:
按明确时间、成本和成交语义执行 Strategy 并产生结果的计算层；Vector、Fast Event 与 Accurate Event 是不同层级。

**Order**:
请求按指定方向和数量交易 Instrument 的意图；提交不保证成交。

**Fill**:
Order 实际成交的一次记录，包含数量、价格、时间与费用依据。

## Capital and experiments

**Trading Account**:
持有实际现金、负债和仓位，并作为成交与清算账本权威的账户。

**Virtual Portfolio**:
在 Trading Account 内分配给一个或多个策略的资本和风险视图；其合并不能重复计算账户资产。

**PortfolioResult**:
在声明的估值、成本和执行假设下得到的组合表现及可用明细；不同 Engine 可提供不同粒度。

**Experiment**:
由固定研究问题、策略、数据、参数空间、成本与 Engine 选择构成的一组可比较 Run。

**Run**:
Experiment 中一组确定参数和输入身份的一次执行，结果须可追溯到代码、数据、成本与随机种子。

**ResultLevel**:
Run 持久化结果的详细程度：Summary、Standard 或 Full；省略的明细表示未保存。
