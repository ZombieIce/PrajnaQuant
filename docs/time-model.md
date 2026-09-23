# 时间模型与可知性

## 当前策略事件序列（代码已确认）

`backtest.rs::run_with_scores` 每日按日期升序：先取 `pending`，用本日 `open_map` 调仓；再把本日 `close` 写到 `last_prices`，算本日收盘权益/回撤；随后用本日 bar 和本日评分 `select_top_n`，达到周期才产生下次开盘待执行目标。因此：

| 时间概念 | 实际定义 |
| --- | --- |
| observation_time | 日线 T 的收盘价/当日完整 bar；当前信号数值只读 close 历史 |
| signal_time | T 收盘后，由 `strategy.rs`/`factor.rs` 计算 T 分数 |
| decision_time | T 收盘后 Top-N 和周期判断 |
| rebalance_time | T 收盘后创建 pending 目标；目标在下一组行情日开盘使用 |
| order_time | 代码没有独立订单对象/时间戳；仅 pending 目标名单 |
| execution_time | 下一组 ETF 行情日期 T+1 的开盘价加方向性滑点；并非严格交易所日历 T+1，缺数据日可能跳过 |
| position_effective_time | 成交后立即改变现金/数量，当天收盘权益含新持仓 |
| return_period | 权益曲线按相邻**有 ETF bar 的日期**收盘值求收益；首点为第一条行情日的收盘权益。因子评价标签为每证券 `close(i+n)/close(i)-1`，n 是该证券的可用 bar 序号，不一定是全市场交易日 |

`rebalance_every` 用有非空排名的行情日计数；没有候选就不增计数、不生成待执行目标。缺少个别目标执行日的 bar 时该 ETF 不成交；旧持仓无当日收盘时沿用最近收盘价估值。最终日期生成的 pending 不执行。`benchmark_curve` 只是独立保存沪深300日期/close，Rust 未计算对齐后的基准收益或超额收益。

## 已核查的未来函数边界

`feature.rs` rolling 窗口终点含当前值；`strategy.rs` 用 `i` 与更早下标；`factor.rs`/`signal.rs` 的未来收益只在因子评价的 `forward_return` 字段中，策略分数从当前或历史价获得；`runner.rs` 的策略分数 map 并未取评价标签。此次静态扫描未找到明确的“未来收益进入策略分数”或反向 shift。不能据此声称完整 PIT：当期数据最终值的历史可知性未证实。

## Unresolved / P0 Open Question

- `research.etf_daily_bar` 用 `core.instrument.asset_class='ETF'` **当前值**选取全历史，没有历史在市名单、`listing_date`/`delisting_date` 或版本可知时点。该流程确定有幸存者偏差风险；历史收益适用范围待限定。
- `core.instrument.first_observed_date` 是第一次本地观察日期，不是交易所上市日期；`observed_at` 是采集时间，不是历史公布时间。ETF 分类修订虽有 `effective_date`，快照查询未按历史有效/可知时刻过滤。
- 原始 ETF 价对分红/拆分的可比收益如何定义仍未解决；`research.daily_bar_qfq` 没被 ETF 回测读取。分红总回报或复权价不能自行假定。
- 缺行情时最后收盘价估值、缺执行 bar 跳过成交，未引入停牌/状态视图；何时把目标视为可交易待明确。开盘成交没有独立“订单已提交/部分成交”语义。
- 基准图的日期集合可能与权益曲线不一致；任何超额、跟踪误差和信息比率都需先确立交集/现金流/分红口径。

## 变更验收

时序变更必须用至少 3 ETF / 10 个交易日的已知价格、信号、下一日开盘、费用与逐日权益测试证明；特别验证 T 日 close 不会以 T 日价格成交、无行情日、最后一日 pending 和基准日期对齐。
