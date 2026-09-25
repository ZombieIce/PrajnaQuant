# 实际数据模型

## Batch 2 新增（2026-09-25）

迁移 `007_daily_sync.sql` 增加 `ops.daily_sync_job`（请求指纹、显式证券/范围、游标、状态、错误类、审计）、`ops.daily_sync_attempt`（每证券/窗口/重试原文路径/hash/行数）、`ops.daily_snapshot` 与 `ops.daily_snapshot_current`（不可变发布身份/当前指针），以及 `ops.security_status_coverage`。`core.security_status_revision` 增加来源引用、原文路径/hash、`published_at`、`available_at`、验证状态与覆盖引用；`observed_at` 仍只是本地采集时间。A 尚未取得五 ETF 完整真实状态，不能由字段存在宣称已验证可交易。

B 的日频发布目录为 `<data-dir>/snapshots/<snapshot_id>/`：`daily.parquet` 保留日线修订/source/hash及可空状态证据，`securities.json` 冻结当前观察证券身份/分类，`trading_calendar.txt` 冻结已保存 SZSE 日期，`manifest.json` 记录 schema 7、请求/截止日期、覆盖审计与三个内容 hash；`snapshots/current.json` 持有 ID/manifest hash。先校验再替换 current，旧目录不覆盖。只读查询在开始时固定指针或显式 ID 并复验四个 hash，不读取正在写入的 DuckDB。隔离真实样本 snapshot `56a22581-b3e1-49c8-9e32-89c754b154e8` 截止 2026-09-24，显式两证券 2 行、行情覆盖 complete，但证券主键/分类均 UNKNOWN；这不是生产股票目录或状态完整性证据。研究侧旧 schema 1 `daily_research.parquet` 和 ETF `etf_daily.parquet` 保持只读兼容，不会自动升级为 B 发布身份。

`complete` 在 B 的审计中仅指显式证券×已确认深交所日历的价格行，状态未知数与冲突数另记，不能把它解释为全市场、PIT 成员、执行状态或分红完整。A 的 1205 个五 ETF 状态格均 UNKNOWN，占位回测单独保存，不导入生产状态库。详细限制见 [`Batch 2 验收`](handoffs/batch2-integration-acceptance.md)。

当前 schema 以 `sql/schema.sql` 和 `sql/migrations/` 为准。仓库打开时按幂等方式重放 schema，再应用版本化迁移；迁移 005 新增历史指数成分事实与独立覆盖修订表，迁移 006 增加退出公告时间和发布时间模拟方法。通用导入记录原文快照 SHA-256、来源修订 hash 与 observed_at，但强制成员状态为 `unknown`，并把来源声称的 `complete` 覆盖降为 `unverified`；只有后续来源专属验证通过，才能提升为 `verified_pit`/`complete`。目前已临时导入 `unliftedq/index-constitution` 的沪深300区间数据（覆盖声明 2016-01-01 至 2026-06-12）：`opt-in`/`opt-out` 的模拟公告时间分别取对应生效日提前 14 个自然日，存入 `published_at` 和 `effective_to_published_at`，方法标记为 `simulated_minus_14_calendar_days`。全部 1221 条成员区间仍为 `unknown`，覆盖为 `unverified`；4 条源行缺失 `opt-in`，没有猜测日期，并记录于覆盖缺口。原始 CSV、URL、抓取时间和 SHA-256 归档在本机 `data-core/raw/index-constitution/`。历史代码/名称经过项目规范化，不构成真实公告可知性或官方覆盖证明。研究实验、因子、组合数据仍保存在文件/内存，不在 DuckDB 建表。

| 概念 | 实际存储/类型 | 现状与缺口 |
| --- | --- | --- |
| Instrument | `core.instrument`（`instrument_id`, market, code, **当前** asset_class, first/last_observed_date）；`instrument_symbol_revision`、`instrument_classification_revision`；`data.rs::load_security_directory` | 有代码、分类修订和本机已观察目录；上市/退市日期保持 null，`EQUITY_CANDIDATE` 不猜成股票。无可靠历史 ETF 全集；主表当前分类过滤历史 |
| Market Data | `staging.daily_bar_revision` 原始价、volume_shares、amount_cny、source、observed_at、run_id、hash；`daily_bar_latest`；`research.etf_daily_bar`；`research.daily_bar_qfq`、`daily_bar_execution`；`create_daily_snapshot` | 多源修订/不复权日线；显式股票/ETF 列表可生成带预热、正式区间、逐行来源和状态采集时间的通用冻结 Parquet。新版 ETF 快照按证券/日期嵌入状态；仅明确 TRADABLE 且有来源放行。旧快照缺状态字段时明示 `legacy_bar_only`。本次真实五 ETF 快照属于旧格式；价格收益不是总回报 |
| Factor | Rust `SignalDefinition`、`FactorObservation`、`FactorReport` JSON | 无数据库因子表、通用版本化注册或因子值缓存。`forward_return` 是评价标签 |
| Signal | `signal.rs` 的 key/定义和报告；策略轮动分数为内存 `HashMap<(date,symbol), f64>` | 无持久信号事件/独立 signal_time |
| Position | `backtest.rs::Account.holdings` 证券→整数数量；结果有 `final_positions`、每日 `position_curve` | 逐证券数量、最近收盘估值价/日期、市值、陈旧自然日数和组合资金占用率已保存；无独立每日证券权重或持仓查询 API |
| Trade | `backtest.rs::Trade`，date/symbol/side/quantity/reference/fill/gross/commission/tax/slippage | 有模拟成交，未建独立 order/fill 表、失败或部分成交记录 |
| Portfolio | 运行时 `Account.cash`、`EquityPoint` 每日 equity/cash/positions/drawdown；`PositionPoint` 每日现金/逐仓市值 | 可逐日核对 `equity=cash+Σmarket_value`；无外部现金流 |
| Backtest Result | `research-output/experiments/<uuid>/experiment.json` | 完整配置、snapshot manifest、因子报告、backtest 报告、假设；没有 strategy_id/version 字段 |
| Experiment | `ExperimentConfig`、`ExperimentResult`；批量 `batches/<uuid>/batch.json` | 新 Universe-aware 实验可冻结版本/内容 hash、逐日成员 hash、来源修订、PIT/coverage 与 snapshot hash；旧实验字段可缺省并标 `legacy_universe_unknown`。仍是 JSON 文件，无数据库实验表/异步作业状态 |
| Universe | Rust `UniverseDefinition`；服务输出目录 `universes/<uuid>.json` | UUID 与不可变发布版本；手工成员可解析，指数事件 schema 已具备，但仓库历史来源 provider 未接入。配置单进程锁写入，serve 不写 DuckDB |

`ops.ingest_run`、`quality_issue`、`dataset_release`、`market_daily_coverage`、`history_backfill_window` 管理数据血缘、基础覆盖/发布与回填。`core.trading_calendar_revision`、`adjustment_factor_revision`、`security_status_revision` 已存在。快照只冻结生成时选择的状态修订；`status_observed_at` 是本地采集时间，不能证明历史决策前可知。状态行缺失保持 UNKNOWN/未覆盖，不是 HALTED。通用快照的 SZSE 日历旁车只导出完整自然月；`data-core/` 与 `research-output/` 被 Git 忽略，本机样本不等于可迁移数据。

当前无基本面事实表、ETF 分红/份额变更表、策略配置表、每日目标权重/持仓明细表。指数成员 CSV 已进入可追溯底座，但历史 provider 尚未连接到 Universe API，不能用于严格 PIT；`sh000300` 仍只是指数价格。旧 `docs/A股本地数据库建设方案.md` 的逻辑模型是路线图，不能当已实现 schema。
