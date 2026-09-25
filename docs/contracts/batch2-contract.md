# Batch 2 最小公共契约

状态：2026-09-24 冻结的集成接口。`Implemented` 表示 Batch 1 代码中已存在；`Planned` 表示本批拟新增，完成状态须由集成验收按代码与测试更新。本文件是 A/B/C 的唯一公共字段与接线约定；未列为已实现的能力不能提前在报告或页面中声称可用。

## 1. 身份、日期与时间

| 字段 | 约定 | 当前依据 |
| --- | --- | --- |
| `instrument_id` | 稳定仓库主键 `SH:600000`、`SZ:159952`、`BJ:xxxxxx`；`market`/`exchange` 使用 `SH`、`SZ`、`BJ`。API 不再生成 `XSHG:code`/`XSHE:code` 作为同名主键 | `core.instrument`、`SecurityDirectoryEntry` 为 Implemented；现有 `/api/v1/instruments` ETF 建议项仍输出 `XSHG`/`XSHE`，由 C 在接线时统一，属 Planned 修正 |
| `symbol` / `code` | 行情内部和现有回测用小写 `sh600000`、`sz159952`、`bjxxxxxx`；`code` 为六位数字。请求优先用 `instrument_id`，保留 `symbol` 作追溯，不凭代码前缀推断资产类型 | Implemented |
| `asset_type` | 沿用 `core.instrument.asset_class`：`ETF`、`EQUITY`、`EQUITY_CANDIDATE`、`UNKNOWN` 等；查询响应原样给出。`EQUITY_CANDIDATE` 可供行情查询，但界面须显示“股票候选，分类未核验”，不能冒充已核验股票；查询可用不放开股票回测 | Implemented 类型，C 的跨资产查询为 Planned |
| `trade_date` | `YYYY-MM-DD`，Asia/Shanghai 的交易日期；含起止边界。时间戳用带偏移的 RFC 3339。Universe 决策截止为当日上海时间 15:00，T 收盘信号只可在下一可用日 open 模拟执行 | Implemented 时间模型 |
| `observed_at` / `available_at` | `observed_at` 是本地采集时间；历史来源公开/可用时刻须单独用 `published_at`/`available_at` 和来源证明。未知时为 `null`，不得回填为 `observed_at` | 前者 Implemented，后者用于 A 证据为 Planned |

当前 `first_observed_date`/`last_observed_date` 不是上市/退市日期；`listed_date`/`delisted_date` 未经可靠证据时保持 null。五 ETF 发布 Universe 版本固定为 `d8811237-6c37-4189-86b8-9b05fbccc405` / `3fcfed9c-b8c1-4cce-a933-bc2b79ebf0da`；成员仍为 `retrospective_static`，不能由状态证据自动提升 PIT 成员身份。

## 2. A：执行状态证据与冻结输入（Planned）

每条来源事实至少包含：`instrument_id`、`symbol`、`effective_from`、`effective_to`（半开区间；单日记录用次日作上界）、`trade_status`（`TRADABLE|HALTED|UNKNOWN`）、`source`、`source_ref`（URL/公告编号/文件定位）、`raw_path` 或可检索原文标识、`raw_sha256`、`published_at`、`available_at`、`observed_at`、`verification_status`、`coverage_ref`。不得从存在/缺失 bar 推出 `TRADABLE`、`HALTED`、上市或历史可投资性。来源没有可核验历史公开时刻时，`available_at=null` 且历史可知性为 `unknown`。

覆盖审计按 **证券 × 预期交易日 × 来源** 报告：预期日历身份、请求窗口、已证实状态日期、未知/冲突/缺失日期、原文与 hash、来源覆盖声明及独立核验结果。覆盖可为 `complete`、`gaps`、`unverified`；`complete` 必须有来源完整性证据，不能只由五只证券各有 bar 得出。多源冲突保留原事实并把合并执行状态置为 `CONFLICT`，不得任选其一。状态 UNKNOWN 和资料缺失均不可交易，但须分别保留原因。

现有 `core.security_status_revision`、`ExecutionStateInput` 和新版 ETF Parquet 状态列为 Implemented：状态、source、采集时间和覆盖布尔值已接通，`TRADABLE` 且有来源才放行。它们尚没有历史 `available_at`、原文/hash 与独立覆盖证明。A 的扩展事实由 B 的单写者入口导入生产库，或先在隔离库验证；A 不并发写生产 DuckDB。冻结快照应同时保存状态来源/审计身份以及未知值，运行报告继续保存 `execution_status_mode`、成交、拒单、延期、NAV、成本。真实复跑必须使用同一已发布 Universe 版本，明确对照 Batch 1 `legacy_bar_only` 实验 `4e076758-8069-4e0e-9269-1d3f9d574159`。若真实来源不足，交付可核验缺口表并保持 P0-3 开放。

## 3. B：增量任务与发布（Planned）

任务输入固定为 `dataset`、`source`、`trade_date` 或闭区间、显式证券集合/集合身份、来源请求参数、预期日历/市场范围；输出 `job_id`、请求指纹、进度游标（证券/日期/窗口）、尝试次数、开始/结束/更新时间、原文路径/hash、读入/新增修订/重复行数、质量审计身份、状态与错误分类。可恢复状态至少区分待处理、运行中、成功、失败、已发布；错误分类至少区分网络/限流、源数据格式、校验/覆盖、写库与发布故障。重试仅对可重试故障并保留原尝试记录；相同请求重复运行不得制造新的有效行。已有 `ops.ingest_run`、`history_backfill_window`、`daily_bar_revision` 和 `row_hash` 是基础，不把现有手动回填等同完整日更任务。

唯一仓库写者执行原文归档、修订去重及质量审计。`staging.daily_bar_latest` 的源内最新修订语义保留；跨源选择沿现有研究快照优先腾讯、再 TDX 的规则，修订来源可追溯。若预期交易日未收齐、官方日历未知、市场覆盖不足、任务失败或当天日线尚未完成，不能发布 `complete` 快照，也不能让它成为默认研究输入。当前 `publish_daily` 已要求成功 TDX run、深交所开市记录与 SH/SZ/BJ 覆盖门槛；完整性与未完成日判定及原子发布仍须本批验证/扩展。

发布产物为不可变目录中的 Parquet、manifest 与文件 SHA-256，发布记录同时给出 `snapshot_id`、`as_of_date`、`data_cutoff_date`、`coverage_status`、`price_adjustment`、源 run/revision、质量结果和创建时间。先写临时产物并校验，再原子公布 manifest/当前发布指针；失败仅留下失败任务，不替换旧指针。回测/查询在开始时固定 `snapshot_id` 与 hash，整个请求或运行不得随新发布悄悄切换版本。新快照需能按 ID 重新读取，旧快照不被覆盖。

## 4. C：只读证券与日线 API（Planned）

保留 `GET /api/v1/instruments?q=&asset_type=`；目前只查 ETF 旧快照且 `q` 至少 2 字、股票返回空，属 Implemented。C 扩成已发布股票/ETF 快照搜索：`q` 至少 2 字；`asset_type=etf|stock` 可选，其中 `stock` 可返回 `EQUITY` 和带候选提示的 `EQUITY_CANDIDATE`；`snapshot_id` 可选（缺省固定请求开始时最新已发布版本）；`limit` 默认 20、最大 100，`cursor` 用于下一页。响应 `items` 每项含 `instrument_id`、`symbol`、`code`、`name`、`exchange`、`asset_type`、身份来源与当前分类的非 PIT 提示，并含 `snapshot` 身份、`data_cutoff_date`、`next_cursor`。不存在或未发布的快照不得回退到任意最新文件。

新增 `GET /api/v1/daily-bars?instrument_id=&snapshot_id=&start=&end=&limit=&cursor=`。必须指定稳定 `instrument_id` 与闭区间 `YYYY-MM-DD`；`snapshot_id` 可省略但响应必须回显固定 ID/hash，续页必须继续同一 ID。`limit` 默认 250、最大 1000；稳定排序为日期升序，游标绑定快照、证券和范围，非法/跨快照游标拒绝。一次响应只返回完整日线，不按折线点抽样；十年数据按分页取得。每根 bar 含 `trade_date`、`open/high/low/close`（CNY/份或 CNY/股，`null` 表示缺失）、`volume_shares`（股/份）、`amount_cny`（人民币元，可 null）、`source`、`observed_at`；价格口径仅 `none`（原始未复权）。`qfq`/`hfq` 如未获得完整复权契约与证据则返回不支持，不默默变换。`start/end` 不得超过该快照已发布范围；预热行不得作为正式研究期数据展示。

错误统一使用现有 JSON 风格 `{ "error": { "code": ..., "message": ..., "details": ... } }`：参数/游标/不支持口径 400，不存在证券或快照 404，快照未发布或 hash 不符 409/503（依是否身份冲突或服务暂时不可用），内部错误 500；空日期范围在合法请求下返回空 `items` 而非伪造零价。具体错误代码由 C 测试锁定。页面同时展示股票与 ETF 日 K 线和成交量，标题/提示标明快照 ID、数据截止日、原始价格口径及非实时数据；必须保留 OHLC 高低点和原始成交量语义，不用线图抽样或平均价替代蜡烛线聚合。若未来增加周/月聚合，开=首、收=末、高=max、低=min、量/额=sum，并对缺失区间显式标注。

## 5. 兼容、来源与不完整数据

旧 ETF Parquet 缺状态列继续明示 `legacy_bar_only`；旧实验无执行模式为 `unknown`，不能由空拒单数组推断无异常。通用快照 `schema_version=1` 已存在，但尚非发布目录；旧文件只按其已有字段读取，不合成上市、来源、覆盖或 `available_at` 证据。新 manifest 扩展字段须有版本和向后兼容读取；若旧快照不满足新查询的发布/身份/单位门槛，API 给明确不可用错误。修订不覆盖原文，引用的原文/hash、源 run、状态审计与日历身份都能从发布身份追查。任何缺失、冲突和源覆盖未知均显示为 unknown/gaps，不能作为零价、停牌或完整日线参与默认研究。

## 6. 文件所有权与接线

| 负责人 | 独占修改范围 | 公共文件接线 |
| --- | --- | --- |
| A | 状态来源适配、原文/覆盖审计、独立验证与真实复跑工具；只写 `docs/handoffs/batch2-a-*.md` | 状态证据的新模块或迁移交 B 导入；`data.rs`、`runner.rs`、回测公共接线由集成负责人协调 |
| B | 仓库单写者、增量任务、进度与重试、质量审计、发布模块；只写 `docs/handoffs/batch2-b-*.md` | `src/lib.rs`、`src/main.rs`、schema/迁移由 B 主接线；涉及共享快照 manifest 时先按本契约提交接口 |
| C | 只读行情查询模块、证券/蜡烛图页面和测试；只写 `docs/handoffs/batch2-c-*.md` | C 在独立模块实现 handler；`server.rs` 路由、`crates/quant-research/src/lib.rs` 及公共 `data.rs` 由集成负责人接线/复核 |
| 集成负责人 | 本契约、共享类型和公共 CLI/路由冲突、`STATUS`、`HANDOFF`、优先级、路线、API/数据文档、必要 ADR 与最终验收 | 接收 A/B/C 已审查改动后统一解决公共文件冲突；不得清理或覆盖别人的未提交成果 |

交付顺序：A 可先审外部源及合成状态，B 可在隔离库用固定响应验证增量/幂等，C 可用合成发布快照完成 API/页面；共同字段固定后才接生产输入。C 只读已发布不可变快照；A 状态事实经 B 单写者或隔离库；B 发布不能改变已固定的查询或回测版本。

## 集成状态（2026-09-25）

- B 的任务、迁移 007、`snapshots/current.json` 发布目录和 `import-status-evidence` 已实现并在隔离库/小样本测试；生产库与自动调度未验。B manifest 使用 schema 7、`daily.parquet`、`securities.json` 和 `trading_calendar.txt`，不采用 C 最初的 `publication.json`。
- C 的两个 GET 路由、分页/原始 OHLCV、`/market` 已实现。集成方让服务 `--market-data-dir`（默认 `data-core`）解析 B 的 current 或指定 ID，调用 B 只读 resolver 复验 manifest、Parquet、证券目录与日历 hash；合成 schema 1 reader 保留。B 真实样本目录没有 `instrument_id` 时 C 不猜主键，搜索为空。股票/ETF 页面仅用合成发布快照完成浏览器验收。
- A 的来源审计与 UNKNOWN 占位复跑已实现，但 1205 格全未知，未取得官方逐日来源/完整性/历史可用时刻，未向生产库导入状态。P0-3 仍开放。最终证据与状态按 [`Batch 2 集成验收`](../handoffs/batch2-integration-acceptance.md) 为准；本契约前文的 Planned 项只在相应代码/测试列明的范围内转为 Implemented。
