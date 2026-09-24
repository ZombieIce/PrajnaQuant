# 回测引擎现状

## Inputs

`backtest.rs::run_with_scores(bars, benchmark_bars, config, factor_map)`；bar 为 symbol/date/OHLC/量额，factor_map 为 `(NaiveDate, String)→f64`。生产 runner 从新版 ETF 快照额外读取逐证券/日的 `trade_status` 与 `is_tradable`；快照旧格式没有这两列时保留旧的 bar-only 兼容行为，并在新报告记录 `execution_status_mode=legacy_bar_only`；带状态列时为 `status_gated`。旧报告缺这个字段时模式未知，不能把默认空审计数组解释成无异常。状态 map 缺失记录或标记不可交易会阻止该订单。`runner.rs`/`batch.rs` 选择单动量或轮动综合分数并生成评价报告。当前无事件总线、独立订单簿或数据库交易流水表。

## Internal State / Event Sequence

状态为 `cash`、`holdings: BTreeMap<symbol,i64>`、trades、最近收盘价、带决策日的 `pending` 目标、延期记录、权益曲线、持仓快照、峰值、可选候选日期计数。按日期升序：

1. 当天开盘若有 `pending`，按当前开盘价格及缺价时最近收盘价估计账户总值与等权目标。若任一需要卖出的旧持仓没有当日 open，或状态不是明确 `TRADABLE`，则**整次调仓延迟**；目标内旧仓缺 open 时保守延期，任何卖买腿均不执行，pending 保留。
2. 可执行时先卖后买，成交调整 cash/quantity。状态不明确/不可交易的目标买单和缺开盘价买单会跳过，并在 `BacktestReport.unexecuted_orders` 逐腿记录决策日、尝试日、标的、方向、可计算的目标数量和原因。买入按手数递减直到现金足够。新的调仓信号可替换先前延期的目标。
3. 更新当天可用 bar 的收盘价；`equity = cash + Σ(quantity × last_close)`；更新峰值和回撤，追加当日 `EquityPoint` 与 `PositionPoint`。后者记录现金、逐标的数量/收盘估值/市值、最近 mark 日期、陈旧自然日数和 `Σ市值 / equity` 资金占用率。
4. 用当天 close 的分数排名，达到 `rebalance_every` 时创建下一开盘待执行名单。结尾不再清算。

## Order / Fill / Position / Cash

`pending` 保存目标证券和决策日期，`BacktestReport.rebalance_deferrals` 记录尝试日期、目标、阻塞卖出标的及缺 open/不可交易原因；`unexecuted_orders` 记录未执行买卖腿及 `missing_open`、`missing_status`、`unknown_status`、`halted`、`conflicting_status_sources` 等原因。两者是订单审计摘要，不是完整订单生命周期。没有成交量限制或部分成交。成交基价是 open；买卖各按配置滑点调整 fill；`gross=fill×quantity`；佣金取 `max(gross×rate,minimum_commission)`；方向性 tax 另计。买入现金减少 gross＋commission＋tax，卖出现金增加 gross－commission－tax；`slippage_cost` 仅单列分析，已通过 fill 进入现金，不应二次扣减。spread 无独立配置/模拟，税字段可设零；ETF 示例双向税为零。`CostConfig::default()` 是非零佣金、最低佣金和滑点，实验 JSON 保存有效配置与交易费用，React 当前策略页展示延期及未执行腿，尚无完整成交/成本明细表。

`BacktestReport.position_curve` 为逐日持仓快照，`capital_utilization` 定义为证券市值合计/当日组合权益，不是额外杠杆指标。`instrument_performance` 按标的输出买卖数量、成交笔数、已实现/浮动/总盈亏和期末数量、市值。成本口径固定为移动加权平均成本：买入成交额连同佣金/税计入成本；卖出成交额减佣金/税后扣除对应平均成本；成交价已含滑点；期末浮动盈亏按最后可用收盘价估值。逐标的总盈亏之和应等于期末权益减初始资金（现金无收益）。旧实验 JSON 缺少新增数组时反序列化为空数组，缺执行模式/mark 日期时保持 null；UI 用模式缺失提示旧报告审计未知，不伪造仓位、盈亏或“无异常”，需重跑才能生成新字段。详见 `decisions/0006-instrument-pnl-attribution.md`。

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

理论上每日 `equity ≈ cash + Σ(quantity×估值价)`；应逐笔守恒、cash 不低于零（考虑浮点容差）、quantity≥0、长仓 weight≥0、`Σweight + cash_weight≈1`、实际 gross exposure 不超过预算，交易费用和权益变化一致。已有 3 ETF×10 日金标准走真实评分到回测路径并逐日重建现金、持仓和 NAV；新增断言还校验逐日持仓市值/现金与权益相等、资金占用率口径及逐标的损益归因合计。另有手算状态用例覆盖未知状态买单、停牌旧仓卖单、缺状态记录、延期和逐腿原因，并有 Parquet 状态读取/旧格式兼容测试。基准仍只验原始点保留，不计算基准收益。未验证真实历史状态来源完整性、可知时点、涨跌停规则或真实交易所执行。

## 明确风险

`last_prices` 缺行情时沿用旧 close；完整日期之间的变化可含跳空，非交易日不在权益曲线。新版快照已将交易状态接入订单门槛，但仅有单日状态/来源合并，没有侧别涨跌停规则；旧快照仍走兼容的 bar-only 路径。原始 ETF 价未处理分红及份额变动；它对长期收益的影响尚未量化。`metrics` 的日收益中非有限值被过滤而非报错，若输入源价/现金异常应先验证。不把未证实的问题写成实际未来函数。

旧保存实验中 Top-5 最大正仓数曾达到 6 或 7（详见 STATUS）。当前实现对可复现的成因增加整次调仓延期保护和固定答案测试，因此新状态感知运行不会在旧仓缺卖出 open/可交易状态时继续增加新目标仓位。旧实验不回写；目标买入 bar/状态不可用时仍可能少持仓，但已写入 `unexecuted_orders`。策略详情展示逐腿失败、执行模式和陈旧估值；旧快照维持 bar-only 成交行为，本批五 ETF 真实实验即为此模式。
