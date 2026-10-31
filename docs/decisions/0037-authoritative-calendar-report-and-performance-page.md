# ADR 0037：权威日历收益/回撤报告与完整绩效页面

- 状态：Accepted implementation contract，2026-10-31；#379（#309 B），消费 ADR 0034–0036。
- Rust 生成并持久化报告；浏览器只读取已保存值，不重算绩效。

## 选择与身份

`complete-performance@1` 的 `summary` 绑定增加可选
`"calendar":"calendar-performance@1"`。选择参与规范化 Experiment/Run 身份；省略时
保留 #378 的定义、身份、Summary JSON/Arrow 内容与 replay/promotion。选择只适用于
既有完整 Summary 门槛：单 Venue Static、Fast Event、冻结 evaluation/公司行动、
252 Session/年；不合法版本在解析期拒绝。

## 持久化与读取

报告写入既有 `summary.parquet.summary_json.complete.calendar`，沿用 Summary 的
逻辑 hash、`read_run_table`、`result`、replay/promotion 路径，不新增表、结果级别或
账户/执行核心。旧存档无该字段时读取为缺失，不当作零收益或零回撤。

## 数学与日历

评价端点为首末 close Session；期初资本是 `fast_event.initial_cash`，与 #378 相同。
points 保存每个 close 的 scale-18 权益、`nav=equity/initial_capital` 与
`drawdown=nav/历史峰值-1`，权益恒等于现金+持仓市值+分红/清算应收。
最大回撤按 #378 的逐 close 规则取首次严格峰谷；期初即为峰值时峰值标
`initial_capital`，无回撤时峰谷为空且最大回撤为 0。

月度/年度按 close Session 日期分组，收益为区间末 close / 区间起点 -1。首段起点是
初始资本（保留首日成本，不把首日权益重置为 1），后续起点为上一包含区间的末 close。
每个区间显式保存 `start_basis`、起止 Session 与收益。评价窗口内没有 Session 的月份
不生成区间，列入 `missing_months`，不补零；缺失区间后的收益从上一包含 close 桥接。
跨月/跨年与不足整月/整年由端点表达，不修成完整区间。

空评价窗口或公司行动事实不完整时 `available=false` + 原因（`empty_evaluation` /
`corporate_actions_incomplete`），不输出 points/区间/回撤；账户恒等式或非正分母失败
仍使执行失败，不产生成功报告。

## 对照集成与页面

`comparison::freeze` 为策略与基准两角色注入完整 Summary+日历报告，并只在策略
Summary 中绑定期初持有基准（嵌入定义不含嵌套 summary），因此基准收益/超额由 Rust
产生。`comparability.performance` 保存两角色共同版本并纳入 checks；读取侧核对报告
语义/估值时点/评价端点/初始资本/点数与 Summary 峰谷一致，缺失或不一致返回 409，
行动不完整转为 `comparable=false` 的逐角色原因。旧对照任务无报告仍可读取，页面显式
标注缺失。

页面从 `result` 读取完整指标 value/reason、账户币种金额、费率/换手比例、事件次数、
回撤峰谷与月/年区间；金额不转浮点，费率只做显示格式化；缺失报告、不可用原因、
不完整或损坏均显示明确状态，不以零或成功替代。图表直接绘制持久
`account_valuations.close.equity` 与 `calendar.points.drawdown`。

## 限制

只验证合成 DSV/对照与生产浏览器闭环；不构成真实总回报、完整 PIT、公告全集或
盈利能力验收。仍限单 Venue Static Fast Event 与既有结果级别。
