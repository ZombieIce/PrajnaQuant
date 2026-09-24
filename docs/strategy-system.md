# ETF Rotation 策略现状

## 实际流程

| 阶段 | 状态 | 当前证据/含义 |
| --- | --- | --- |
| ETF Universe | Partially Implemented | `research.etf_daily_bar` 由**当前** `core.instrument.asset_class='ETF'` 取历史日线；没有历史在市/退市全集 |
| Eligibility Filter | Partially Implemented | 需要当日 bar、足够该证券历史、有限分数、可选趋势；执行层对新版冻结快照有交易状态门槛；没有经验证的历史状态可用时刻、上市天数/成交额门槛 |
| Market Data | Implemented | 原始日线快照和沪深300快照；证券日缺口/分红未调整 |
| Factor Calculation | Implemented | 单动量或短/长动量减波动率，趋势过滤；`strategy.rs` |
| Factor Normalization | Missing | 分数以原始小数加权，权重不要求和为 1，无横截面标准化 |
| Composite Score | Implemented | 短动量权重×收益＋长动量权重×收益－波动率权重×样本标准差 |
| Ranking / Top-K Selection | Implemented | 分数降序、symbol 作为平局排序；选 `top_n` |
| Weighting | Partially Implemented | Top-N 目标等权，按开盘价/手数向下取整；买入可能因现金/费用减少，有逐日资金占用率与各标的数量，缺逐证券权重序列 |
| Rebalance | Implemented | 每 `rebalance_every` 个有候选的行情日，T 收盘设目标，下一组行情日开盘执行 |
| Execution | Partially Implemented | 开盘价、方向性滑点、佣金/最低佣金、税、先卖后买；无独立订单/部分成交/流动性约束 |
| Portfolio Accounting / NAV | Partially Implemented | 现金、整数持仓、成本、收盘权益、回撤；已有逐日证券级估值明细、估值日期与合成账本恒等式测试 |
| Performance Metrics | Partially Implemented | Rust 有收益、年化波动、夏普、卡玛、回撤、换手、成本；缺基准派生/Sortino 等 |
| Visualization | Partially Implemented | React 展示目录、策略净值、回撤、因子图；有仓位数量/资金占用率与逐标的盈亏，缺成交/成本明细和基准图 |

## Strategy Configuration

`ExperimentConfig`：`name,start,end,initial_cash,lot_size,strategy,research,costs`。`StrategyConfig`：`lookback_days,top_n,rebalance_every,momentum_short_days,momentum_long_days,volatility_window,trend_window,use_trend_filter,short_momentum_weight,long_momentum_weight,volatility_weight`。`CostConfig`：佣金率、最低佣金、双向税、双向滑点 bps，且有非零默认值。`ResearchConfig`：前瞻天数与分组数。

缺显式 `strategy_id`/策略版本、universe 规则/版本、因子列表/因子版本、归一化、排名规则、权重模式、日历频率、可配置 benchmark。`end` 可为空；未指定时读当前快照尾端。`benchmark` 在数据快照/报告中固定为沪深300 `sh000300`。配置 JSON 示例位于 `configs/`；网格只展开部分参数，权重等仍来自 base。策略和因子尚未完全解耦：综合因子、ETF universe、Top-N、等权都在 Rust 代码里决定。

## Selection / Weighting / Rebalance / Execution

`rotation_scores` 对每证券独立按日期排序；足够窗口后用当日 close 与历史 close 得分。可选趋势：当日收盘价不低于包含当日的 `trend_window` 均价。选择只在当日有行情且有分数的 ETF 中发生；候选少于 Top-N 时选已有候选。目标每只占执行开盘估值权益的 `1/targets.len()`；卖出超额持仓，买入缺额持仓，最终手数向下取整。未出现执行开盘 bar 的目标不买；未出现开盘 bar 的旧持仓不卖。空候选不会主动清仓。最后一个收盘信号没有后续开盘则不执行。

## 风险边界

当前 ETF 分类来自今天的证券状态，是已证实的选择机制，存在幸存者偏差风险。`first_observed_date` 非上市日期，没有 `listing_date`/`delisting_date` 或最短上市历史限制（窗口长度仅保证可计算）。既有状态和前复权视图未接回测。长仓数量由买卖逻辑保持非负，但已有逐日资金占用率与合成账本恒等式，尚无通用逐仓权重/敞口接口。`top_n` 不必等于实际持仓数；现金、交易费用、执行缺失会影响暴露。详见 `time-model.md` 与 `backtest-engine.md`。

本机五个已保存 `top_n=5` 实验中，每日 `positions` 最大分别为 7、6、7、7、7，验证了缺 bar 时旧仓滞留的实际结果。这些既有产物只用于诊断，不证明新代码/新环境的可复现性。
