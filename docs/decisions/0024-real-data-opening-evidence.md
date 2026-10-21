# ADR 0024：真实开盘证据与含公司行动 Dataset 的 PIT 路径

- 状态：Accepted implementation contract，2026-10-21（[#171 P27-21～P27-24](https://github.com/ZombieIce/PrajnaQuant/issues/171)；实现票据 [#252](https://github.com/ZombieIce/PrajnaQuant/issues/252)）。仅为 MVP-6 新路径增加 `szse-derived-open@3`；@1/@2 和 `szse-etf-frozen@3/@4` 的既有身份与行为不变。

## 背景

正式 `PitContext::build` 之前没有 Catalog/Dataset 开盘证据生成器；@2 也只接受 `szse-etf-frozen@3/@4`，无法承载含公司行动表的 @7。该 ADR 定义新的 opt-in 状态语义与 @7 绑定方式。#252 以合成数据验收；真实数据重跑由 #224 承担，不宣称公告集合完整、实盘可成交或真实总回报已验证。

## 决定

### `szse-derived-open@3`

- `semantic_version` 显式为 `szse-derived-open@3` 才选择新语义。省略/null 的 @1 和显式 @2 行为及身份保持不变。
- 生命周期内有正成交日线 Bar 即为 `TRADABLE`，`Bar.open()` 作为当前数据粒度可用的执行价格，保存为 `MarketCell.execution_price`。生命周期内没有正成交 Bar 为 `HALTED`，成交额为零，不计入 `UNKNOWN`；不读取或核对停牌事实。
- 限价规则与正数 TDX `bar_sources.reference_price` 均可信且在开盘前已知时，按 `open` 判定涨停/跌停方向门禁。规则未知时跳过限价判定、保持可交易并记录原因，不计 `UNKNOWN`。规则已知但参考价缺失属于数据缺陷，保持 `UNKNOWN`。
- 若任一 Bar OHLC 价格超出计算的涨跌停区间或未对齐 tick，跳过限价判定、保持可交易并记录原因，不计 `UNKNOWN`。其它有 Bar 的数据缺陷（例如已知规则下缺参考价、来源冲突或可用时刻未知）保持 `UNKNOWN`。P21 未解释跳变与未应用公司行动只保留为 TRI 诊断，不传给 UNKNOWN 覆盖评估。
- 不要求 `announcement_coverage` 或 `source_independence`。有 Bar 状态的可用时刻不早于 Session 收盘；OpeningEvidence 的 `market_available_at` 固定为 Session 收盘，推导格取不早于收盘的时刻。不使用本地 `observed_at`。

### 本机开盘证据生成器

生成器只读取已构建 Catalog、冻结 Dataset 日历和公开 `configs/mvp-6/limit_rules_v2.json`，通过 `RuleBinding` 为每个生命周期/Session 解析限价规则。无法解析时规则留空。每格的 `market_available_at` 固定为 Session 收盘；生成的 evidence 与覆盖报告只写到本机 acceptance 目录。生成器不读取 `events.csv`、停牌事实或引用索引。

### `szse-etf-frozen@7` 行情父数据绑定

本 ADR 选择让 @3 接受 @7，而不让 PitContext 增加第二个 DSV 参数。@7 Raw provenance 必须唯一给出 `market_dsv`。该 DSV 必须是规范配置的 `szse-etf-frozen@4` 并包含 TDX 代码表。构建时先用 Manifest logical hash 校验父 DSV 的行情表，再以父 DSV 配置重放 @7 Normalizer；`instruments`、`sessions`、`bars`、`source_crosscheck` 和 `bar_sources` 的每个 @7 行必须与父表中同主键的行一致。@7 可只包含 PIT 路径选定的 ETF 子集，父行情表也可含其他品种；缺失父行、字段不一致、schema 不兼容或父表 hash 不符均拒绝。公司行动表继续由既有 TRI 构建器读取，因此行情校验和公司行动核算仍共用 `PitContext::build`。

## P27-22.5 执行披露

1. `Bar.open()` 作为日线记录的首笔成交价。开盘停牌时首笔成交可能发生在复牌后；没有分钟数据，无法确认其具体时刻。
2. 账户模型以 Session 为结算粒度，未建模同一 Session 内卖出与买入的资金先后。卖出款可能在现实到账前被用于当日买入。
3. 假设开盘前提交的订单会参与复牌后的交易。该假设尚未由深交所规则或队列数据验证。

## 验证边界

合成端到端测试通过 @7 Dataset 和 `PitContext::build` 检查状态、执行价、限价方向、规则未知、缺少参考价、不可置信价格、收盘可用时刻、P21 诊断隔离和确定性。真实 Dataset 复核属于 #224；公开仓库和 PR 不含行情值、Raw、hash、覆盖派生数值或采集日志。合成验证不证明真实来源完整性、官方队列成交或收益正确性。
