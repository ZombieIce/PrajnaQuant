# 因子与信号系统现状

## Factor Definition / Registry

固定 `signal.rs::registry()` 提供 10 个信号规格：动量 20/60/120、均线距离 20/60/120、波动率 20/60、窗口回撤 20/60。每个 `SignalDefinition` 有 `key`、展示名、category、direction、description、lookback、dependencies；`signal_values` 解析 key 得到算子。该 Lite 注册表能查询元信息并运行**单信号评价**，不等于通用 Factor Registry。`strategy.rs::rotation_scores` 的短/长动量－波动率＋趋势过滤是另一条硬编码策略因子路径，名称 `etf_rotation_score` 仅在报告中出现，未登记到 `signal.rs`。

| 期望元数据 | 实际状态 |
| --- | --- |
| factor_id / name / category | `key` / `display_name` / `category` 已有，轮动综合分数不在注册表 |
| version | 缺失；报告有 engine_version，但不足以标识因子口径 |
| parameters / frequency / lookback | key 包含窗口，`lookback` 字段有；频率仅由日线流程隐含，任意参数接口不存在 |
| required_fields / backend | `dependencies=[close]`；实现只在 Rust，未有正式 backend 字段 |
| Factor Storage / Cache | 报告 JSON 及实验 JSON，网格按 `feature_key` 在一次运行内复用评分；无持久因子值缓存 |

基础算子位于 `feature.rs`。`signal.rs`/`factor.rs` 计算因子观察/评价；轮动综合分数及其观察位于 `strategy.rs`。`factor.rs::momentum_observations` 与 `signal.rs` 动量规格存在口径维护重叠，暂不删除。因子结果以当前日期收盘可知的值和未来 close-to-close 标签组成；未来标签不输入回测选股。

## Factor Evaluation

| 评价项 | 后端状态 | 当前界面 |
| --- | --- | --- |
| coverage、missing rate | Partial：`observation_count`、逐日有效 `count`；无应有 universe 分母或 missing rate | 观察数/有效 ETF 数，非覆盖率 |
| distribution：mean/std/quantile/skewness/kurtosis | Missing：没有完整因子值分布报告 | Missing；旧未接入 HTML 有 IC 分布，非因子分布 |
| Pearson IC | Missing | Missing |
| Spearman Rank IC / Mean / Std / ICIR / positive ratio | Implemented：逐日 Rank IC 和汇总；未定义风险调整年化 | 目录和详情展示 Mean/ICIR/正率；逐日 IC 图 |
| Quantile Return / Long-short Return | Implemented：逐日分组平均未来收益和 Q高－Q低；**不是可成交组合收益** | 分组/多空累计图是浏览器把重叠未来收益复利，口径不成立，见 STATUS |
| Rolling IC / Decay | Partial：逐日 IC；多个 horizon 的 `signal-research` 报告可用于 decay；没有后端滚动 IC 输出 | React 60 期简单滚动平均与多 horizon decay 图 |
| Factor Autocorrelation / Turnover / Factor Correlation Matrix | Missing | Missing；旧 HTML 占位不算实现 |

`signal-research` 可传多个正的 forward periods/分组数，存 `signal-reports/<uuid>/report.json`。策略 `run` 的因子报告由配置中的一个 `forward_days` 生成。更详细的可视化边界见 `architecture.md` 和 STATUS。

## 正确性约束与建议

`signal.rs` 中波动率把首个不存在的日收益置为 `0.0`，首个可计算窗口可能含这个占位，导致首个波动率值偏差；应以确定样例验证后修正。评估标签按单证券可用 bar 序号推进，停牌/缺数时“5 日”可能跨过超过五个市场交易日。分组跨日高频重叠，不得累积成 NAV；前端应展示统计量或建立明确的可交易持仓/执行模型。未来若统一因子接口，先确定 factor_id、version、参数序列化、数据快照、可知时刻与后端归属，并写 ADR。
