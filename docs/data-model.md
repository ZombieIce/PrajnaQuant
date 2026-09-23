# 实际数据模型

当前 schema 以 `sql/schema.sql` 为准。DuckDB `ops.schema_version` 写入版本 1–4，但没有独立迁移脚本，版本表不等于每版可回放迁移。研究实验、因子、组合数据保存在文件/内存，不在 DuckDB 建表。

| 概念 | 实际存储/类型 | 现状与缺口 |
| --- | --- | --- |
| Instrument | `core.instrument`（`instrument_id`, market, code, **当前** asset_class, first/last_observed_date）；`instrument_symbol_revision`、`instrument_classification_revision` | 有代码和分类修订；无真实上市/退市日期和历史 ETF 全集。ETF 识别用 TDX 名称含 ETF 且符合沪 `5*`/深 `15*`；主表当前分类过滤历史 |
| Market Data | `staging.daily_bar_revision` 原始价、volume_shares、amount_cny、source、observed_at、run_id、hash；`daily_bar_latest`；`research.etf_daily_bar`；`research.daily_bar_qfq`、`daily_bar_execution` | 多源修订/不复权日线；研究快照取腾讯优先。`research.etf_daily_bar` 不使用状态/复权视图。价格是原始价，收益不是 ETF 总回报 |
| Factor | Rust `SignalDefinition`、`FactorObservation`、`FactorReport` JSON | 无数据库因子表、通用版本化注册或因子值缓存。`forward_return` 是评价标签 |
| Signal | `signal.rs` 的 key/定义和报告；策略轮动分数为内存 `HashMap<(date,symbol), f64>` | 无持久信号事件/独立 signal_time |
| Position | `backtest.rs::Account.holdings` 证券→整数数量；结果只保存 `final_positions` 和每日仓位**数量** | 无每日证券级持仓/权重序列 |
| Trade | `backtest.rs::Trade`，date/symbol/side/quantity/reference/fill/gross/commission/tax/slippage | 有模拟成交，未建独立 order/fill 表、失败或部分成交记录 |
| Portfolio | 运行时 `Account.cash`、`EquityPoint` 每日 equity/cash/positions/drawdown | 无可审计的每日 market value/每仓估值价；无外部现金流 |
| Backtest Result | `research-output/experiments/<uuid>/experiment.json` | 完整配置、snapshot manifest、因子报告、backtest 报告、假设；没有 strategy_id/version 字段 |
| Experiment | `ExperimentConfig`、`ExperimentResult`；批量 `batches/<uuid>/batch.json` | UUID、created_at、`engine_version`、配置、快照 hash；结果文件不是数据库实验表。无统一策略/因子版本及运行环境锁定摘要 |

`ops.ingest_run`、`quality_issue`、`dataset_release`、`market_daily_coverage`、`history_backfill_window` 管理数据血缘、基础覆盖/发布与回填。`core.trading_calendar_revision`、`adjustment_factor_revision`、`security_status_revision` 已存在；不能推断这些数据已进入 ETF 回测。`data-core/` 与 `research-output/` 被 Git 忽略；本机样本存在不等于可迁移的新环境数据。

当前无基本面事实表、指数历史成分表、ETF 分红/份额变更表、策略配置表、每日目标权重/持仓明细表。旧 `docs/A股本地数据库建设方案.md` 的逻辑模型是路线图，不能当已实现 schema。
