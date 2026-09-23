# Project Status

扫描日期：2026-09-23。状态含义：Implemented=代码存在；Partially Implemented=只覆盖部分契约；Planned=有明确路线但无实现；Missing=未见实现；Unknown=仓库证据不足。**Implemented 不表示量化正确性已被充分测试或数据已被验收。**代码仓库已关联 GitHub `ZombieIce/PrajnaQuant`；本机被忽略的数据仍不是可交接资产。

完整的问题排序、解决路径及验收判据见 [`priorities.md`](priorities.md)。这里保留按领域归类的事实状态。

## Current Phase

本地日频 ETF Rotation MVP 原型，另有 A 股 DuckDB 仓库。单动量与轮动综合评分/参数扫描均可通过 Rust CLI 执行，React 是只读结果工作台。下一阶段先建立结果正确性金标准，再扩展功能。

## P0 — Correctness

1. **历史 ETF universe 的幸存者偏差（已确认机制）**：`sql/schema.sql::research.etf_daily_bar` 按 `core.instrument.asset_class` 当前值过滤历史；`data.rs` 对该视图全量出快照。无交易所历史名录、上市/退市有效期和可知时刻，也无最短上市历史门槛。历史回测的选股全集不可信为 PIT；以当前观察池解释结果。
2. **ETF 原始价不是总回报（已确认机制）**：`data.rs` 读未复权 open/close，`backtest.rs` 未处理分红/拆分/现金派发，实验假设也明确分红只在源价体现时才反映。长期收益/排名可能失真；影响大小待逐产品验证。
3. **Top-N 目标与实际持仓偏离（已观察结果）**：旧仓无执行日开盘 bar 时 `rebalance` 跳过卖出，新目标仍尝试买入。本机 5 个已保存 Top-5 实验的 `equity_curve.positions` 最大分别为 7/6/7/7/7；因此 Top-N 不是实际持仓上限，可能改变风险暴露与收益。需要金标准测试明确缺价情境和目标重排/待执行规则。

这些是全局回测结果的代码可指认问题/局限；因子图口径与波动率首窗口问题列在下方 Known Bugs，按影响范围在 `priorities.md` 排为 P1。静态检查**没有找到**未来收益进入策略分数、rolling 读未来下标、反向 shift、backfill 或全样本归一化。严格 PIT、分红和交易可执行性仍未被完整证明。`docs/time-model.md` 细述未决时刻问题。

## P1 — ETF Rotation MVP

轮动评分、趋势过滤、Top-N、等权目标、下一行情日开盘成交、成本和参数网格已实现；因子/策略配置没有正式解耦，universe、benchmark、归一化、权重模式未可配置。运行前需要仓库内存在合格 ETF 历史和至少 1000 条沪深300基准行。优先任务见 HANDOFF，仅推荐一个。

## P2 — Research Platform

Signal Registry Lite、单因子 Rank IC/分组/多 horizon 报告、只读 API 与 ECharts 图已实现一部分。通用因子版本、参数元数据、缓存、相关性矩阵、换手/自相关、研究 Python 包、实验服务 API 均缺失。

## P3 — Future

严格 PIT ETF/股票数据、财务披露时点、行业/指数历史成分、分钟/实盘、分布式执行、正式数据发布和十年数据查询优化仍是未来方向；旧设计文件详述目标，不能视为当前状态。

## Completed

- Rust 仓库：原文/hash/run、版本化日线、ETF 当前分类、基础日历/状态/复权因子表与研究视图、Parquet 导出。
- Rust 研究：快照 manifest/hash、单因子报告、综合轮动评分、批量参数扫描、T 后下一行情日开盘模拟、绩效/成本汇总、实验 JSON。
- Axum 六个 GET 接口；React 因子目录/详情、策略目录/详情。`apps/web` 可构建。

## In Progress

当前没有由代码或 Git 提交可确认的持续开发分支；本次交接完成的是文档扫描。既有 `research-output/` 样本并非正式回归验收。

## Missing

- 历史 ETF 名录与上市/退市日期、交易状态接入回测、公司行动/分红总回报。
- 通用 Factor Registry/版本/持久缓存；独立订单/持仓时间序列；统一策略 ID/版本。
- 后端 Sortino、Win Rate、Benchmark Return、Excess Return、Tracking Error、Information Ratio。
- React 的基准/超额净值、月度收益热图、年度收益、持仓、权重、换手、交易、成本；因子值分布、因子换手/自相关/相关热图。
- API 分页、日期范围、降采样；独立 CI、迁移系统、Python 研究包。

## Known Bugs

- 因子分组/多空“累计净值”的口径错误：`factor.rs` 的逐日分组数字是 `forward_days` 后的 close-to-close 收益，React `main.tsx::nav` 把相邻日期的重叠持有期收益复利。`forward_days>1` 时不是可交易组合 NAV；不能据图解释策略业绩。
- 信号波动率首窗口偏差：`signal.rs::signal_values` 将未知首日收益 `None` 变 `0.0`，`feature.rs::rolling_std` 在第 `window-1` 位输出时混入虚构零收益。影响 `volatility_20/60` 信号研究首窗口；轮动策略独立公式不走该分支。
- React 策略目录将轮动网格实验也写作 `lookback_days` 日动量，详情同理；会误导参数解读。旧 HTML 正确展示了部分指标但**不被当前服务使用**。

## Missing Tests

- 3 ETF/10 日人工可验算：T close→下一 open、排名/Top-N、换手、买卖费用、现金/数量/市值/NAV、末日 pending、缺价、日历与 benchmark 对齐。
- 每日 `NAV≈cash+Σ(position×price)`、long-only、权重/现金余量/敞口不变量；历史 universe/分红情境。
- Rank IC/分位数组的固定答案、波动率首窗口、前端展示语义/API 契约。现有测试覆盖仓库入库/版本/锁、基础 rolling、排名统计、手数及单笔成本、参数展开，但不覆盖端到端策略。

## Technical Debt

- Rust 与 Python 重复行情入库原型；Rust 动量/轮动分数有多条路径，统一口径/版本未完成。保留现状，权威边界见 `architecture.md`。
- `backtest.rs::metrics` 是权威；未使用旧 `dashboard.html` 自算绩效。React 仍自己算部分滚动 IC/归一化曲线，语义需与 Rust 报告一致。
- `server.rs` 完整加载/返回大 JSON，无分页/降采样；API 前缀 `/api` 和 `/api/v1` 不统一；前端 JS 构建约 1.27 MB（本轮 build 警告）。
- 本轮对本机 402 MB DuckDB 只读查询 `research.etf_daily_bar` 分源计数时发生 DuckDB `OutOfMemoryException`，提示临时磁盘使用达到 20.6 GiB 上限。它是该视图大范围查询的已观察可扩展性问题；未分析执行计划前不归因到特定 JOIN。快照 manifest 曾记录约 143.6 万 ETF 行，不能据此推断全市场覆盖。

## Risks

- 缺执行 bar 跳单，缺估值 bar 沿用旧收盘价；`research.daily_bar_execution` 的状态不进入策略，成交假设可能过于乐观。
- 当前 ETF 分类与证券名称来自当前观察，不支持历史退市产品；原始价格复权/分红缺失；`observed_at` 不等于公开时刻。
- 基准仅原始价序列，日期可能与权益点不齐；不能由它声称已实现超额/跟踪误差。
- `signal-research` forward horizon 按单证券有效 bar 计，不一定等于 n 个市场交易日。
- 本地数据目录被忽略，新 Agent 无法只凭 Git 仓库复现已有真实数据结果；需注明数据获取/快照来源与 hash。

## Open Questions

1. 研究基准是 ETF 价格收益还是含分红总回报？需要具体 ETF 分红/拆分样本和数据源证据。
2. 历史 ETF universe 的权威名录、上市/终止日期及可知时刻从何获得？未解决前结果只能限定为当前观察池实验。
3. 缺 bar/停牌时旧持仓用何估值价、待执行目标是否保留？现代码采用旧 close 和跳过交易，目标语义需审议。
4. 基准及其他独立序列的对齐日历、缺值处理与日期范围口径是什么？现代码未定义。
5. 非空 universe 但没有有效评分日是否应暂停调仓计数/清仓？现代码暂停计数、保留持仓。
