# Batch 2 集成验收（进行中）

日期：2026-09-24。此页先记录可恢复基线与验收门槛；A/B/C 尚未提交第二批独立交接，以下未验项目均为 **Pending**。收到三个任务完成通知后，按实际代码、测试、真实样本和浏览器证据填入逐项结论，不以 Agent 文字报告代替复核。

## 可恢复基线

- 启动时 `main`、HEAD `49c796e`（`Integrate batch 1 universe, snapshot and execution audit`），与 `origin/main` 同步；单一 worktree，`git status --short` 为空。没有需要合并或清理的历史未提交修改。本批新增的公共契约在 [`batch2-contract.md`](../contracts/batch2-contract.md)，当前尚未提交；未创建第二批代码提交，也未推送。
- Batch 1 正式结论为 **部分通过**：合成执行状态/通用快照通过；真实五 ETF 固定快照无状态列，实验 `4e076758-8069-4e0e-9269-1d3f9d574159` 的 `execution_status_mode=legacy_bar_only`。不得将其算作真实执行状态验收。
- 本机 `data-core/` 与 `research-output/` 被 Git 忽略。已存在旧真实快照文件，启动检查未连接或写入生产 DuckDB，未运行真实状态源或新快照复跑；生产数据最新写入状态为 Unknown。
- `docs/contracts/batch2-contract.md` 先冻结身份、证据、增量任务、发布和只读查询语义；新增字段/路由均标为 Planned。A/B/C 文件所有权及公共文件接线由该契约约束。

## 基线命令（第二批实现前）

| 命令 | 结果 | 说明 |
| --- | --- | --- |
| `cargo fmt --all -- --check` | 通过 | Cargo 提示用户级旧 config 路径弃用 |
| `cargo test --workspace --locked --offline` | 通过 | warehouse 10、research 49；真实五 ETF opt-in 测试 1 项默认忽略 |
| `cargo clippy --workspace --all-targets --locked --offline -- -D warnings` | 通过 | 无代码告警 |
| `cd apps/web && npm run build` | 通过 | 既有 ECharts chunk 535.01 kB 警告；构建不等于浏览器验收 |
| `git diff --check` | 通过 | 新契约尚是 untracked 文档，最终仍需复查 |

## 待验收矩阵

| 任务/要求 | 代码证据 | 测试证据 | 真实数据/浏览器证据 | 结果与限制 |
| --- | --- | --- | --- | --- |
| A：来源原文/hash、状态覆盖、历史可知时刻 | Pending | Pending | Pending | Pending；P0-3 仍开放 |
| A：带状态快照、同一 Universe 版本复跑、成交/拒单/NAV 对比 | Pending | Pending | Pending | Pending |
| B：增量任务、单写者、修订去重、错误分类/恢复/重复运行 | Pending | Pending | Pending | Pending |
| B：质量审计、未完成日阻断、原子发布、运行中版本固定 | Pending | Pending | Pending | Pending |
| C：固定已发布快照证券搜索/日线 API，身份/范围/单位/价格/分页 | Pending | Pending | Pending | Pending |
| C：股票/ETF K 线与成交量页面，浏览器对拍截止日和 OHLCV | Pending | Pending | Pending | Pending |
| 跨链：旧快照兼容、来源追溯、缺失与未知、股票回测门槛 | Pending | Pending | Pending | Pending |

## 最终复核清单

1. A：真实状态证据 → 覆盖/可知时刻审计 → 冻结输入 → 同一发布 Universe 版本复跑 → 执行模式与成交/拒单/NAV 对比；真实证据不足时交付可核验缺口，不把 `observed_at` 当历史发布时间，不从 bar 推断停牌或上市。
2. B：增量任务 → 单写者入库 → 修订/去重 → 质量审计 → 原子发布 → 失败恢复与重复执行；失败任务和未完成交易日不能进入默认完整快照，已打开查询/回测不得换版本。
3. C：固定快照 → 证券搜索/日线 API → 股票/ETF K 线和成交量页面；复核日期、单位、价格口径、快照身份，不能用线图抽样破坏日线 high/low。
4. 保留五 ETF `retrospective_static`、零分红、原始价格收益假设；股票行情可查不代表股票回测开放；沪深300真实历史成分核验继续暂缓。
5. 集成后重跑五个规定命令、任务集成测试、真实样本与浏览器检查，列出每条命令退出状态。随后同步 `STATUS`、`HANDOFF`、`priorities`、路线图、API/数据模型及必要 ADR/README/部署说明。
