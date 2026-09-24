# 实际系统架构（2026-09-23 扫描）

## System Overview

这是本地模块化单体：Rust 仓库写 DuckDB；Rust 研究进程从仓库产生 ETF 和沪深300 Parquet 快照，计算研究/回测并将 JSON 落盘；Axum 读取结果文件并管理本地 Universe JSON；React 展示结果，并通过本地 API 管理 Universe 与筛选已保存结果。旧 `warehouse.py` 是旁支原型。当前没有 Python 因子研究包、Python↔Rust 绑定、外部任务调度或 CI；仓库现有 005 可回放迁移。

```text
公开源/本地 ZIP/CSV → src/sources.rs → src/lib.rs → sql/schema.sql / data-core/market.duckdb
  → research.etf_daily_bar + sh000300 → data.rs → research-output/snapshots/*.parquet
  → feature/signal/factor + strategy → backtest → experiment/batch JSON
  → server.rs 结果 GET + 本地 Universe CRUD API → apps/web React/ECharts
```

## Module Responsibilities / Rust–Python–React Boundary

| 模块 | 当前职责 | 边界 |
| --- | --- | --- |
| `src/main.rs`, `src/lib.rs`, `src/sources.rs` | 采集/导入、源修订、原文归档、发布和查询 | 单写者 DuckDB；研究用只读快照 |
| `data.rs` | 研究快照与 manifest/hash、ETF/沪深300读取 | 快照创建每次只看当前 `research.etf_daily_bar` |
| `feature.rs`, `signal.rs`, `factor.rs` | 基础窗口算子、有限信号目录、横截面 Rank IC/分组评价 | Rust 研究计算权威 |
| `strategy.rs` | 轮动评分和 Top-N | 评分写死于当前策略配置，未通用解耦 |
| `backtest.rs` | 延后一日开盘成交、账户、净值/回撤与逐日持仓曲线、逐标的损益归因、指标 | Rust 组合计算权威；逐标的成本采用移动加权平均法 |
| `batch.rs`, `experiment.rs`, `runner.rs` | 参数网格、报告保存与编排 | JSON 文件，不是数据库实验表 |
| `server.rs`, `universe_api.rs` | 结果 GET、Universe CRUD/成员/覆盖/结果筛选、托管前端静态文件 | Universe 配置为输出目录版本化 JSON；无认证，拒绝非 loopback；不提供运行队列或 POST /runs |
| `apps/web/src/main.tsx`, `apps/web/src/pages/`, `apps/web/src/Chart.tsx` | React 路由壳按需加载策略/因子/Universe 页面；只有详情页加载 ECharts 图表模块 | 仍计算滚动 IC/复利图，非权威结果；策略详情的净值/回撤使用同一日期区间，图表区间首点归一化 |
| `warehouse.py` | 早期 Python 腾讯样本入库/导出 | 不参与 Rust CLI/React 的运行链 |

## Data Flow

仓库原始响应附 URL/hash/run；`staging.daily_bar_revision` 保留修订；`core.instrument` 的**当前** ETF 分类经 `research.etf_daily_bar` 选取历史行。快照优先腾讯日线，次选 TDX；沪深300从 `sh000300` 单独导出，要求至少 1000 行。实验 JSON 保留 config、snapshot manifest、因子报告、回测、假设。

## Factor Flow

信号目录 `signal.rs::registry()` 是固定 10 个规格，`factor.rs` 负责评价。ETF Rotation 综合分数由 `strategy.rs::rotation_scores` 单独计算；它**未注册**为通用信号定义。单独 `signal-research` 写 `signal-reports/`。

## Strategy Flow / Backtest Flow

冻结快照与已发布 Universe 版本 → 从预热历史窗口得分 → 有当天得分和 bar 的 ETF 排名 → Top-N → 每 N 个“有合格目标的日期”在收盘后设 pending → 下一组行情日开盘先审计旧仓卖出可执行性、再卖后买 → 现金/数量更新 → 最近收盘价估值和陈旧天数 → 净值/回撤 → Rust 指标。详见 `strategy-system.md`、`time-model.md`、`backtest-engine.md`。

## API Layer / Frontend Layer

API 清单见 `api.md`。React Universe 管理与成员查看、策略/因子已保存结果筛选已接入 API；运行按钮因无 `/api/v1/runs` 作业接口而禁用。当前 React 原有目录页、详情页与 ECharts 内置缩放/提示；已有持仓数量/资金占用率图、逐标的盈亏及执行异常列表，但无完整成交/费用明细表；服务端一次返回完整实验。仓库的 `dashboard.html`、`factor_dashboard.html` 为**未接入**的旧页面，不应按当前交付界面认定功能。

| 期望视图 | 当前 React 状态 |
| --- | --- |
| Factor Overview / Detail | Implemented：固定信号目录、报告详情 |
| Distribution | Missing：未画因子值分布 |
| IC / Rolling IC / Quantile Return / Long-short Return | Partial：Rank IC 与简单 60 期均值；分组/多空图用未来标签复利，非可交易净值 |
| Factor Turnover / Autocorrelation / Correlation Heatmap | Missing |
| Strategy Overview Metrics / NAV / Drawdown | Implemented：累计/年化收益、夏普、卡玛、最大回撤及两图；策略曲线以首个权益点归一化 |
| Benchmark NAV / Excess NAV / Monthly Return Heatmap / Annual Return | Missing |
| Position Utilization / Per-Symbol Quantity / P&L | Implemented：资金占用率与逐标的持股数量曲线共用绩效日期范围；标的损益表展示成本法下已实现/浮动/总盈亏 |
| Holdings / Weights / Turnover / Trades / Transaction Cost | Partial：期末持仓、资金占用率和每标的交易/数量汇总已展示；逐日权重、组合换手曲线、成交明细和成本明细仍未展示 |

ECharts 图有 tooltip、inside/slider zoom 和无逐点 symbol 的折线；未配置 brush、统一十年数据降采样或经过大数据集交互压测。当前 `apps/web` 只依赖 ECharts，未使用 Plotly。

## 重复与权威实现

Python 与 Rust 均有行情入库/快照原型，主链权威为 Rust；不存在 Python 回测或 Python metrics。`factor.rs::momentum_observations` 与 `signal.rs` 动量计算、`strategy.rs` 轮动评分构成因子逻辑重复边界：当前策略分数以 `strategy.rs` 为权威，通用信号评价以 `signal.rs`/`factor.rs` 为权威，整合需版本和口径审查。Rust `backtest.rs::metrics` 是绩效指标权威；旧 `dashboard.html` 重算收益/年化/夏普/回撤，React 又重新计算归一化净值、滚动 IC 和因子累计净值，存在显示口径与后端偏离的风险。`factor_dashboard.html` 也有独立图表计算但不被服务引用。
