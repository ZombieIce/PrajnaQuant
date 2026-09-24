# 时间模型与可知性

## 当前策略事件序列（代码已确认）

`backtest.rs::run_with_scores` 每日按日期升序：先取 `pending`，用本日 `open_map` 调仓；再把本日 `close` 写到 `last_prices`，算本日收盘权益/回撤；随后用本日 bar 和本日评分 `select_top_n`，达到周期才产生下次开盘待执行目标。因此：

| 时间概念 | 实际定义 |
| --- | --- |
| observation_time | 日线 T 的收盘价/当日完整 bar；当前信号数值只读 close 历史 |
| signal_time | T 收盘后，由 `strategy.rs`/`factor.rs` 计算 T 分数 |
| decision_time | T 收盘后 Top-N 和周期判断 |
| rebalance_time | T 收盘后创建 pending 目标；目标在下一组行情日开盘使用 |
| order_time | 代码没有独立订单对象；待执行目标记录决策日和目标名单，发生旧仓卖出缺价时 `BacktestReport.rebalance_deferrals` 记录尝试日期、阻塞标的和原因 |
| execution_time | 下一组 ETF 行情日期 T+1 的开盘价加方向性滑点；并非严格交易所日历 T+1，缺数据日可能跳过 |
| position_effective_time | 成交后立即改变现金/数量，当天收盘权益含新持仓 |
| return_period | 权益曲线按相邻**有 ETF bar 的日期**收盘值求收益；首点为第一条行情日的收盘权益。因子评价默认使用 Open(T+1) 到 Open(T+1+H) 的市场日历标签；显式 `close_to_close` 诊断选项为每证券 `close(i+n)/close(i)-1`，n 是该证券的可用 bar 序号 |

因子研究日历优先取快照 manifest 指向的完整自然月开市日历，否则退化为输入 ETF bar 日期并集，报告携带 `calendar_basis`。缺少单只 ETF 在入场/退出日的 bar 时标签为空，评价不插值；使用观察日期并集作为 fallback 时不能识别全体 ETF 同日缺行情。此选择只影响标签和评价分母，不改变回测事件序列或执行日历。

`rebalance_every` 用有非空排名的行情日计数；没有候选就不增计数、不生成待执行目标。若本次调仓需卖出的任何旧仓缺执行日 open，或执行状态不是明确 `TRADABLE`，则整笔调仓不成交，保留 pending 并在下一行情日重试；目标内旧仓缺 open 时也保守延期，避免无法排除其需要减仓而先买其他目标。期间到期的新调仓信号会替换旧目标。该延迟尝试写入 `rebalance_deferrals`，阻塞卖腿也逐条写入 `unexecuted_orders`。若不存在这类阻塞，仍先卖后买；目标买入标的缺 open、缺状态或状态为 `HALTED`/`UNKNOWN`/多源冲突时会跳过该买单，并记录可计算的目标数量和原因。新版快照把 `research.daily_bar_execution` 的状态按证券/日期合并写入 ETF Parquet；多个状态源必须全部为 `TRADABLE` 才放行。旧快照缺少状态列时仍用 bar-only 兼容行为，并在报告写 `execution_status_mode=legacy_bar_only`；新状态快照写 `status_gated`。状态行 `observed_at` 仅为本地采集时间，缺历史 `available_at`，不证明 T 日已公开。旧持仓无当日 close 时沿用最近 close 估值；新报告逐仓记录 `mark_date` 与 `stale_calendar_days`，旧报告字段缺失时保持未知。最终日期生成的 pending 不执行，报告用 `TARGET/no_future_execution_session` 记录未执行目标。`benchmark_curve` 只是独立保存沪深300日期/close，Rust 未计算对齐后的基准收益或超额收益。

## Universe v1 时间边界

Universe 成员解析以交易日 T 的 Asia/Shanghai 15:00 为决策截止（UTC 07:00）。历史加入事件必须已生效且已公开；退出事件只有在有效日已到且公告已公开后才会从当时决策集合移除。成员状态由截止时点可知的事件重建，不能用今天完整历史区间反向截断公告前的候选。覆盖必须来自独立完整性证明；缺少某日截面不表示该日成员为空。

当前临时沪深300数据将公告时点模拟为对应 `opt-in`/`opt-out` 生效日之前 14 个自然日的 00:00 UTC；加入和退出时间分别保存在 `published_at` 与 `effective_to_published_at`。这是为打通事件时间字段的占位假设，不是历史公告发布时间证据。相关事实保持 `verification_status=unknown`、覆盖 `unverified`，不可用于严格 PIT 回测。

Universe-aware 回测入口保留完整价格面板做因子预热、出池后持仓估值和卖出，只按日对评分与因子观察横截面应用成员掩码。已解析且覆盖完整的真实空池日，若恰逢调仓周期，会产生显式空目标并在下一有行情开盘尝试清仓；成员非空但评分缺失、预热不足或历史覆盖未知时不推断空池，不生成空目标。缺执行 bar 仍按原有行为保留仓位并沿用最近收盘估值。固定 3 证券 × 10 日期 fixture 在 `docs/universe-contract.md` 和 `universe.rs` 测试中给出。


## 已核查的未来函数边界

`feature.rs` rolling 窗口终点含当前值；`strategy.rs` 用 `i` 与更早下标；`factor.rs`/`signal.rs` 的未来收益只在因子评价的 `forward_return` 字段中，策略分数从当前或历史价获得；`runner.rs` 的策略分数 map 并未取评价标签。此次静态扫描未找到明确的“未来收益进入策略分数”或反向 shift。不能据此声称完整 PIT：当期数据最终值的历史可知性未证实。

## Unresolved / P0 Open Question

- `research.etf_daily_bar` 用 `core.instrument.asset_class='ETF'` **当前值**选取全历史，没有历史在市名单、`listing_date`/`delisting_date` 或版本可知时点。该流程确定有幸存者偏差风险；历史收益适用范围待限定。
- `core.instrument.first_observed_date` 是第一次本地观察日期，不是交易所上市日期；`observed_at` 是采集时间，不是历史公布时间。ETF 分类修订虽有 `effective_date`，快照查询未按历史有效/可知时刻过滤。
- 原始 ETF 价对分红/拆分的可比收益如何定义仍未解决；`research.daily_bar_qfq` 没被 ETF 回测读取。分红总回报或复权价不能自行假定。
- 缺行情时最后收盘价估值；新版快照将交易状态用于成交门槛，旧快照仍兼容 bar-only 行为。真实状态源的历史完整性/可知时点与 side-specific 涨跌停规则未验证；开盘成交没有独立“订单已提交/部分成交”语义。
- 基准图的日期集合可能与权益曲线不一致；任何超额、跟踪误差和信息比率都需先确立交集/现金流/分红口径。

## 变更验收

时序变更必须用至少 3 ETF / 10 个交易日的已知价格、信号、下一日开盘、费用与逐日权益测试证明；特别验证 T 日 close 不会以 T 日价格成交、无行情日、最后一日 pending 和基准日期对齐。
