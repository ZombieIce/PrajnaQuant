# Agent 工作约定

## Project Scope

项目主路线是 Rust-first 的多市场研究、并行回测与未来实盘平台；当前先执行 POC-0，再按三级引擎与能力路由分阶段建设。目标决策见根目录 `ARCHITECTURE.md`，交付顺序见 `docs/product-roadmap.md`，领域词汇见 `CONTEXT.md`。现有实现仍是 A 股日频仓库和 ETF Rotation 纵向场景，React 页面已经存在；新平台 MVP 暂不以 Web 为验收项。不得把多市场、三级引擎、严格 PIT、分红总回报、Python 研究平台或实盘当作现有能力。功能/风险状态见 `docs/STATUS.md`。

## Architecture Principles

依据当前实现：Rust 为数据写入、研究计算、回测、指标与只读 API 的主实现；Python `warehouse.py` 是独立早期入库原型；React 只展示报告。不要为了文档设想改写正确代码。避免新增第二套量化核心。证据优先级：代码与可运行测试 > 当前配置/schema > 最新 ADR > STATUS > HANDOFF > README > 旧文档/注释。

## Rust Responsibilities

`src/` 管理采集、原文、DuckDB 和导出；`crates/quant-research/src/` 管理特征、因子研究、综合评分、排序、调仓、成交、账户、绩效、实验和 Axum API。当前报告以 Rust 计算为 canonical。`docs/decisions/` 说明已确认的边界；未决方案只写提案。

## Python Responsibilities

现有 `warehouse.py` 仅为样本入库原型。目标是让 Python 承担因子探索、统计分析与研究实验，通过明确版本和时间语义的数据/结果契约与 Rust 互通；回测及绩效权威继续在 Rust。当前没有 Python 调用 Rust 的正式接口、研究包或回测引擎。不得宣称已接通。

## React Responsibilities

`apps/web/` 是当前服务端实际托管的界面。目标界面还包括选股池、股票/ETF K 线及更完整的因子和策略报告。展示、排序和交互应与后端报告语义一致；不得让浏览器成为绩效/因子结果的权威计算方。仓库中的旧 HTML 页面未被当前 `server.rs` 引用。

## Quant Correctness Rules

- 任何因子必须说明输入字段、窗口、可观测时刻、可用时刻、方向、缺失规则、版本/来源；未来收益只能用于评价标签。
- 若历史 ETF 成分、上市/退市和价格分红调整无法做 point-in-time 证明，应在结果与文档显式披露幸存者和收益口径风险。
- 区分配置要求、代码实现、已有测试和实际数据验证。没有证据时标记 Unknown/Unresolved。

## Look-ahead Bias Rules

- 当前策略 T 日收盘含当日 close 形成信号，下一可用日 open 执行。不得用 T 日 close 同时成交，或把 `forward_return` 输入评分。
- 核查 rolling 边界、shift 方向、缺失填充、全样本归一化、基准日期和历史 universe 的可知性。基本面按公开时间而非报告期末决定可用时刻。

## Time Model Rules

遵守 `docs/time-model.md`。变更执行时序前先给出事件序列和小型可人工验算用例；`observed_at` 是本地采集时间，不是历史发布时间。

## Portfolio Accounting Rules

现金、数量、成交价、佣金、税、滑点必须可追溯。逐日验 `NAV ≈ cash + Σ(quantity × 当日估值价)`；长仓数量和权重非负，解释现金余量与总权重。不得把停牌/缺行情默认为零价或可成交。交易成本配置必须随结果保存。

## Development Rules

本轮文档记录的是现状，不构成开发许可。改动前读相关代码和测试，先 `git status`，保留用户的未提交文件。接口、数据 schema 或量化口径变更时同步文档与必要 ADR。

## Testing Rules

`cargo fmt --all -- --check`、`cargo test --workspace --locked --offline`、`cargo clippy --workspace --all-targets --locked --offline -- -D warnings`；Python 有环境时 `.venv/bin/python -m unittest discover -s tests -v`；前端 `cd apps/web && npm run build`。仓库缺少独立端到端的 3 ETF / 10 日金标准用例。每次相关变更需检查 look-ahead、日期对齐、现金持仓恒等式与成本。不要把编译通过当量化正确性证明。

## New Agent Startup

按顺序读：1 `AGENTS.md`；2 `README.md`；3 `ARCHITECTURE.md` 与 `CONTEXT.md`；4 `docs/product-roadmap.md`；5 `docs/architecture.md`；6 `docs/time-model.md`；7 `docs/data-model.md`；8 `docs/factor-system.md`；9 `docs/strategy-system.md`；10 `docs/backtest-engine.md`；11 `docs/STATUS.md`（及其引用的 `docs/priorities.md`）；12 `HANDOFF.md`；13 相关 ADR 与 POC 规范；14 任务代码；15 任务测试。然后 `git status`、`git log -5 --oneline`，运行相关验证。

## Agent Completion Checklist

运行相关测试、formatter/lint；复查未来函数、时序、组合核算和交易成本；更新 `docs/STATUS.md` 与 `HANDOFF.md`。仅在发生重要决策/契约变化时更新 ADR、API、README。交接中写明已验证命令、失败、开放问题和唯一建议下一步。

## Agent skills

### Issue tracker

Issues live as Markdown files under `.scratch/<feature>/`. See `docs/agents/issue-tracker.md`.

### Triage labels

Use the five default triage labels: `needs-triage`, `needs-info`, `ready-for-agent`, `ready-for-human`, and `wontfix`. See `docs/agents/triage-labels.md`.

### Domain docs

Use the single-context layout with a root `CONTEXT.md` and decisions in `docs/decisions/`. See `docs/agents/domain.md`.
