# 单 ETF 严格趋势择时（MVP-7 T14）

`/backtests` 选择 `single_etf_timing`，显式选择已发布 ETF Dataset 与该证券的
复权版本，经现有 `/api/v1/tasks/selections` 冻结并提交。任务状态、Summary、
NAV/权益读取现有持久任务与结果入口；刷新按 task 重读，无浏览器量化计算。
买入持有及旧 `/api/v1/runs` 路径和身份保持不变。

## 信号与时间

参数为 `params_version: 1`、`ma_window`（正 Session 数，页面默认 60）、
`rebalance_every`（正 Session 数，默认 5）。这是单均线水平判定，不是已有
`ma_crossover` 双均线交叉。复用 `TrendFilterStrict@1` 的尾随含当日窗口：
`close > mean(close[t-n+1..t])` 才持有，等于或低于为空现金目标。因子使用
T10 的相等容差 `1e-12 * max(1, |close|, |mean|)`，不改变 TrendFilter v1 的 `>=`。
未来收益标签不参与信号。

显式绑定的供应商复权收盘序列只进入独立信号 Panel，成交、估值及费用使用
原始 Dataset；不把复权收益再加到账户。沿用 [ADR 0030](decisions/0030-supplier-adjusted-research-series.md)
的回溯性/收盘可用假设，`observed_at` 不是历史发布时间，不宣称严格 PIT。
仅提供 Vector/Fast Event；Accurate 拒绝本模板，不扩张 Backend 范围。

共同窗口按 [ADR 0031](decisions/0031-common-evaluation-window-and-adjusted-binding.md)
冻结：至少 n 个完整历史收盘才能就绪，预热无 Decision/成交/绩效。固定评价
起点为第一个调仓 Session，此后每 N Session 形成目标；缺失或未知/晚于决策
收盘的可用时刻使该次信号不可用，不制造现金卖出，不压缩或平移后续日历。
策略只接受单 ETF；缺复权绑定、非法版本/参数、历史不足均显式拒绝。

事件例：均线窗口 2、频率 2，复权 closes 为 `[10,12,12,12,8,8]`。
第 1 Session 只预热；第 2 收盘 12>11，目标长仓，第 3 开盘尝试买；第 4
收盘 12=12，目标现金，第 5 开盘尝试卖；若该开盘 HALTED，则第 6 开盘重试。
非调仓 Session 的新数值不产生目标。最终 Session 的目标保留 pending，不在
窗口外成交。新调仓目标替换旧延期目标；受阻卖腿延期，受阻买腿跳过且资金
留现金，沿用原有 `unfilled_entry=skip` 与 full-target 执行层。

## 冻结与成本

原生参数、adjusted versions、评价窗口、本金、成本进入 Experiment/Run 定义与
身份；证券手数来自冻结 Dataset 元数据，非所有证券硬编码相同值。长仓、
不融资/做空，现金收益零；整数手数余量现金保留。

页面默认本金 1,000,000 CNY、佣金率 0.0003、最低佣金 5 CNY、买/卖滑点各
2bp、买/卖税率零，均可修改并保存，仅为模拟假设。择时页面使用 Full 保存
原始成交、实际 commission/tax/slippage、订单/延期及期末 pending 审计，NAV
页面读取账本；现有 Summary 的 `total_cost` 为累计成本率，不是现金费用金额。
不继承旧最少三证券、61 Session 或成交额诊断门禁。行动事实及完整总回报、
基准与额外指标仍依对应 MVP-7 票验收，本模板不扩大其验证结论。

## 验证边界

`crates/prajna-experiment/tests/timing.rs` 的 10 Session 独立十进制手算：原始
开收均为 10，复权序列产生不同信号；900 份买入价 10.01，佣金 5、买税
9.009，余款 976.991；卖价 9.99，佣金 5、卖税 17.982，现金 9945.009。
逐日检查权益=现金+数量×原始估值价，应收为零；含 equality、预热、频率、
现金、成本/手数、HALTED 延期和末尾 pending。研究测试核对缺失/未知可用时刻
不推断现金或改变日历；生产 Playwright 测试检查提交、冻结定义、持久值与
页面一致及刷新重读，并保留买入持有/旧诊断兼容测试。公开证据仅为合成输入；
真实历史可用性、PIT Universe 与策略盈利未验证。
