# Rust 回测研究框架

> 2026-09-23 状态说明：本文保留早期设计和操作背景。当前可运行能力以 `README.md`、`docs/architecture.md`、`docs/STATUS.md` 与实际代码为准；旧 HTML 页面功能不能等同于当前 React 页面。

Prajna Quant 当前是一个本地、日频的 **ETF Rotation Research Platform MVP**。它从 DuckDB 行情仓库生成不可变 Parquet 快照，独立完成信号研究、策略回测和批量实验，并在本地浏览器查看结果。当前范围只包含 ETF 轮动；个股、小市值、财务因子和行业中性化不在本期交付范围内。

## 快速开始

```bash
cargo run -p quant-research --release -- run \
  --warehouse data-core/market.duckdb \
  --output research-output \
  --config configs/etf_momentum.json

# 在同一份不可变快照上并行扫描 ETF 轮动参数
cargo run -p quant-research --release -- run-grid \
  --warehouse data-core/market.duckdb \
  --output research-output \
  --config configs/etf_rotation_grid.json

# 不依赖策略回测，单独研究一个注册信号
cargo run -p quant-research --release -- signal-research \
  --warehouse data-core/market.duckdb \
  --output research-output \
  --signal momentum_60 \
  --forward-periods 1,5,10,20,60 \
  --quantiles 5

cd apps/web && npm ci && npm run build && cd ../..
cargo run -p quant-research --release -- serve \
  --output research-output \
  --address 127.0.0.1:7878
```

打开 `http://127.0.0.1:7878` 进入当前 React 工作台。策略详情展示 Rust 报告中的累计/年化收益、夏普、卡玛、最大回撤，以及策略净值和回撤图；未显示沪深300曲线、交易成本或自定义区间重算。旧 `dashboard.html` 具备一些上述图表和区间计算，但当前 `server.rs` 没有托管它。服务默认仅监听本机地址，不提供身份认证，不应直接暴露到公网。

策略绩效首页是策略实验目录，可按累计收益、年化收益、夏普、回撤和卡玛排序；点击策略名称查看净值和回撤详情。`configs/etf_momentum_60.json` 是 ETF 60 日动量 Top 5、每 5 个交易日调仓的单因子 MVP 配置：

```bash
cargo run -p quant-research --release -- run \
  --warehouse data-core/market.duckdb \
  --output research-output \
  --config configs/etf_momentum_60.json
```

打开 `http://127.0.0.1:7878/factors` 进入独立信号目录。目录表格可以按 Mean Rank IC、ICIR、IC 胜率和观察数排序；点击信号名称进入详情，可查看信号覆盖、Rank IC、滚动 IC、分组净值、多空净值和 IC decay。`signal-research` 的输出作为可复现报告保存在 `signal-reports/<report-id>/report.json`；每个报告同时包含多个 forward period 的结果。

也可以只生成研究快照：

```bash
cargo run -p quant-research --release -- snapshot \
  --warehouse data-core/market.duckdb \
  --output research-output
```

## 配置与口径

示例配置位于 `configs/etf_momentum.json`。费率使用小数，例如 `0.0003` 表示万分之三；滑点使用基点（bps），`2.0` 表示成交价相对开盘价不利移动万分之二。

| 配置 | 含义 |
|---|---|
| `commission_rate` | 买卖双向佣金率 |
| `minimum_commission` | 每笔最低佣金 |
| `buy_tax_rate` / `sell_tax_rate` | 买卖方向独立税率 |
| `buy_slippage_bps` / `sell_slippage_bps` | 买卖方向独立滑点 |
| `lookback_days` | 收盘到收盘动量回看期 |
| `momentum_short_days` / `momentum_long_days` | ETF 轮动短、中期动量窗口；三项轮动参数均未配置时沿用 `lookback_days` 单动量逻辑 |
| `volatility_window` | ETF 轮动评分的日收益波动率窗口 |
| `trend_window` / `use_trend_filter` | 均线趋势过滤窗口与开关；开启时仅保留收盘价不低于均线的 ETF |
| `short_momentum_weight` / `long_momentum_weight` / `volatility_weight` | 综合评分权重：短动量 + 长动量 − 波动率 |
| `top_n` | 持有因子值最高的证券数 |
| `rebalance_every` | 每隔多少个可计算因子的交易日调仓 |
| `forward_days` | 因子检验的未来收益期 |
| `quantiles` | 因子分组数 |

回测在 T 日收盘后生成信号，在 T+1 日可用开盘价上执行。先卖后买，目标组合等权，数量向下取整到 `lot_size`。成交价格使用未复权原始价格；佣金、税费和滑点分别进入成交记录和汇总指标。没有执行行情的持仓保持不动，日线模型不会推断盘中排队和成交路径。

默认 benchmark 为沪深300指数 `sh000300`，使用腾讯不复权日线并随快照固化。原始响应、请求 URL 和哈希保存在行情仓库；研究快照单独保存 `benchmark_hs300.parquet` 及 SHA-256。夏普比率采用日收益率、零无风险利率和 252 个交易日年化；卡玛比率为年化收益率除以最大回撤绝对值。

信号研究按交易日做 ETF 横截面排名，报告 Rank IC、ICIR、正 IC 比例、分位数组收益和最高组减最低组收益。信号在 T 日收盘观察，未来收益定义为单证券第 n 个后续可用 bar 的 `close(T+n) / close(T) - 1`，只作为检验标签，不进入策略信号。注册表中的低值更优信号（例如波动率）会先转成“数值越高越优”的统一方向；策略轮动综合分数在 `strategy.rs` 单独实现，尚未注册。React 把重叠 forward return 复利成“累计净值”的图仅是现有展示，不能当作可交易 NAV。

## 模块边界与当前迁移状态

项目按模块化单体演进，研究计算不会依赖浏览器页面：

```text
crates/quant-research/src/
  data.rs       ETF snapshot、DuckDB/Parquet 读取和数据质量边界
  feature.rs    收益、滚动均值、波动率、回撤等基础特征算子
  signal.rs     Signal Registry Lite、信号计算、SignalResearchReport
  factor.rs     截面 Rank IC、分组收益与统计口径
  strategy.rs   ETF Rotation 综合评分、排名和持仓选择
  backtest.rs   T+1 开盘撮合、费用、滑点、净值与回撤
  batch.rs      参数笛卡尔积和 Rayon 并行实验
  server.rs     本地展示与只读 API 适配层
```

Signal Registry Lite 已登记并统一计算 `momentum_20/60/120`、`volatility_20/60`、`ma_distance_20/60/120`、`drawdown_20/60`。本地 API 已提供 `GET /api/v1/health`、`GET /api/v1/signals` 和 `GET /api/v1/signals/{key}`，供后续 React/ECharts 前端及 Python 研究适配层共享信号目录。

当前 `server.rs` 已使用 Axum 提供只读 API 并托管 `apps/web/dist` 中的 React + TypeScript + ECharts 构建产物；需要先运行前端构建。`dashboard.html` 和 `factor_dashboard.html` 是未接入的旧实现。Python 尚无 PyO3/maturin 绑定或正式研究包；计算权威边界见 `docs/architecture.md`。

## ETF 轮动与批量实验

`configs/etf_rotation_grid.json` 定义 ETF 轮动的基础配置与参数网格。`run-grid` 会先创建一份快照并在内存中加载一次行情；随后按特征参数分组，复用同一组短中期动量、波动率和趋势评分，再并行运行不同持仓数量或调仓周期。`max_parallelism` 用于限制并发数，避免大数据集上的内存争用。

每个参数组合都会生成原有的 `experiments/<experiment-id>/experiment.json`；同一批次还会生成 `batches/<batch-id>/batch.json`，其中按总收益排序保存组合摘要。轮动策略启用后，因子报告使用与选股相同的综合评分 `etf_rotation_score`，而不是单独的收盘动量。

## 结果结构

每次运行生成两个不可变目录：

```text
research-output/
  snapshots/<snapshot-id>/
    etf_daily.parquet
    benchmark_hs300.parquet
    manifest.json
  experiments/<experiment-id>/
    experiment.json
  batches/<batch-id>/
    batch.json
  signal-reports/<report-id>/
    report.json
```

快照 manifest 保存来源数据库、时间范围、证券数、行数和 SHA-256。实验结果保存完整配置、快照引用、因子逐日结果、净值、回撤、成交及成本明细，使相同输入和配置可以重放。

## 当前边界

当前 ETF 股票池来自现有证券观察表，包含目前仍可观察的产品，尚未接入交易所历史产品名录，因此存在幸存者偏差。数据的 `observed_at` 是本地采集时间，不能替代历史公开时间。框架已经为历史股票池、行业、市值、财务和交易状态留出数据层边界，但这些数据具备可靠历史版本之前，不应声称完成严格 PIT 股票回测。

第一期撮合支持现金账户、整数份额、交易单位、方向性费用及滑点。涨跌停、停牌、分红送转和部分成交需要相应历史数据后接入规则引擎；当前 ETF 样本验证不覆盖这些股票交易规则。

## 验证

```bash
cargo fmt --all -- --check
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
```

端到端验收使用 `run` 命令，并核对控制台输出、`experiment.json` 和浏览器 API 展示的指标一致。
