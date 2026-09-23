# 回测引擎现状

## Inputs

`backtest.rs::run_with_scores(bars, benchmark_bars, config, factor_map)`；bar 为 symbol/date/OHLC/量额，factor_map 为 `(NaiveDate, String)→f64`。`runner.rs`/`batch.rs` 选择单动量或轮动综合分数并生成评价报告。当前无事件总线、独立订单簿或数据库交易流水表。

## Internal State / Event Sequence

状态为 `cash`、`holdings: BTreeMap<symbol,i64>`、trades、最近收盘价、`pending` 目标、权益曲线、峰值、可选候选日期计数。按日期升序：

1. 当天开盘若有 `pending`，按当前开盘价格及缺价时最近收盘价估计账户总值与等权目标；**先卖后买**。
2. 成交调整 cash/quantity；无开盘 bar 则跳过该证券交易。买入按手数递减直到现金足够。
3. 更新当天可用 bar 的收盘价；`equity = cash + Σ(quantity × last_close)`；更新峰值和回撤，追加当日 `EquityPoint`。
4. 用当天 close 的分数排名，达到 `rebalance_every` 时创建下一开盘待执行名单。结尾不再清算。

## Order / Fill / Position / Cash

只有 `pending: Vec<String>` 目标名单和模拟 `Trade`；没有 order_time、订单状态、成交量限制或部分成交。成交基价是 open；买卖各按配置滑点调整 fill；`gross=fill×quantity`；佣金取 `max(gross×rate,minimum_commission)`；方向性 tax 另计。买入现金减少 gross＋commission＋tax，卖出现金增加 gross－commission－tax；`slippage_cost` 仅单列分析，已通过 fill 进入现金，不应二次扣减。spread 无独立配置/模拟，税字段可设零；ETF 示例双向税为零。`CostConfig::default()` 是非零佣金、最低佣金和滑点，实验 JSON 保存有效配置与交易费用，React 当前策略页没有显示它们。

## NAV / Returns / Metrics

`EquityPoint.equity` 是绝对组合权益；实际没有单独持久 NAV 序列，归一化展示由 React 以首个权益点为 1 计算。`total_return = final_equity / initial_cash − 1`，相邻权益点求日收益；年化收益按 `252/(权益点数−1)` 指数换算；波动/夏普用样本标准差及 252，零无风险收益；最大回撤按从 `initial_cash` 开始的运行峰值；卡玛为年化收益/绝对最大回撤。换手为所有成交 gross 加总 / 各权益点均值，**不按单边、年或起始资产再缩放**。当前没有 Sortino、Win Rate、基准收益、超额收益、Tracking Error、Information Ratio；沪深300仅为原始 close 序列保存。

| 指标 | 后端状态 |
| --- | --- |
| Total Return / Annualized Return | Implemented；年化指数收益可称 CAGR，但口径按 252 交易日计 |
| Annualized Volatility / Sharpe / Max Drawdown / Calmar / Turnover | Implemented，见上述定义 |
| Win Rate / Sortino | Missing |
| Benchmark Return / Excess Return / Tracking Error / Information Ratio | Missing；仅 `benchmark_curve` 原始价 |
| Commission / Tax / Slippage / Total Cost | Implemented 汇总与逐笔记录 |

## 必须验证的不变量与当前测试

理论上每日 `equity ≈ cash + Σ(quantity×估值价)`；应逐笔守恒、cash 不低于零（考虑浮点容差）、quantity≥0、长仓 weight≥0、`Σweight + cash_weight≈1`、实际 gross exposure 不超过预算，交易费用和权益变化一致。当前 `backtest.rs` 只有手数取整和一次买卖费用/现金的单元测试；没有每日权益恒等式、Top-N→下日开盘、缺价、调仓周期、基准日期、分红或成本敏感度的完整小型金标准测试。不能把公式写在代码里等同于已有测试。

## 明确风险

`last_prices` 缺行情时沿用旧 close；完整日期之间的变化可含跳空，非交易日不在权益曲线。停牌/状态视图没有进入执行控制。原始 ETF 价未处理分红及份额变动；它对长期收益的影响尚未量化。`metrics` 的日收益中非有限值被过滤而非报错，若输入源价/现金异常应先验证。不把未证实的问题写成实际未来函数。

旧持仓若无当日开盘 bar 会跳过卖出，而新目标仍可买；实际持仓数量可能超过 `top_n`。本机旧实验中 Top-5 的最大正仓数达到 6 或 7（详见 STATUS）；这一点需要进入交易模型的固定答案测试，并在报告中解释实际持仓与目标持仓的差异。
