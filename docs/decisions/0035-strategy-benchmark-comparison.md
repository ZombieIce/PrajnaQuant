# ADR 0035：冻结策略—基准对照

- 状态：Accepted，2026-10-30；#375，消费 ADR 0031/0033/0034。
- 范围：本机产品入口、持久对照任务、只读对照结果与页面；不新增账户、绩效或执行核心。

## 冻结与身份

正式比较只从冻结策略派生对应持有基准：既有 `prajna-freeze-selection` 冻结策略
（`adjusted_versions` + `evaluation` + `corporate_actions`），Rust `comparison::freeze`
再复制同一 DSV/Universe/Venue/成本/本金/可用性/复权/窗口/行动版本并改写为
`buy_and_hold + holding-benchmark@1`，保存显式再投资日历。支持 `single_etf_timing`
（恰一 ETF → `single_etf`）与单 Venue `s2_rotation`（冻结池 → `equal_weight_pool`）。
冻结策略必须恰展开一个 Run；多参数、多 Venue、Vector/Accurate/PIT、缺公司行动/
窗口/复权版本或已带 `holding_benchmark` 的定义拒绝，浏览器不能提供任意基准定义。

`comparison.json` 是任务请求定义，保存两个角色、各自 `definition`、预期
`experiment`/`run` 身份、`semantic_version` 与 `comparability`。任务 `request_sha256`
由该对象内容计算；同 `idempotency_key` 不同定义返回 409，不以 latest Run 或新数据
重配旧比较。结果级别固定 `standard`：两角色都保存 Summary 与 `account_valuations`；
持有基准按 ADR 0034 另含 orders/fills 等追溯表。

## 可比性与再投资日历

`comparability` 保存并核对 DSV、Universe、真实 Venue、成本五项、最低佣金、
`initial_cash`/portfolios、`sessions_per_year`、可用时刻假设、逐成员复权版本、
共同 evaluation、公司行动版本/税率、`fast_event.sizing/lot_size_source`、基准已解析
lot/size/price 规则和收益口径；任一项不一致即拒绝冻结或读取失败，不补日、不缩短
窗口、不以 latest 或另一成本模型替代。

再投资日历在运行前声明并保存：`strategy_rebalance` 从冻结策略 Run 的
`rebalance_every` 与评价窗口派生评价起点后的真实 Session close 序列，作为 #374 的
normal decision closes，下一 Session open 只分配当时可用现金；发放前应收不可花，
已有持仓不卖出恢复权重。`none` 显式空日历，分红留现金。页面/API 必须显式选择。

## 执行与读取

一个 `prajna_comparison` 任务在共享串行队列中依次以既有
`prajna-experiment run <definition> --level standard` 执行策略和基准，保存两个
execution 身份与 failed/run 计数；任一角色有 failed Run 或 CLI 失败则任务 failed，
不产生伪成功对照。服务重启把未完成对照标 `interrupted`，不自动续跑。

`GET /api/v1/tasks/comparisons/{id}/result` 对两个已保存身份调用既有
`prajna-experiment result`，核对保存的 `data.exp`/`runs[0].run_id` 等于冻结预期、
每角色恰一个 completed Run、Summary 存在、close 日期集相同且首末等于 evaluation
起止、`accounting_verified=true`、`return_basis=total_return` 与行动版本一致；损坏/
未保存表由 CLI 读取路径整体拒绝，不显示替代曲线。结构性不一致（身份、Run 数、
窗口、日期集）409 `comparison_result_mismatch`；缺基准角色 409
`comparison_benchmark_missing`。行动事实不完整仍返回两侧权威数据，但
`comparable=false` 与逐角色原因。页面直接绘制持久 `account_valuations.close.equity`，
不重算、不归一化首点、不填零；显示角色、窗口、本金、成本、行动版本与再投资日历。
旧 `/backtests` 单 Run 流程和旧结果身份不变。

## HTTP 边界

`POST /api/v1/tasks/comparisons` 接受 `{idempotency_key, selection, reinvestment_calendar}`，
经冻结 CLI 后创建 `prajna_comparison` 持久任务；同键不同定义 409，冻结拒绝按 code
返回 400/404/422/5xx。`GET ...` 列出/读取对照任务，`GET .../{id}/result` 返回
`{semantic_version, comparability, verification, strategy, benchmark}`，角色保留
`definition`、`exp`/`exe` 与既有 `runs`。服务不新增 `prajna-*` Rust 依赖，CLI 经参数
调用；成功与错误均 `no-store`，沿用本机 Host/Origin；不提供任意基准定义、latest
选择或失败后自动重配。

## 限制

只验证合成数据；不建立真实行情、完整 PIT、公告全集、真实总回报或盈利结论。
实现仍限单 Venue Static Universe Fast Event、一个策略参数组合与标准结果级别；
Vector/Accurate/PIT 与多 Venue 对照、通用指标比较平台不在本契约。
