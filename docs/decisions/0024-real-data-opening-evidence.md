# ADR 0024：真实开盘证据与含公司行动 Dataset 的 PIT 路径

- 状态：Accepted implementation contract，2026-10-21（[#171 P27-21/P27-22](https://github.com/ZombieIce/PrajnaQuant/issues/171)；实现票据 [#252](https://github.com/ZombieIce/PrajnaQuant/issues/252)）。仅为 MVP-6 新路径增加 `szse-derived-open@3`；@1/@2 和 `szse-etf-frozen@3/@4` 的既有身份与行为不变。

## 背景

正式 `PitContext::build` 之前没有真实 Dataset 开盘证据生成器；@2 也只接受 `szse-etf-frozen@3/@4`，无法承载含公司行动表的 @7。#234 因而无法使用统一的 PIT 构建路径生成真实数据 UNKNOWN 覆盖报告。该 ADR 定义新的 opt-in 状态语义与 @7 绑定方式，不宣称公告集合完整、实盘可成交或真实总回报已验证。

## 决定

### `szse-derived-open@3`

- `semantic_version` 显式为 `szse-derived-open@3` 才选择新语义。省略/null 的 @1 和显式 @2 行为及身份保持不变。
- 有正成交日线 Bar、正数 TDX `bar_sources.reference_price`、开盘前已知且适用于该生命周期/日期的官方限价规则，并且 `Bar.open()` 位于规则限价范围内，才为 `TRADABLE`。`Bar.open()` 是当前数据粒度可用的执行价格，保存为 `MarketCell.execution_price`；开盘涨停/跌停方向门禁继续生效。
- 开盘停牌的 `morning`/`open` 事实不推翻同日正成交：保持 `TRADABLE`，记录 `official_opening_halt` 和 `execution_at_first_trade`。已在开盘前公布的 `full_day` 停牌且没有正成交为 `HALTED`；同日公布且无正成交时无法证明开盘状态，保持 `UNKNOWN`。`full_day` 与正成交冲突时 `UNKNOWN`。没有 Bar 且没有停牌事实时 `UNKNOWN`。
- 不要求 `announcement_coverage` 或 `source_independence`。规则未知、参考价缺失、无正成交、开盘越界、公告与成交冲突、可用时刻未知等均 fail closed。停牌金额为零只用于明确 HALTED 格。
- 行情状态的可用时刻不早于该 Session 收盘；OpeningEvidence 的 `market_available_at` 设置为 Session 收盘，推导格使用其与收盘时间的较晚值。不使用本地 `observed_at`。事件发布时间取 citation 所验证 Raw 的公告日期；只有日期的引用保存为 `DateOnly`，同日公告不能算作开盘前已知。

### 本机开盘证据生成器

生成器从已构建 Catalog、冻结的行情 Dataset、公开 `configs/mvp-6/limit_rules_v2.json`、本机 `events.csv` 与对应事件的 citation/Raw 引用生成稳定排序的 MarketEvidence。限价规则使用 `RuleBinding` 绑定到具体生命周期；未能解析的规则留空。`suspension/morning` 与 `suspension/open` 映射到 `OpeningOnly`，`suspension/full_day` 映射到 `FullSession`。正式终止由 Catalog 生命周期处理；未知类型、无效时段、覆盖外事件或缺失/不一致引用报错。命令将输入 evidence 与 UNKNOWN 覆盖报告写到本机 acceptance 目录；不提交数据文件。

### `szse-etf-frozen@7` 行情父数据绑定

本 ADR 选择让 @3 接受 @7，而不让 PitContext 增加第二个 DSV 参数。@7 Raw provenance 必须唯一给出 `market_dsv`。该 DSV 必须是规范配置的 `szse-etf-frozen@4` 并包含 TDX 代码表。构建时先用 Manifest logical hash 校验父 DSV 的行情表，再以父 DSV 配置重放 @7 Normalizer；`instruments`、`sessions`、`bars`、`source_crosscheck` 和 `bar_sources` 的每个 @7 行必须与父表中同主键的行一致。@7 可只包含 PIT 路径选定的 ETF 子集，父行情表也可含其他品种；缺失父行、字段不一致、schema 不兼容或父表 hash 不符均拒绝。公司行动表继续由既有 TRI 构建器读取，因此行情校验和公司行动核算仍共用 `PitContext::build`。

## P27-22.5 执行披露

1. `Bar.open()` 作为日线记录的首笔成交价。开盘停牌时首笔成交可能发生在复牌后；没有分钟数据，无法确认其具体时刻。
2. 账户模型以 Session 为结算粒度，未建模同一 Session 内卖出与买入的资金先后。卖出款可能在现实到账前被用于当日买入。
3. 假设开盘前提交的订单会参与复牌后的交易。该假设尚未由深交所规则或队列数据验证。

## 验证边界

合成端到端测试通过 @7 Dataset 和 `PitContext::build` 检查状态、执行价、收盘可用时刻、含公司行动路径和确定性。#224 真实 Dataset 复核结果仅留在本机；公开仓库和 PR 不含行情值、Raw、hash、覆盖派生数值或采集日志。该复核不证明真实来源完整性、官方队列成交或收益正确性。
