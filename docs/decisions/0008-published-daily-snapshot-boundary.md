# ADR 0008：日频发布快照的只读边界

- 状态：Accepted for Batch 2 local implementation
- 日期：2026-09-25

## Context

仓库 DuckDB 由单写者同步，研究服务需要在增量写入期间查询股票/ETF 日频行情。B 初版把快照发布到 `<data-dir>/snapshots/current.json` 与不可变 ID 目录；C 初版只识别研究输出目录的 `publication.json`/schema 1，导致两条已独立通过的链无法接通。若服务每次翻页选择“最新”，发布瞬间可混合版本。

## Decision

1. B 的 `snapshots/current.json` 是本地正式日频发布入口；它指向不可变 ID 目录的 schema 7 manifest。目录内 `daily.parquet`、`securities.json`、`trading_calendar.txt` 和 manifest 都按 SHA-256 校验。仓库 writer 完成审计并校验产物后原子替换 current；旧目录保留。`complete` 只描述显式证券与确认日历的价格覆盖。
2. `quant-research serve --market-data-dir` 通过只读 resolver 固定 current 或显式 `snapshot_id`，不打开生产 DuckDB。响应返回 ID、数据/hash、截止日；续页必须继续同一 ID，避免查询和发布交错时换版。证券身份只来自同目录 `securities.json`；缺主键时不猜代码对应的资产类别。
3. C 原有 schema 1 `daily_research.parquet`/`publication.json` reader 仅为已有合成发布快照和兼容场景保留；正式 B 发布目录优先由 current 指针解析。旧 ETF 研究 Parquet 没有发布身份时不能冒充新行情 API 输入。
4. 新行情 API 仅返回原始未复权日频 OHLCV 和分页游标；浏览器不聚合/抽样成伪日 K。股票行情可查不表示股票回测能力已开放。

## Consequences

跨链 Rust 测试覆盖 B 格式 current、新旧 ID 固定查询和 OHLCV；合成浏览器覆盖股票/ETF 页面。隔离真实 B 样本没有证券主键/分类，因此生产股票查询仍未验。历史交易状态/PIT/分红门槛由既有 ADR 与 P0 列表继续约束。本 ADR 不定义远程部署、任务队列或服务器认证。
