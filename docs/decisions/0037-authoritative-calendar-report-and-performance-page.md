# ADR 0037：权威日历收益/回撤报告与完整绩效页面

- 状态：Accepted implementation contract，2026-10-31；#379（#309 B），消费 ADR 0034–0036。
- Rust 生成并持久化报告；浏览器只读取已保存值，不重算绩效。

## 选择与身份

`complete-performance@1` 的 `summary` 绑定增加可选 `"calendar":"calendar-performance@1"`；
选择参与规范化 Experiment/Run 身份，省略时保留 #378 的定义、身份、Summary JSON/Arrow 内容与
replay/promotion。选择沿用完整 Summary 门槛（单 Venue Static Fast Event、冻结 evaluation/公司行动、
252 Session/年），不合法版本在解析期拒绝。

## 持久化与读取

报告写入既有 `summary.parquet.summary_json.complete.calendar`，沿用 Summary 逻辑 hash、
`read_run_table`、`result`、replay/promotion，不新增表、结果级别或账户/执行核心。旧存档无该字段时
读取为缺失，不当作零收益或零回撤。

## 数学与日历

评价端点为首末 close Session；期初资本是 `fast_event.initial_cash`。points 保存每个 close 的
scale-18 权益、`nav=equity/initial_capital` 与 `drawdown=nav/历史峰值-1`，权益恒等于现金+持仓市值+
分红/清算应收。最大回撤按 #378 的逐 close 规则取首次严格峰谷；期初即为峰值时峰值标
`initial_capital`，无回撤时峰谷为空且最大回撤为 0。

月/年按 close Session 日期分组，收益为区间末 close / 起点 -1。首段起点是初始资本（保留首日成本，
不把首日权益重置为 1），后续起点为上一包含区间的末 close；区间显式保存 `start_basis`、起止
Session 与收益。没有 Session 的月份不生成区间、列入 `missing_months`（不补零），其后收益从上一
包含 close 桥接；不足整月/整年由端点表达。空评价窗口或公司行动事实不完整时 `available=false` +
原因（`empty_evaluation` / `corporate_actions_incomplete`），不输出 points/区间/回撤；账户恒等式
或非正分母失败仍使执行失败。

## 对照集成与页面

`comparison::freeze` 为两角色注入完整 Summary+日历报告，只在策略 Summary 绑定期初持有基准
（嵌入定义不含嵌套 summary），基准收益/超额由 Rust 产生。`comparability.performance` 保存共同版本
并纳入 checks；读取侧核对报告语义/估值时点/端点/初始资本/点数与 Summary 峰谷一致，缺失或不一致
409，行动不完整转 `comparable=false` 原因。旧对照任务无报告仍可读，页面显式标注缺失。

页面从 `result` 读取指标 value/reason、账户币种金额、费率/换手比例、事件次数、回撤峰谷与月/年
区间；金额不转浮点，费率只做显示格式化；缺失/不可用/不完整/损坏均显式，不以零或成功替代。
图表直接绘制 `account_valuations.close.equity` 与 `calendar.points.drawdown`；页面“净值”即该
close equity，归一化 `nav` 与其同源，不重复绘制。

## 限制

只验证合成 DSV/对照 golden、前端构建/类型检查与 Rust 读取校验；生产浏览器 E2E 已扩展但当前沙箱
未执行（需在 `ps` 可见的正常主机补跑），不构成真实总回报、完整 PIT、公告全集或盈利能力验收。
仍限单 Venue Static Fast Event 与既有结果级别。
