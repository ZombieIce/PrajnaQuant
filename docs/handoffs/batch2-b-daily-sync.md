# Batch 2 B：日频增量同步与快照发布

核对时间：2026-09-25 13:31 CST。代码、测试和有限样本完成；生产回填未验。

## 已实现入口

仓库写入入口在 `src/main.rs`：

- `sync-daily --symbol <sh/sz代码>... --start YYYY-MM-DD --end YYYY-MM-DD [--lookback-days 10] [--retries 2]`：按显式证券列表顺序同步，请求范围从 `start - lookback_days` 到 `end`；每次新任务仍会重取该修订回看范围。
- `publish-sync --job-id <UUID>`：重试已成功入库任务的审计/发布，不重新请求数据。
- `sync-daily-latest --symbol ...`：调度适配入口，要求 SZSE 日历已覆盖当前日期，再选择最近确认开市日。当前日只有在日历确认开市且上海时间不早于 18:30 才可同步；日历超过 7 天则停止。
- `import-status-evidence --source ... --source-url ... --input status.csv --coverage coverage.csv`：统一写入 A 的状态事实、来源、原文 hash、公开/可用/采集时间、验证标记、coverage 声明和引用。旧 `import-status-csv` 继续兼容。

典型的日更调用：

```sh
cargo run -p ashare-warehouse --release --locked -- \
  --data-dir data-core sync-daily-latest \
  --symbol sh600519 --symbol sh510300 --lookback-days 10 --retries 2
```

日期范围调用：

```sh
cargo run -p ashare-warehouse --locked --offline -- \
  --data-dir /private/tmp/pq-b-test sync-daily \
  --symbol sh600519 --symbol sh510300 \
  --start 2026-09-24 --end 2026-09-24 --lookback-days 3 --retries 2
```

通过时会在 `<data-dir>/snapshots/<snapshot_id>/` 发布不可变目录，包含 `daily.parquet`、`securities.json`、`trading_calendar.txt` 和 `manifest.json`。目录完成校验后重命名，再原子替换 `snapshots/current.json`。manifest 记录数据/目录/日历 hash、schema、来源尝试和原文 hash、源 run id、请求/截止范围、覆盖审计、价格口径及快照 ID。每个 Parquet 行保留源 revision id/hash、实际来源、观察时间，以及状态来源/revision/hash、source ref、raw hash、published/available/observed 时间、coverage ref 和 UNKNOWN/CONFLICT 语义。

只读进程用 `resolve_published_daily_snapshot(data_dir, Some(snapshot_id))` 固定指定版本，或传 `None` 在请求开始时固定 current 指针；该读取不打开 DuckDB。并用已导出的同快照 `securities.json` 目录解析证券，不要自行从名称/代码推断资产分类。没有可信目录事实时 manifest 保留 `instrument_id=null, asset_type=UNKNOWN`。

## 幂等、恢复与发布保护

- 相同有效行 hash 不增加 revision；源内最新 revision 继续由 `staging.daily_bar_latest` 决定，快照跨来源按 Tencent、TDX 优先级选择，并稳定排序。
- 新任务重取显式范围和回看窗口，历史修订可被发现，不依赖最后日期水位。失败或进程中断留下 job/cursor/attempt；相同输入再次运行会续用 RUNNING/FAILED job，跳过已成功证券，继续未完成证券。每次触发重试有上限 5 次且指数退避上限 2 秒；空响应单独记录，不作为完整覆盖。
- 当前日未满足日历/18:30 发布门槛会记录 `NOT_PUBLISHED` job/attempt；行情入库成功但快照审计有缺口会记录 `AUDIT_FAILED`，补齐日历/数据后可用 `publish-sync` 重做审计发布。
- 每个请求保留 attempt 状态、raw path/hash、错误类别、行数、新 revision 和重复数。成功但 coverage 缺口时保留审计结果，不发布。
- 覆盖仅声明为“显式证券 × 已确认官方开市日”的范围结论；完整月历缺失、证券日线缺口、闭市日/无日历日期行或 OHLC/成交量单位异常都会阻断发布。它不声称全市场完整。
- 模拟审计失败以及原子目录重命名后、current 指针替换前中断；两种情况均验证最后成功快照仍可由 ID 读取。旧目录不删除或覆盖。
- `Warehouse::open` 继续用 `.writer.lock` 排他锁；隔离测试里第二 writer 打开同一路径失败。读者经快照解析器运行，不连接正在写入的仓库。

## A/C 接线

- A：通过 `import-status-evidence` 由同一仓库 writer 导入；status CSV 可带 `source_ref,published_at,available_at,verification_status,coverage_ref`。timestamps 要带时区。coverage CSV 字段为 `symbol,coverage_start,coverage_end,declared_coverage,verification_status,source_ref,detail`。来源缺席保留 NULL、`UNKNOWN`、`status_covered=false` 和 `unverified`，绝不自动提升为 TRADABLE。
- C：读取当前/指定快照应先调 `resolve_published_daily_snapshot`，随后从固定的 `daily.parquet` 分页读取；证券目录用同目录 `securities.json`。行含原始未复权 OHLC、股/份成交量、可空成交额、source、observed_at 和 revision hash。续页固定同一 snapshot ID/hash。旧 ETF 快照 reader 不变。
- 公共 `data.rs`、API 路由、STATUS/HANDOFF 和 Batch 2 公共契约未在本分支改动；由统筹安排公共接线和文档汇总。

## 任务进度与样本范围

- 2026-09-25 13:31 CST 检查时，生产 `data-core/.writer.lock` 存在（文件 mtime 2026-09-21 22:26 CST），`market.duckdb` mtime 为 2026-09-24 20:19 CST；`lsof data-core/market.duckdb` 未返回持有者。没有打开或复制生产库。当前生产 writer 是否仍持锁、`ops.history_backfill_window` 的任务计数/游标/最大完成日均 **Unknown**；未采用旧日志百分比。
- 隔离真实样本：目录 `/private/tmp/pq-b-real-sample-v2`；官方 SZSE 2026-09 月历 30 天；Tencent 股票 `sh600519` 与 ETF `sh510300`，2026-09-21 至 2026-09-24（3 日回看），首轮读 8 行、新增 8 revisions，最终冻结 2026-09-24 两证券各 1 行，覆盖审计 0 缺口、0 异常。
- 无变化重复请求：读 8 行、新增 0 revisions、重复 8 行；Parquet SHA-256 保持为 `63ab4d0b67def0b2ec02e7c5f0d1921138e277dda1e400c0fef962663e60624a`。当前验证快照 ID `56a22581-b3e1-49c8-9e32-89c754b154e8`；manifest SHA-256 `a436483185e0248a28d5f7b91a118534632b500754573a9363aa67c7a5042484`。之前的成功快照也保留在独立 ID 目录。manifest 明确报告 2 个 UNKNOWN 状态日。
- 样本目录没有导入生产证券主目录，因此冻结目录对两项均明确保留 `asset_type=UNKNOWN`、身份/名称为空；这是隔离输入的已知边界。真实行情样本成功不等于生产验证、全市场同步或资产分类认证。

复现真实样本（网络请求走 Tencent 与深交所官方日历；目标 data-dir 必须隔离）：

```sh
cargo run -p ashare-warehouse --locked --offline -- \
  --data-dir /private/tmp/pq-b-real-sample-v2 fetch-calendar --year 2026 --month 9
cargo run -p ashare-warehouse --locked --offline -- \
  --data-dir /private/tmp/pq-b-real-sample-v2 sync-daily \
  --symbol sh600519 --symbol sh510300 \
  --start 2026-09-24 --end 2026-09-24 --lookback-days 3 --retries 1
```

生产回填没有被中断；没有生产库写入或生产快照发布。

## 项目内调度（尚未启用）

本环境为 macOS，提供 LaunchAgent 渲染脚本和日同步 wrapper：

1. 将 `configs/daily-sync.symbols.example` 复制为 `configs/daily-sync.symbols` 并人工编辑小规模证券名单。
2. 手动运行 `sh scripts/daily_sync_schedule.sh`，验证当日的明确名单及发布输出。
3. `sh scripts/install_daily_sync_launchd.sh` 只生成 `~/Library/LaunchAgents/org.prajnaquant.daily-sync.plist`，不会加载它。
4. 审查 plist 后，如统筹决定启用，再执行 `launchctl bootstrap gui/$(id -u) ~/Library/LaunchAgents/org.prajnaquant.daily-sync.plist`；停用用 `launchctl bootout gui/$(id -u) ~/Library/LaunchAgents/org.prajnaquant.daily-sync.plist`。

调度为周一至周五 19:30（本机时区），不会以聊天自动化替代项目任务。当前没有符号配置文件、没有渲染或 bootstrap LaunchAgent；生产调度未启用。

## 验证

- `cargo fmt --all -- --check`：通过。
- `cargo test --workspace --locked --offline`：通过；仓库 14、研究 55；1 个依赖本地 ETF Universe 的既有集成测试 ignored。
- `cargo clippy --workspace --all-targets --locked --offline -- -D warnings`：通过。
- `sh -n scripts/daily_sync_schedule.sh scripts/install_daily_sync_launchd.sh`：通过。
- 隔离真实样本日历 + 股票/ETF 首次同步 + 无变化重复同步 + 两次 snapshot hash/ID 验证：通过。
- 生产数据库写入、当前回填任务状态、生产调度运行：未验证。

## 统筹合并建议

- STATUS/HANDOFF：记录生产回填进度 Unknown；实现状态写成“显式范围增量/回看、隔离真实样本及不可变发布通过，生产未验；自动调度模板未启用”。
- 数据模型文档：记录 migration 007 的任务/attempt/coverage/snapshot 表；status evidence 新来源时间字段；快照数据、目录、日历 sidecar 的 hash 和 current 指针。
- C 集成：在只读 handler 固定当前/指定 snapshot ID 后用同目录 Parquet 与证券目录；处理 UNKNOWN 分类，并展示原始未复权口径与数据截止日。
- 唯一建议下一步：统筹在生产 writer 停止且单写者窗口获确认后，读取当时的 backfill job/window 记录，再决定是否安装并启用本机调度。
