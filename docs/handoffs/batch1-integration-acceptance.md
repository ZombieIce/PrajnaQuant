# Batch 1 集成验收（2026-09-24）

**结论：部分通过。** A 的执行规则与 B 的冻结状态输入在合成快照中接通并通过手算测试；C 的真实五 ETF 已发布版本、固定快照、实验落盘、API 和页面闭环通过。但固定的真实 ETF 快照没有交易状态列，本次真实复跑明确是 `legacy_bar_only`，不能用它证明真实停牌/状态门槛。历史成员 PIT、分红总回报、状态来源可知时点仍未通过。

## 工作区与集成方式

- 检查时为 `main`、HEAD `8381beb`；只有这一个 Git worktree。A/B/C 均在共享工作区的未提交文件和被忽略的本地 `research-output/` 中，无可 cherry-pick 的独立提交。未 reset、clean、stash、推送或终止生产回填进程。
- 共享修改跨 `data.rs`、`runner.rs`、`backtest.rs`、`server.rs`、React 和文档。初始格式、全工作区测试、Clippy、前端构建均通过，说明交接中曾出现的编译冲突在本次检查前已消失。没有重复合并。
- `data-core/.writer.lock` 存在；`ps` 被系统拒绝，`lsof` 没有返回持有者，无法证明生产 DuckDB 无写进程。只读取已冻结 Parquet 与 Universe JSON，不连接或复制生产 DuckDB；回填窗口实时进度为 Unknown。
- 本次补修：目标内旧仓缺 open 的保守延期、状态字段互相矛盾或无来源时 fail closed、报告执行模式与估值日期/陈旧天数、通用快照日历旁车排除不完整月份、结果假设按实际执行模式生成、API 评分口径摘要、React 审计/假设/旧结果缺失提示及轮动标签。

## 验收矩阵

| 任务/要求 | 代码证据 | 测试证据 | 实际结果 | 剩余限制 | 是否通过 |
| --- | --- | --- | --- | --- | --- |
| A：旧仓无法卖出整笔延期、重试、最新目标替换 | `backtest.rs` 的 `planned_sales` 预检、`pending`、`rebalance_deferrals` | `unavailable_holding_sale_defers_rebalance_instead_of_exceeding_top_n` 等手算 fixture；新增目标内缺 open/状态减仓检查 | D3 零成交，D4 按新目标处理，Top-1 不超额 | 真实交易所停牌、涨跌停、排队/部分成交未验 | 合成通过 |
| A：目标买单跳过、未知与停牌区分、末日目标 | `unexecuted_orders`、`status_block_reason`、末日 `TARGET` 记录 | 目标缺 open 后新信号再入、UNKNOWN/HALTED/缺状态、末日测试；新增矛盾状态 fail-closed 测试 | 原因、决策/尝试日期与来源可查询；跳过买腿不自动保留旧目标 | 目标可能少于 Top-N；末日没有真实开盘，不推断数量 | 合成通过 |
| A：账户、费用、估值、P&L | `PositionPoint`/`InstrumentPerformance`；新增 `mark_date`、`stale_calendar_days` | 3 ETF×10 日独立预期权益和成交；逐日现金+持仓估值、成本/P&L 恒等式 | 佣金、税、滑点分别入账，滑点只经成交价扣一次；陈旧估值可追溯 | 真实公司行动、现金利息不支持 | 合成通过 |
| B：证券目录与通用冻结快照 | `load_security_directory`、`create_daily_snapshot`、`load_daily_research_rows`、`ExecutionStateInput` | `mixed_daily_snapshot_freezes_selected_assets_revisions_and_prewarm_range`、混合分类/旧 ETF 兼容测试 | 显式证券、预热标记、当前资产分类与来源、状态采集时间冻结；旧 ETF reader 拒绝混合股票快照 | 未从生产库创建股票快照；当前观察目录不是历史 PIT 名录 | 代码/合成通过，生产未验 |
| B→A：状态输入真正被 runner 消费 | `load_snapshot_execution_statuses` → `runner.rs` → `run_with_scores_and_statuses` | `runner_loads_execution_status_from_snapshot_before_backtest`，Parquet 状态列/缺列测试 | 新快照缺状态拒单；旧快照以 `legacy_bar_only` 明示 | 状态源 `observed_at` 仅本地采集时间；无历史 `available_at`，真实源覆盖未证 | 合成通过，真实未验 |
| B：覆盖审计与日历 | `audit_daily_market_coverage`、`audit_etf_market_coverage`；完整月份导出过滤 | 新增不完整月份不得写入旁车测试；B 报告中的 2025-09 五 ETF 样本审计 | 固定五 ETF 窗口 241 个 SZSE 开市日，每只 241 bar，缺日/重复/OHLC 异常为 0 | 原 ETF Parquet 无逐行 source；SH 日历未独立交叉核验，整体 audit=`unverified` | 日期覆盖通过，来源未验 |
| C：真实已发布版本与固定快照复跑 | `examples/etf_universe_mvp.rs` 读取 `UniverseStore`、核对成员/区间/hash 和锁定快照 | opt-in `published_version_and_locked_snapshot_round_trip_with_account_ledger` 用绝对输出目录通过；隔离目录与项目目录均复跑 | 真实 Universe/version、内容 hash、241 日实验及成员 hash 落盘；重复 ensure 返回 `version_created=false`、总版本数 1 | 版本没有可持久化手工 coverage segment，实验 membership coverage=`gaps`；本次快照是 bar-only | 部分通过 |
| C：结果持久化、筛选与旧结果 | `ExperimentResult`、`server.rs` 精确筛选及旧字段默认 | HTTP 详情 API 对比磁盘 JSON；正确版本包含本批新实验，错误版本 0；旧实验实际读取 | 身份/数组/字符串字段一致；数值重序列化最大绝对差 `2.33e-10`；旧结果 Universe/执行模式显示未知 | 详情一次返回完整 JSON，无分页 | 通过 |
| 前端：Universe/策略追溯 | `UniverseDirectory.tsx`、`StrategyDetail.tsx`、`shared.tsx` | 本地浏览器 `127.0.0.1:18791` 与最终项目 `:18792` 实看；`npm run build` | 版本 1、五成员、精确筛选、轮动规则、净值/仓位/盈亏、执行模式和假设可见；新运行按钮禁用 | 无服务端作业 API；旧报告不能补造审计；当前实测样本无延期/拒单供真实页面展示 | 通过已实现页面，运行能力未开放 |

## 固定身份与结果

- Universe `d8811237-6c37-4189-86b8-9b05fbccc405`；version `3fcfed9c-b8c1-4cce-a933-bc2b79ebf0da`；内容 SHA-256 `91aa64f7872ced85b4513f224fa617a14fa12ae99c86f5936085a86466a4ba18`。成员：513300、518880、159612、510320、159952；有效区间 `[2025-09-23, 2026-09-22)`，回溯静态、窗口内默认可投资。
- Snapshot `fe1dd8a3-d227-4e06-bb98-f0a6b3be0f34`，ETF Parquet SHA-256 `8c8debb7e1fdd7b3b592cc0991e807b26f1e94a59fb7bbe7e66e00f3030af872`；日历 SHA-256 `0701455f5d65e317974bf61b223a0cbdb36250079d1dca7ca4e3ba01b17bcfee`。运行期间没有切换最新快照。
- 最终项目实验 `4e076758-8069-4e0e-9269-1d3f9d574159`，文件 `research-output/experiments/4e076758-8069-4e0e-9269-1d3f9d574159/experiment.json`。241 个权益点、32 笔成交、期末权益 1,179,713.3164、原始价格收益 17.9713%、最大回撤 −15.6544%；佣金 2,991.4708，税 0，滑点 1,981.6128，总成本 4,973.0836。逐日独立账本最大 NAV 误差 0；最终 Top-2 实际 2 仓；标的 P&L 合计约 179,713.32，与权益变化一致。数字只属于零分红/原始价格假设，不是总回报或无偏历史业绩。
- 隔离测试先写入 `/private/tmp/prajna-batch1-final.kijx7N`，项目输出后写入上述新实验；未覆盖历史实验。`ensure_etf_universe_mvp.py` 重复调用没有新建 Universe 或版本。

## 验证命令与结果

- `cargo fmt --all -- --check`：退出 0。
- `cargo test --workspace --locked --offline`：退出 0，仓库 10、研究 49、doc 0；opt-in 真实用例默认 ignored 1。
- `cargo clippy --workspace --all-targets --locked --offline -- -D warnings`：退出 0。
- `cd apps/web && npm run build`：退出 0；ECharts 图表 chunk 535.01 kB 警告。
- `git diff --check`：退出 0。
- opt-in 真实测试第一次使用相对 `PRAJNA_ETF_MVP_OUTPUT=research-output`，因 Cargo 测试工作目录为 crate 而失败（找不到 Universe JSON）；改为项目绝对路径后 1 passed。应在复现命令中始终使用绝对路径。
- `cargo run --locked --offline -p quant-research --example etf_universe_mvp -- --output research-output --universe-id ... --version-id ...`：退出 0，产生最终实验；脚本与 API 结果如上。
- `python3 scripts/ensure_etf_universe_mvp.py --base-url http://127.0.0.1:18792`：退出 0，`version_created=false`。
- HTTP：`GET /api/v1/universes/{id}/members` 返回五成员；指定范围 coverage 的 `calendar_status=complete`、241 日期、总体 `unverified`；`GET /api/experiments?universe_id=&version_id=` 找到新实验，错误版本空；详情读取和旧实验兼容通过。浏览器核对最终详情的规则、执行模式、图表、盈亏及身份/假设。Python 入库代码本批未变，未运行其缺 `duckdb` 的环境测试。

## 开放 P0 与唯一下一步

P0-1 历史 ETF 成员/上市终止 PIT、P0-2 ETF 公司行动/分红总回报、P0-3 真实执行状态完整性/历史可知时点和旧快照 bar-only 风险均开放。合成状态测试不等于真实状态验证；行情日覆盖不等于历史成员 verified PIT。沪深300真实历史成分继续暂缓，股票数据快照不开放股票回测。

**唯一最优先下一步：**获取并核验本五 ETF 窗口内可追溯的历史交易状态来源、覆盖与可用时刻，导出带状态的冻结快照并按同一发布版本复跑；在此之前保持 P0-3 开放及旧快照模式提示。
