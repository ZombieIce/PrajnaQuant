# ADR 0034：持有基准与声明日历现金再投资

- 状态：Accepted，2026-10-30；#374，扩展 ADR 0031/0033。

## 定义与身份

正式 Experiment JSON 在冻结 `buy_and_hold` 定义上显式增加：

```json
{"holding_benchmark":{"semantic_version":"holding-benchmark@1","kind":"single_etf","reinvestment_calendar":{"semantic_version":"session-close-calendar@1","decision_dates":["2026-01-09"]}}}
```

`kind` 为 `single_etf`（恰一 ETF）或 `equal_weight_pool`（冻结 Universe 全部为 ETF）。
只支持单 Venue Static Universe Fast Event、lot/full_target 输入，必须固定行动版本、
税率、逐成员 adjusted_versions 与共同 evaluation；省略此绑定保留旧行为和身份。
空 decision_dates 明确表示不再投资；非空必须严格递增、无重复，且为评价起点之后、
评价终点以内的实际 Session。末日声明允许，但保留 pending，不能伪造成交。

绑定及其日历、收益口径、分配/不可交易规则、证券价格/数量增量与原生 lot 随
Experiment/Run 身份保存；实际 lot、全部成本/本金/参数/窗口/DSV/行动版本仍由
既有 Run Spec 保存。新路径使用 fast_event@16，effective rebalance=cash_only、
unfilled_entry=skip；输入 full_target 仅作兼容的引擎配置，不代表实际恢复权重。
旧 fast_event@15 与 BuyAndHold 不增加日历决策。比较关联和页面由 #375 承接。

## 执行与现金

预热无决策、成交、绩效；评价首个 Session close 发期初买入决策，下一 Session
open 执行。日历列出正常调仓的 **Decision close**，其买入也只在下一 Session open。
行动顺序仍为除息应收/拆分 → raw open 执行 → close 估值、发放、登记 → Decision。
close 发放现金不可用于同日 open，应收不进入买入预算。

每次决策把当时可用现金等分给冻结池的全部成员；按 raw open 预算数量向下取 lot，
再按 InstrumentId 顺序处理实际买入。滑点后的买价向上取到该证券价格增量，佣金、
最低佣金和买税按实际成交额计算；总现金不足时按 lot 减量到可付费用。费用可能使
最后成员少一手，剩余现金保留，不额外重分配。已有数量直接加上新增量，绝不卖出
赢家恢复初始权重；拆分仍走原账户处理器与数量舍入记录。

不可交易/缺 Bar 的份额留现金，该次买入跳过；不在未声明日期重试，下一声明日历
重新分配当时现金。无可支付手数则保留现金/数量调整记录。每次可用现金含既有
整数余量和已支付分红，不只跟踪某笔分红；这属于现金再投资，不是主动权重再平衡。
缺价沿既有同字段最新价估值并保存来源，不造零价或成交；缺必要行动事实保留不完整标记。
收益使用 raw 价格加净现金行动，不叠加 adjusted 的收益。

## 持久证据与限制

新基准 Standard 保存既有账户/组合估值、行动、orders、fills、fee_settlements 表；
orders 的尝试记录额外标 initial_entry / calendar_cash_reinvestment，保留受阻、数量调整
与 pending_at_end。成交中的实际费用是权威，fee_settlements 无独立结算时为空。
既有 result/read_run_table 读取，权益与 NAV 按实际初始本金，不用首笔成交后权益重归一。
独立合成手算与正式 CLI 保存读取覆盖两类基准；没有真实总回报、完整 PIT 或公告全集验证。
