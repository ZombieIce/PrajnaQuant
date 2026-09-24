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

基础算子位于 `feature.rs`。`signal.rs`/`factor.rs` 计算因子观察/评价；轮动综合分数及其观察位于 `strategy.rs`。`factor.rs::momentum_observations` 与 `signal.rs` 动量规格存在口径维护重叠，暂不删除。评分值只用 T 日收盘及之前数据；默认评价标签为下一行情日开盘入场、H 个市场日区间后开盘退出（`next_open_to_forward_open`），并保留 `close_to_close` 诊断选项。标签不输入回测选股。

## Factor Evaluation

| 评价项 | 后端状态 | 当前界面 |
| --- | --- | --- |
| coverage、missing rate | Partial：逐日 `expected_count`、因子数、有效标签数、缺分数/标签数、总体 coverage；分母需由调用方提供 | 详情展示覆盖率和缺失计数 |
| distribution：mean/std/quantile/skewness/kurtosis | Missing：没有完整因子值分布报告 | Missing；旧未接入 HTML 有 IC 分布，非因子分布 |
| Pearson IC | Implemented：逐日及均值；无有效值时为 null | 因子详情指标和时序图 |
| Spearman Rank IC / Mean / Std / ICIR / positive ratio | Implemented：逐日 Rank IC 和汇总；少于 2 个有效日期或标准差为 0 时 ICIR 为 null | 目录和详情展示 Mean/ICIR/正率；逐日 IC 图 |
| Quantile Return / Long-short Return | Implemented：逐日分组平均前瞻标签和 Q高－Q低；**不是可成交组合收益，也不复利** | 展示逐期标签，不绘制累计净值 |
| Rolling IC / Decay | Partial：逐日 IC；多个 horizon 的 `signal-research` 报告可用于 decay；滚动均值由页面按最多 60 个报告日展示 | 简单滚动 Rank IC 均值与多 horizon decay 图 |
| Factor Autocorrelation / Turnover / Factor Correlation Matrix | Partial：相邻报告日同证券分数 Pearson 自相关和 Top 分位成分更换比例；没有交易组合换手和多因子相关矩阵 | 展示前两项；相关矩阵仍缺 |

评价需至少 `max(quantiles, 3)` 个带标签截面值，否则 IC/分组结果为 null/空数组，并带 `insufficient_cross_section` 状态；常量截面相关系数也保持 null，不能解释成 0。分母未知时 coverage 为 null。市场日历优先取快照 manifest 指定的交易日历文件（开市日），否则使用输入 ETF bar 日期并集，并在报告写明 `calendar_basis`。缺单只证券 bar 会得到缺标签；代码不插值、不将缺 bar 视为可成交。

`signal-research` 可传多个正的 forward periods/分组数，存 `signal-reports/<uuid>/report.json`。策略 `run` 的因子报告由配置中的一个 `forward_days` 生成。日历不完整时范围边界或未来交易日仍可能不可知；基于观察日期并集的 fallback 不能发现所有证券同日缺失。更详细的可视化边界见 `architecture.md` 和 STATUS。

## 正确性约束与建议

波动率使用完整的已观测日收益窗口，不再把第一日未知收益当作 0。`next_open_to_forward_open` 按市场日历推进；若对应单只 ETF 缺少入场或退出 bar，标签为空。`close_to_close` 仍是每证券有效 bar 序号诊断口径，可能跨越超过 H 个市场交易日。分组标签跨日可能重叠，不得累积成 NAV；前端展示逐期统计，若需组合净值须另建明确的可交易持仓/执行模型。因子 ID/version、完整参数元数据、分布统计和相关矩阵仍待设计。
