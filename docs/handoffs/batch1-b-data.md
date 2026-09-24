# Batch 1-B：证券目录与通用日频快照交接

审计时间：2026-09-24 20:49 CST。实现范围仅含 `crates/quant-research/src/data.rs`、该模块测试及 `examples/audit_daily_snapshot.rs`；没有改 `src/`、`sql/schema.sql`、`server.rs`、前端或共享回测契约。现有未提交改动均保留。

## 回填状态核验

- 已记录的目标是当时本机观察到的 6,882 只 SH/SZ 股票候选、已分类股票及 ETF，按 2016-01-01 至当日切成 800 自然日请求窗，共 34,410 窗口；数据源腾讯日线。范围是本机观察目录，不含未观察到的退市证券，不含北交所完整覆盖保证。
- **本次窗口任务状态：未验证。** `ops.history_backfill_window` 中 SUCCESS/EMPTY/FAILED 数、未记录窗数及实际完成水位需要 DuckDB 读取。本轮发现 `data-core/.writer.lock` 文件，底库约 2.84 GB，最近修改时间 2026-09-24 20:19 CST；`lsof` 未返回占用者，但系统拒绝进程枚举（`ps` operation not permitted、`pgrep` 无法取得 process list）。无法证明没有 writer，故未连接或复制生产库，也未触碰锁。当前完成/失败/空窗口数、最新任务截止日期与状态证据时间均为 **Unknown**。
- HANDOFF 旧记录中 2026-09-23 用户终端曾报告 16,775/34,410；这是历史日志，不能作为本次进度。旧记录还含单证券及五证券成功样本，均不代表当下整体窗口状态。
- 可安全读取的既存研究 ETF 快照 `research-output/snapshots/fe1dd8a3-d227-4e06-bb98-f0a6b3be0f34/` manifest 时间为 2026-09-24 19:20:36 CST：1,441,506 行、1,674 个 ETF、首末行情日 2016-01-04 至 2026-09-24；行情文件 SHA-256 `8c8debb7e1fdd7b3b592cc0991e807b26f1e94a59fb7bbe7e66e00f3030af872`。它是 ETF 快照计数，不是窗口任务状态，不证明股票回填进度或市场完整性。

## 实现接口

### 证券目录

```rust
data::load_security_directory(database: &Path) -> Result<Vec<SecurityDirectoryEntry>>
```

从 `core.instrument` 和当前符号/分类证据视图读取 instrument id、canonical symbol、code、原始名称、SH/SZ/BJ、现有 `asset_class`、名称及分类来源、首次/末次观察日。上市/退市日期固定返回 `None`，因为现有 schema 没有可用证据。`EQUITY_CANDIDATE` 与 `UNKNOWN` 原样保留，不按代码段推断证券名或类型。该读取接口是当前观察目录，不是历史证券名录或 PIT universe。

### 通用不可变快照

```rust
data::create_daily_snapshot(
    database, output_root, &["sh600000".into(), "sh510300".into()],
    preheat_start, research_start, research_end,
) -> Result<DailySnapshotManifest>
```

只接受调用方显式列出的、已在 `core.instrument` 观察到的代码；区间要求 `preheat_start <= research_start <= research_end`。Parquet 同时包含预热与正式区间行，以 `is_preheat` 区分，并保留身份、当前资产分类、OHLCV、可空 amount、选中来源、bar `observed_at`、执行状态来源/状态采集时间与覆盖标志。缺失字段继续为 NULL；状态缺失保持 status/source NULL、`covered=false`、`is_tradable=false`，false 仅表示没有已确认可交易证据，不代表 HALTED。每证券/日期依序优先腾讯、TDX、其他源，同源选最新 `observed_at`，最终 revision id 作稳定并列裁决。

manifest 顶层包含旧 `SnapshotManifest` 字段，并附 `schema_version=1`、显式 securities、资产类型、预热/研究日期及日历身份；主文件和日历 sidecar 有 SHA-256。日历只导出仓库中来源为 SZSE 且自然月完整的日期；没有覆盖的月份不使用工作日推测。`verify_snapshot_manifest(&manifest.base)` 验证主文件、可选日历和基准 hash。

共享只读函数：

```rust
data::load_daily_research_rows(snapshot, symbols, start, end)
data::load_snapshot_execution_state_inputs(snapshot)
data::load_security_directory(database)
data::audit_daily_market_coverage(snapshot, Some(calendar), symbols, start, end)
```

`ExecutionStateInput` 的最小状态字段为 instrument、symbol、trade_date、可空 status、source、covered、`status_observed_at`。后者是本机采集时间，不能解释成历史公告/公开时间。Agent A 可将 `status`、`source` 映射到其执行 gate；`covered=false` 或 status 缺失都应视为 UNKNOWN。`research.daily_bar_execution` 的现存定义见下节。

### ETF 兼容边界

旧 `create_etf_snapshot` 与旧 Parquet 结构保持不变；`SnapshotManifest` 未加必填字段，旧 manifest 仍能反序列化。`load_bars` 对无 `asset_type` 的历史 ETF Parquet 保持兼容；当新快照带 `asset_type` 时，只接受所有行都明确为 ETF，遇到股票、候选或未知分类会拒绝，避免旧 ETF runner 意外读取股票。通用行应使用 `load_daily_research_rows`，目前不开放股票回测能力。

## 覆盖审计与有限真实样本

审计检查指定证券的重复日期、OHLC 非正/越界/空值、官方开市日缺口、官方闭市日行情、首末日期及可用时的来源缺值。官方日历不完整时只比较已确认开市日，并返回缺日清单和 `unverified`；无独立日历时观察日期并集只是诊断，不能证明所有证券共同完整。无逐行 source 列时审计也返回 `unverified`。

复现命令：

```bash
cargo run -p quant-research --example audit_daily_snapshot --locked --offline -- \
  --snapshot research-output/snapshots/fe1dd8a3-d227-4e06-bb98-f0a6b3be0f34/etf_daily.parquet \
  --calendar research-output/snapshots/fe1dd8a3-d227-4e06-bb98-f0a6b3be0f34/trading_calendar.parquet \
  --symbols sh510010,sh510300,sz159919,sh518880,sh510050 \
  --start 2025-09-01 --end 2025-09-30
```

审计所得：保存的官方 SZSE 月历覆盖 2025-09-01 至 2025-09-30 共 30 个自然日，22 个开市日；5 只证券各有 22 行，首末日分别 2025-09-01/2025-09-30；无重复、缺开市日、闭市日数据或 OHLC 异常。SH/SZ 共用日历的独立交叉核验未做。审计 `status=unverified`，原因是这份旧 ETF Parquet 未导出逐行行情 source，故本次真实样本的来源缺口 **未验证**。manifest 的选择规则写明 Tencent 优先 TDX，但不能用该规则反推逐行实际来源。范围只覆盖这 5 个 ETF 的 1 个月，不说明更广市场完整。

## 执行状态事实（提供给 Agent A）

`sql/schema.sql` 中 `research.daily_bar_execution` 是：`staging.daily_bar_latest b LEFT JOIN core.security_status_latest s ON symbol 相同且 effective_date=trade_date`，输出 `s.trade_status/is_st/limit_rule_id`，并用 `coalesce(s.trade_status='TRADABLE', false)` 生成 `is_tradable`。状态最新视图按 symbol/date/source 保留每源最新 observed revision；多来源会使该 research view 对同一 bar 多行，且 `is_tradable=false` 本身不能区分未知和停牌。通用快照将日状态按证券/日折叠：状态一致保留原值，冲突置 `CONFLICT`，保留来源合并及最大采集时间；无状态行时 status/source 为空且覆盖为 false。没有状态源历史可知时点证据；`observed_at` 不是发布时间。当前可信历史状态、ST/限制状态覆盖和可知时间均 **未验证**。

Agent A 的输入建议：按 `(instrument_id, trade_date)` 读取 `ExecutionStateInput`；只在 `covered=true && status==Some("TRADABLE")` 作为可交易证据，其余状态按已有 gate 拒绝/延期并输出原因。不要把 UNKNOWN 转成 HALTED，也不要仅使用 `is_tradable=false` 推断停牌。股票执行规则/回测能力仍由 Agent A 的门槛控制。

## 测试与验证

新增合成 fixture 覆盖：混合股票候选与 ETF；UNKNOWN 分类、名称/amount 缺失；同源修订择优；预热和正式日期分段；缺失执行状态为 UNKNOWN；重复日期、共同缺日、非开市日、坏 OHLC、来源缺口；hash 篡改检查；旧 ETF bar-only Parquet 兼容；混合快照被 ETF-only reader 拒绝。

- `cargo test -p quant-research --locked --offline data::tests`：10 passed。
- `cargo test --workspace --locked --offline`：ashare-warehouse 10 passed；quant-research 46 passed；1 个需本地 Universe/快照的集成用例 ignored；doc tests passed。
- `cargo fmt --all -- --check`：passed；本轮只对 `data.rs` 和本批 example 单文件运行 rustfmt，没有全仓格式化写入。
- `cargo clippy --workspace --all-targets --locked --offline -- -D warnings`：passed。
- 真实快照审计命令如上成功；审计结果限定在说明范围。
- 生产回填表读取、真实股票通用快照创建及生产执行状态覆盖：**未验证**。

## 集成步骤与共享文档建议

1. Agent A 使用新执行状态输入或明确从混合快照读取状态字段；继续由回测层拒绝未启用的股票，不能扩大 runner 能力门槛。
2. Agent C/统筹可将 Universe 的明确 symbol 列表交给 `create_daily_snapshot`；先确认资产身份来自目录且 `asset_type` 原样保留，再对固定日历/范围跑审计并校验 manifest。
3. 后续搜索/K 线 API 可直接复用 `load_security_directory`、`load_daily_research_rows`，增加 API 外层的分页/范围限制；不要在 `server.rs` 再实现 SQL 身份映射或把 null 数值置零。
4. STATUS 建议记为“有显式证券列表的通用 immutable 日频 snapshot/data readers implemented；真实股票生产生成未验证；旧 ETF 路径兼容”；窗口进度仍 Unknown。
5. HANDOFF 应仅在 writer 已结束并允许单写者安全查询后，按 dataset/symbol/window 重新汇总 SUCCESS/EMPTY/FAILED/未记录数量和最大结束日；不要沿用 9/23 百分比。
6. API 契约建议记录证券目录字段和 K 线 reader 字段、null、来源、schema/manifest/hash及日期过滤；本批没有改 API。
7. 数据文档建议区分当前观察分类与可靠 listing/delisting，记下 `daily_bar_execution` 原 view 的一对多/未知语义，以及无完整日历时必须返回 unverified。

唯一建议下一步：待统筹确认 DuckDB writer 停止并恢复安全单读窗口后，查询 `ops.history_backfill_window` 做当前全量窗口盘点，再由统筹集成 STATUS/HANDOFF/API 文档并决定固定股票/ETF快照的调用入口。
