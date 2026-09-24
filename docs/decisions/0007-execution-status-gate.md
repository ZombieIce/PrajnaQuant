# ADR 0007: 新版 ETF 快照按明确交易状态控制成交

- Status: Accepted
- Date: 2026-09-24

## Context

仓库已有 `core.security_status_revision` 和 `research.daily_bar_execution`，但 ETF 研究快照和回测此前只使用价格 bar。停牌或未知交易状态无法阻止买卖，目标买腿缺行情也没有独立原因记录。

## Decision

新版 ETF Parquet 快照按证券和交易日附带合并后的 `trade_status`、`is_tradable` 与来源列表。多个来源时，所有来源都必须明确为 `TRADABLE` 才标记可交易；冲突记为 `CONFLICT` 并拒绝执行。回测门槛同时要求状态字符串 `TRADABLE`、布尔值 true、非空来源；任何自相矛盾或无来源的状态也拒绝。新版快照状态缺失、`UNKNOWN`、`HALTED` 或冲突时不成交。

需要卖出的旧持仓缺当日 open 或不可交易时，整次调仓延期，不执行任何腿并保留待执行目标；可执行时仍先卖后买。目标买入腿缺 open 或状态不可交易时单独跳过，报告记录 `decision_date`、`attempt_date`、标的、方向、可计算的目标数量、拒绝原因、状态和来源。状态表的可交易标记只处理证券/日期维度，不实现买卖方向相关的涨跌停、成交量、排队或部分成交规则。

没有 `trade_status`/`is_tradable` 列的旧 Parquet 快照保持旧 bar-only 成交行为以兼容既有结果；从旧快照生成的新回测在 `execution_status_mode` 明确记录 `legacy_bar_only`，带状态列则记录 `status_gated`。旧报告的模式字段为 null，不能因默认空 `unexecuted_orders`/`rebalance_deferrals` 声称旧规则下无异常。报告新增逐仓 `mark_date` 与 `stale_calendar_days`，旧报告缺字段时保持 null。

## Consequences

缺状态的新快照会 fail-closed，可能让回测无交易，直到状态资料补齐。报告依其 snapshot hash 固定状态输入，但当前库内状态源的历史完整性和可知时点尚未证明；状态 `observed_at` 是本地采集时间，不能充作历史公告/可用时间。本批真实五 ETF 锁定快照无状态列，复跑属于 `legacy_bar_only`，合成门槛测试不构成真实停牌验证。现有 `is_st` 与 `limit_rule_id` 不参与下单门槛；价格限制需另行实现及测试。

三 ETF 十日合成场景固定验证未知状态买单、停牌旧仓卖单的延期、更新目标恢复后成交、逐腿跳过审计、现金/持仓/NAV 守恒和交易成本；Parquet 单测覆盖新版状态列读取以及旧快照兼容。
