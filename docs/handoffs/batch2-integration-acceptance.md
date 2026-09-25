# Batch 2 集成验收（2026-09-25）

**总评：部分通过。** A 的真实状态来源与历史可用时刻不足，P0-3 未关闭；B 的显式证券增量同步、隔离真实样本和不可变发布通过，生产库/调度未验；C 的固定快照证券与日线 API、股票/ETF 页面在合成目录通过，真实样本缺证券身份，生产股票行情未验。集成方修复 B 发布格式与 C 查询格式不兼容，并完成跨链测试。股票行情查询不开放股票回测。

## 工作区与集成修复

- 启动基线为 `main`、HEAD `f786606`，只有一个 worktree。A/B/C 在同一工作区交付未提交文件；集成方未 reset、clean、stash 或覆盖历史真实行情及实验产物。第一批真实实验 `4e076758-8069-4e0e-9269-1d3f9d574159` 仍为 `legacy_bar_only`。
- 三方各写独立交接：[`A`](batch2-a-execution-evidence.md)、[`B`](batch2-b-daily-sync.md)、[`C`](batch2-c-market-ui.md)。集成方负责本页、公共契约、共享状态文档与路由接线。
- B 发布 `data-core/snapshots/current.json` → 不可变 `daily.parquet`、`securities.json`、`trading_calendar.txt`、manifest；C 原先只识别 `research-output/snapshots/*/publication.json` 与 `daily_research.parquet`。现 `serve --market-data-dir <目录>`（默认 `data-core`）从 B 指针/指定 ID 固定版本，经仓库只读 resolver 校验四个 hash，再以同目录证券目录和 Parquet 提供 API。C 的 schema 1 合成发布快照 reader 仍用于合成验收，需在启动时明确把 `--market-data-dir` 指向该目录。查询不打开生产 DuckDB。
- C 的证券搜索“更多”现在携带第一页 `snapshot_id`；新发布发生在翻页间不会切换快照。新增 B 格式→搜索/日线→固定旧版本回归测试。B 样本目录无 `instrument_id` 时不编造证券身份，搜索为空；状态按 UNKNOWN 保留。B manifest 只记录创建时间，API 的 `published_at` 返回 null，不编造发布时刻。

## 验收矩阵

| 要求 | 代码证据 | 测试证据 | 真实数据/浏览器证据 | 结果与限制 |
| --- | --- | --- | --- | --- |
| A：来源、原文/hash、覆盖与历史可知时刻 | `scripts/audit_etf_status.py`、`audit_execution_status.py`、机器可读来源事实；迁移 007 与 B 的 `import-status-evidence` 提供统一写入口 | Python 状态审计 19 项通过；半开区间、迟到、冲突、无原文与静默未知均 fail closed | 固定窗口 241 开市日 × 5 ETF = 1205 格，全 UNKNOWN；只有 513300 单次盘中限制的二级消息，交易所原文/hash 未取得 | **部分通过**：缺两市逐日来源完整性、原始文件及可用时刻；A JSON 未导入生产库；P0-3 开放 |
| A：同一价格/Universe 版本复跑与账本 | `etf_status_gated_replay.rs` 锁定旧价格快照、版本、策略，生成 UNKNOWN 阻断输入并独立核算 NAV | 集成方复跑退出 0；旧 32 笔→新 0 笔、24 个 `unknown_status` 拒单；独立最大 NAV 误差 4.66e-10 | Universe `d8811237-6c37-4189-86b8-9b05fbccc405` / version `3fcfed9c-b8c1-4cce-a933-bc2b79ebf0da`；价格 snapshot `fe1dd8a3-d227-4e06-bb98-f0a6b3be0f34`、SHA-256 `8c8debb7e1fdd7b3b592cc0991e807b26f1e94a59fb7bbe7e66e00f3030af872` | **部分通过**：新 `status_gated` 只是 UNKNOWN 占位实验，零交易/NAV 100 万不代表真实执行结果；原始价格、零分红、回溯静态假设保留 |
| B：增量、单写者、修订、失败恢复与重复运行 | `sync-daily`、`sync-daily-latest`、`publish-sync`、`src/lib.rs`、迁移 007、`ops.daily_sync_job/attempt` | 仓库 14 项测试含锁、幂等、重试、断点、空响应、发布中断；脚本 `sh -n` 通过 | 隔离 Tencent `sh600519` 与 `sh510300`，2026-09-21—24 回看：首轮 8 行/8 新修订；重复 8 行/0 新修订 | **部分通过**：指定小样本成立，生产回填状态 Unknown，未运行自动日更 |
| B：质量审计、不可变发布与固定版本 | `publish_daily_sync` 审计日历/显式证券行/OHLCV 和本任务空源响应后写 Parquet+目录+日历，校验 hash 后原子替换 `current.json`；只读 resolver 固定 ID | 审计失败与目录发布后指针替换前故障测试，旧当前快照可读；重复执行无新增有效行；即使旧库已有 bar，本任务返回 `EMPTY` 也使审计失败且不切换指针 | 隔离当前 snapshot `56a22581-b3e1-49c8-9e32-89c754b154e8`；manifest `a436483185e0248a28d5f7b91a118534632b500754573a9363aa67c7a5042484`；Parquet `63ab4d0b67def0b2ec02e7c5f0d1921138e277dda1e400c0fef962663e60624a`；集成方核对 manifest/数据/目录/日历四个 hash 全匹配，截止 2026-09-24，2 行 | **部分通过**：`complete` 只指显式证券×已确认 SZSE 日历的行情范围，不是全市场或状态覆盖；目录两证券均 UNKNOWN、无稳定身份；生产发布未验 |
| C：搜索与日线 API、单位/分页/错误 | `/api/v1/instruments`、`/api/v1/daily-bars`；C schema 1 与 B schema 7 只读适配；游标绑定快照/过滤 | 研究 56 项测试，含 8 项 market API；新增 B 发布格式跨链测试验证 current 切换后旧 ID 仍返回旧 OHLCV | B 隔离真实 snapshot 的四个 hash 经只读解析规则校验；合成股票/ETF API 固定版本返回完整 OHLCV、无复权，最大页 1000 根 | **部分通过**：B 真实样本无证券主键，不可用于真实股票搜索；生产股票快照/十年压力未验 |
| C：股票与 ETF 日 K / 成交量页面 | `MarketPage.tsx`、`Chart.tsx` 蜡烛线与成交量、同轴缩放，路由 `/market` | `npm run build` 通过；无浏览器端权威回测计算 | 集成方在本地浏览器 `127.0.0.1:18811/market` 实看：搜索合成 600000/510300，分别显示 1000 根日 K 与成交量、CNY/股与 CNY/份、原始未复权、snapshot/hash、截止 2026-09-24、覆盖 unverified；目视确认蜡烛图保留高低价 | **合成通过、生产未验**；页面明确不是实时。ECharts chunk 544.47 kB 警告 |
| 跨链：不完整数据、旧快照和量化边界 | 旧实验 `legacy_bar_only` 不补造状态；B 缺状态保留 UNKNOWN；C 缺身份不猜分类；ETF runner 保持 T 收盘→下一开盘、股票回测阻断 | 原有 3 ETF×10 日手算与未来标签隔离测试通过；新增状态/发布/API 测试通过 | 五 ETF 真实状态来源仍无完整覆盖；沪深300真实历史成分未重新核验 | **部分通过**：不把 bar 存在当停牌/投资性；不把 `observed_at` 当历史公开时间；P0-1/2/3 仍开放 |

## 验证命令

| 命令 | 集成结果 |
| --- | --- |
| `cargo fmt --all -- --check` | 通过 |
| `cargo test --workspace --locked --offline` | 通过，仓库 14 + 研究 56；本地真实五 ETF opt-in 1 项默认 ignored |
| `cargo clippy --workspace --all-targets --locked --offline -- -D warnings` | 通过 |
| `cd apps/web && npm run build` | 通过；图表 chunk 544.47 kB 警告 |
| `git diff --check` | 通过 |
| `python3 -m unittest tests/test_etf_status_audit.py tests/test_execution_status_audit.py -v` | 19/19 通过 |
| `.venv/bin/python -m unittest discover -s tests -v` | **未通过**：新增状态审计 19 项通过，旧 `test_warehouse.py` 在导入 `warehouse.py` 时因 `.venv` 未安装 `duckdb` 报 `ModuleNotFoundError`，未运行其测试逻辑；系统 `python3` 同样缺此包 |
| `sh -n scripts/daily_sync_schedule.sh scripts/install_daily_sync_launchd.sh` | 通过 |
| `python3 -m json.tool` 校验 A 的两份来源证据 JSON | 通过；集成方修复 `source-evidence.json` 一处尾随逗号 |
| `python3 scripts/audit_etf_status.py --calendar ... --facts ... --output /private/tmp/pq-batch2-a-reaudit` | 退出 0，1205 UNKNOWN；派生快照 SHA 随本次 `observed_at` 改变，覆盖报告 SHA 保持 `83c14ff4...` |
| `cargo run -p quant-research --example etf_status_gated_replay --locked --offline -- --output research-output` | 退出 0，生成新占位实验 `709a4f67-6aca-427c-8064-8aac413d1764`；未覆盖旧实验 |

浏览器只验了合成发布快照；尝试对第二个本地端口直接访问 B 隔离真实样本时，CLI `curl` 连接受沙箱限制，浏览器报告该端口被客户端阻止，**未**记录为 HTTP 通过。B 真样本的文件 hash 与 schema 由本地文件检查验证；跨链行为由 B 格式 Rust 集成测试证明。旧实验的真实价格表现不因占位状态跑而变为可信无偏收益。

## 开放 P0 与唯一下一步

P0-1 历史 ETF 成员/上市终止 PIT、P0-2 分红/公司行动总回报、P0-3 真实执行状态完整性/历史可知时刻均开放。沪深300真实历史成分继续暂缓；股票回测仍未开放。调度模板未安装/启用；远程认证部署不在本批验收范围。

**唯一最优先下一步：**取得并归档两市覆盖五 ETF 固定窗口的官方逐日执行状态原始文件及完整性说明，逐证券/日期核验原文 hash 与历史可用时刻，经 B 单写者入口冻结真实状态，再沿同一发布 Universe 版本复跑。来源不足时保持 UNKNOWN 和 P0-3 开放。
