# ADR 0036：版本化完整绩效 Summary

- 状态：Accepted implementation contract，2026-10-31；#378（#309 A），消费 ADR 0033–0035。
- Rust 权威；只交付 Summary，不交付日历序列或页面。

## 选择、身份与生产路径

正式 Experiment 定义可选 `summary: {"semantic_version":"complete-performance@1"}`。
仅支持已有冻结 evaluation/公司行动的 Static、单 Venue、lot/full_target Fast Event，
252 Session/年；其他版本、周期或引擎拒绝。不声明时完全保留 E11 定义、Run/Experiment
身份、Summary JSON/Arrow 内容、replay/promotion。新选择进入规范化 Experiment 和 Run 身份。
`prajna-experiment run definition.json --level summary|standard|full` 生成并保存
`summary.parquet.summary_json.complete`；`read_run_table(...,"summary")` 与 `result` 原样读取，
replay/promotion 同样校验逻辑 hash。现有平面指标列对应新定义；新增字段在 complete JSON，
不新增或改写旧表列。Summary 级别不声称保存成交明细。

可选 `summary.benchmark_definition` 接受 ADR 0035 冻结出的原始持有基准 definition。
拒绝嵌套 summary、非 holding benchmark、多 Run、不同数据/Universe/Venue/复权/窗口/
可用性/公司行动/成本/本金/portfolio/数量规则；复用 ADR 0035 的全窗口买价 tick 校验。
再投资日历由该冻结定义显式保存，不从未来成交重建。该输入的规范化定义进入 Summary
身份；执行复用现有 holding Fast Event，内部生成同版本 close Summary，不另造账户核心。
无绑定时 benchmark_return/excess_return 为 null + benchmark_not_bound，不暗选基准。

## 数学、端点与单位

评价起点是第一评价 Session 开盘前 initial_cash，随后每 Session close 的账户权益；
应收计权益但不可花费。T close Decision → 后续可用 Session open 执行的时序不变。
例如本金1000、第一日含成本权益985：首收益=-1.5%，不把985重置为净值1。
每个 close 的现金+原始持仓市值+分红/清算应收由既有 scale-18 账户验证。
对 n 个 close，r1=E1/C-1，rt=Et/E(t-1)-1；total=En/C-1，annual=(En/C)^(252/n)-1。
波动为全部 n 个收益的样本标准差×√252；Sharpe=mean/std×√252，无风险为零。
最大回撤从初始净值1逐 close 求最小 NAV/历史峰值-1，保留首次严格峰谷；
峰值在期初资本时用 initial_capital 标记。Calmar=annual/abs(max_drawdown)。
benchmark_return 用相同端点/本金/费用/数量/总回报口径；excess_return=strategy_total-benchmark_total，
是收益率差，不是相对财富比。complete 保存币种、评价首末 Session 和期初资本。

amounts 是账户币种的 scale-18 十进制字符串：commission、minimum_commission_top_up、
tax（成交税）、slippage 和 total_execution_cost；最低佣金补差单列且仅加一次。
滑点取账户 Fill/fee settlement 的 raw 与执行价差归因：lot 已含在执行价，额外现金费用
模式另扣；汇总只计一次，不再次扣 NAV。分红税已在净应收中，不称成交税或重复计执行费用。
cumulative_cost_rate 为逐 Session 实际执行费用/pre-open pre-trade equity 的和，
cumulative_turnover_rate 为原执行器成交基数/相同分母的和；二者均非现金金额。
旧 total_cost 仍是累计归一化费用。fill_count、fee_settlement_count 与既有执行/延期次数分别保存。

每个 complete.metrics 项有 value/reason；缺样本、零波动的 Sharpe、零回撤的 Calmar、
非有限结果或未绑定/不完整基准均明确原因。有效零波动/零回撤本身仍可为0。
行动事实不完整时收益/风险指标和超额为空并带原因，成本诊断保留；不合法账户/非正分母
使执行失败，不产生成功 Summary。只验证合成账户与生产保存读取，不构成真实总回报验收。
