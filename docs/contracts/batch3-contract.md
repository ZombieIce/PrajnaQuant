# Batch 3 最小集成契约

状态：2026-09-26 冻结的接口边界；现有字段标为 Implemented，本批新增路由与作业字段在实际代码及测试接通前均为 Planned。第二批验收为部分通过，旧五 ETF 真实实验是 `legacy_bar_only`，全 UNKNOWN 实验仅证明拒单。

## 共同身份和时间

证券使用 `SH/SZ/BJ:code` 稳定 `instrument_id` 和 `sh/sz/bj` 加六位码的行情 `symbol`；资产类型从冻结目录读取，不按代码猜。交易日为 Asia/Shanghai 的 `YYYY-MM-DD`；请求起止日期含边界；时间戳用带偏移的 RFC 3339。T 日收盘形成信号，下一有行情日开盘尝试执行。`observed_at` 是本地采集时间，不得用作历史公开或可用时刻。历史 Universe、状态和价格都要分别保存版本与 hash。

## A：历史执行状态证据

- **Implemented 输入边界：** B 的 `import-status-evidence` 接收状态 CSV 与逐证券覆盖 CSV，单写者保存原文、`raw_path/raw_sha256`、`source/source_ref`、`published_at/available_at/observed_at`、`verification_status/coverage_ref`。状态事实含 `symbol`、`effective_date`、`trade_status`、`is_st`、`limit_rule_id`；覆盖含 `coverage_start/end`（闭区间）、`declared_coverage`、`verification_status`、`source_ref/detail`。
- **本批证据要求：** A 的来源审计保留原文文件及 SHA-256、证券范围、有效区间（事件采用半开边界，导入日行时展开）、盘中/全日范围、历史发布时间及在执行开盘前可用的证据、来源完整性声明及独立核验。按固定五 ETF × 官方预期交易日列出明确状态、冲突、缺失、原文和覆盖缺口，审计输出有内容 hash。二级报道或仅有事件列表的静默日不得推出全日 `TRADABLE`；盘中临停不得压成全日 `HALTED`。
- `UNKNOWN`、`CONFLICT`、缺原文/hash、未核验完整性、历史 `available_at` 未知或迟于相应执行时刻均不放行真实状态成交。证据不足时 A 交付缺口报告，P0-3 开放。A 不写生产 DuckDB；只有 B 的单写者可导入状态，或 A/B 在隔离库演示同入口。冻结研究输入必须保留状态原文/hash、覆盖和可知时刻身份，真实复跑与旧实验固定同一发布 Universe 版本及可比价格面板，披露成交/拒单/成本/NAV 与回溯静态、零分红、原始价假设。

## B：生产核验、同步和发布

- **Implemented 任务接口：** `sync-daily` 用显式 `symbol` 集合、含边界 `start/end`、`lookback_days/retries`；`ops.daily_sync_job/attempt` 记录 `job_id`、请求指纹、进度、尝试、状态、错误分类及原文路径/hash。重复相同源内容不增加有效修订；本任务 `EMPTY` 响应即使遇到旧 bar 也不能发布 `complete`。
- **Implemented 发布接口：** `publish-sync --job-id` 经日历、显式证券日期、OHLCV、重复及空响应审计，写 `<data-dir>/snapshots/<snapshot_id>/` 的 `daily.parquet`、`securities.json`、`trading_calendar.txt`、`manifest.json`，校验文件 SHA-256 后原子替换 `snapshots/current.json`。响应给 `snapshot_id`、manifest/data hash、`coverage_status`、`data_cutoff_date`；失败不切换指针，旧 ID 仍可读。`complete` 只证明该请求的价格覆盖，不证明状态、PIT 或全市场。
- **本批生产安全：** 先只读确认生产 writer、回填、锁和数据日期/身份；仅在能证明无并发写且由唯一单写者执行时才允许生产同步。不能证明时保留生产现状，在隔离 `data_dir` 做真实小样本、幂等、恢复、审计、发布及 API/页面验收，并明确“生产未验”。不删除锁、不终止回填、不覆盖旧快照。B 负责状态导入、行情身份和发布；C 只消费已发布不可变快照。发布指针变更不能让已打开查询或运行换版。
- 生产状态检查必须真正只读；现有 `ashare-warehouse status` 会进入带排他锁/迁移的 `Warehouse::open`，不能用于生产安全审计。证券目录从单写者核验后的 `core.instrument` 进入快照：`instrument_id`、`symbol`、`market/code`、`asset_class`、名称、来源引用/原文 hash/采集时刻可追溯；分类证据不足保持 `UNKNOWN`，不得依代码或 bar 猜股票/ETF。`first_observed_date` 不是上市日。隔离样本可以用同一入口导入有证据的少量身份，再经发布目录供 `/market` 真实页面验证。

## C：ETF 回测作业（Planned）

- `POST /api/v1/runs` 的请求体包含 `kind=etf_strategy`、`universe_id`、`version_id`、`snapshot_id`、完整 `ExperimentConfig` 与 `idempotency_key`。提交时规范化请求并保存 SHA-256；相同键和相同请求返回同一 `run_id`，相同键不同内容返回 409。成功受理返回 202 和固定输入身份。`GET /api/v1/runs/{run_id}` 返回持久化 `queued|running|succeeded|failed`、阶段、创建/更新时间、Universe 版本/content hash、研究快照 ID/文件 hash、配置 hash、实验 ID（成功时）及结构化 `error {code,message,details}`。进程重启后未完成任务有明确失败或恢复结果，不得永远显示 running。
- `POST /api/v1/runs/preflight` 可作为同请求的只读能力检查入口，返回具体阻断原因；它不创建作业，也不代替 `POST /api/v1/runs` 提交时重新校验。前端可以先展示 preflight 的门槛。
- 作业状态在研究输出目录持久化，使用原子文件替换或等效机制；实验沿现有 Rust runner 保存，不由浏览器计算绩效。运行只读已冻结研究快照和已发布 Universe 版本；提交和执行时核对 ID/hash，不打开生产写库。运行期间固定输入，不随新发布变更。完成后页面依据作业 ID 查询状态及结果，并显示假设/限制。
- 默认可信历史作业的门槛：ETF 资产范围、版本内容 hash 有效、日期成员和价格/日历覆盖满足明确能力检查、快照 manifest 与 Parquet hash 匹配、状态列及**独立核验的状态原文/覆盖/历史可用时刻证明**与快照绑定。仅有状态列或 `status_gated` 字符串不等于证据完整；旧 `legacy_bar_only`、全 UNKNOWN/冲突、回溯静态成员在未说明用途时不得被标作可信历史运行。无法满足时返回 422 和具体 `capability_not_ready` 原因，不落盘一个貌似成功的真实业绩。股票行情可查不开放股票回测；沪深300真实历史成员核验继续暂缓。合成可信 fixture 可用于测试，但不能给生产结果冒充真实来源。
- **Planned 资格侧车：** `research-output/snapshots/<snapshot_id>/run-eligibility.json` 由独立证据审计生成并经集成复核，绑定研究 `snapshot_id/sha256`、Universe `version_id/content_hash`、状态输入 hash、覆盖报告 hash；逐证券×预期开市日必须有全日状态、原文路径/hash、来源完整性核验和历史 `available_at`（不晚于相应执行日上海时间 09:30）。作业在提交及执行时重新校验侧车引用、Parquet 状态与覆盖；缺侧车、UNKNOWN/冲突、缺原文或可用时刻都返回具体 422 阻断。侧车本身的声明不能取代被引用原文和覆盖证据。A 当前 `status-snapshot-v1.json` / `status-coverage-report.json` 为 `gaps`，不可生成可放行的真实侧车；测试可使用明确的合成 fixture。

## 文件所有权与接线

| 负责人 | 独立文件 | 公共接线 |
| --- | --- | --- |
| A | 新状态来源/审计脚本、独立复跑工具与测试、`docs/handoffs/batch3-a-*.md` | 不改生产仓库/共享 Rust 文件；状态 CSV/覆盖文件交 B 导入；需要研究快照改动先向集成方提交接口 |
| B | 仓库同步/发布/状态导入、迁移与测试、`docs/handoffs/batch3-b-*.md` | `src/lib.rs`、`src/main.rs` 和仓库迁移由 B 唯一修改；与 C 共享发布 manifest 字段通过本契约协调 |
| C | 独立 `run_jobs` 模块、页面组件及测试、`docs/handoffs/batch3-c-*.md` | 研究 `server.rs`、`main.rs`、`data.rs`、`runner.rs`、前端路由入口由 C 作为本批唯一实现者接线，先说明变更范围并由集成方复核；不改仓库写者文件 |
| 集成方 | 本契约、共享 `STATUS.md`、`HANDOFF.md`、`priorities.md`、路线/API/架构/时间/数据文档、必要 ADR、最终验收 | 复核所有公共文件、解决跨链冲突及最小修复；Agent 只写自己的交接，不改共享状态文件 |

接口冻结后 A 可继续找原始状态资料，B 可在隔离库验证行情与发布，C 可用合成可信 fixture 验证作业及 UI。A 的真实来源缺口不阻塞 B/C 的独立实现，但不提高任何真实历史结果的可信级别。
