# 池内动量 Top-K 轮动（MVP-7 T15）

`/pools` 选择已发布固定池版本与 Dataset、逐成员绑定复权版本，配置动量窗口、
Top-K、调仓频率与本金/成本，经现有 `prajna-freeze-selection` →
`POST /api/v1/tasks/selections` → 持久任务 → Rust Fast Event → 持久结果，得到
可保存、可重开读取的池内动量 Top-K 等权轮动回测。浏览器只提交选择并读取持久
结果，不计算因子、费用、数量或绩效。旧 `s2_rotation`（九参数多因子 RotationScore）、
`single_etf_timing`、买入持有及旧诊断身份保持不变。

## 参数与信号

模板为 `pool_momentum_topk`，`params_version: 1`：

| 字段 | 含义 | 页面默认 |
| --- | --- | --- |
| `lookback` | 动量窗口 Session 数：`close_t / close_(t-n) - 1` | 60 |
| `top_k` | 目标入选数量；可大于池成员数 | 1 |
| `rebalance_every` | 从冻结评价起点起每 N 个 Session 形成 Decision | 5 |

复用现有 Rust `Momentum` Factor 与 `TopKRank` 排序/执行语义。T 日收盘用绑定的
供应商复权收盘形成信号，下一可用 Session 开盘尝试成交；成交、估值、费用和
公司行动仍使用原始 Bar（ADR 0030/0031）。未来收益标签不进入信号。缺失、
未知可用时刻或晚于决策收盘的因子值不参与排名。

每个计划调仓 Session 先收集状态为 `ok` 且 `available_at <= Session close` 的
成员，按动量降序；**并列按 `InstrumentId` 升序**（沿用已有 TopKRank 契约）。
实际入选数为 `min(top_k, 合格成员数)`，对实际入选数量等权（`1 / count`）。
零个合格成员时目标为空集合，即下一 Session 清仓为全现金；后续调仓可再入场。
参数统计窗口不足的成员只从其首次可评估 Session 起参与，不填零、不以原始价
替代复权研究序列。

共同评价窗口按 [ADR 0031](../decisions/0031-common-evaluation-window-and-adjusted-binding.md)
与 [评价窗口契约](contracts/evaluation-window.md) 冻结：预热为 `lookback`
Sessions，评价起点是全部成员和全部参数组合都就绪的第一个 Session；轮动
周期锚定该起点，不把更早的预热 Session 当作调仓日。预热无 Decision、成交或
绩效；窗口末 Session 的 Decision 没有下一 Session，保留 pending 不成交。

## 冻结、执行与现金

选择请求必须逐成员绑定 `adjusted`，冻结定义保存 `adjusted_versions` 与
`evaluation`；参数、成本、本金和手数来源一并进入 Experiment/Run 身份。
`Experiment::parse` 与 native 冻结入口重新读取绑定、重新规划窗口并核对评价
对象一致，历史不足、窗口不一致、缺绑定或 `accurate_event` 均显式拒绝。
仅支持 Vector/Fast Event；池内单 Venue 与沪深同池均沿用既有真实 Venue 集合，
不伪造或合并 Venue。

执行沿用 Fast Event `full_target` + `unfilled_entry: skip`：每个 Decision 的
目标只含当次选中的成员，长仓、不融资、不做空。受阻买腿（如 HALTED）跳过，
其目标资金留现金，不重新归一化给其他腿，也不在后续 Session 自动重试；后续
调仓以新 Decision 替换旧目标。目标数量按原始开盘价计算、按证券自身 lot 取整，
成交价再叠加配置滑点与佣金/最低佣金/税；实际可用现金不足时按 lot 减量。整数
余量现金保留，股票数量不继承旧三只、61 Session 或成交额诊断门禁。

跨资产池中的黄金 ETF 只在当次排名入选时出现在目标与持仓中；未入选时目标
不含黄金。模板只读取一个冻结 StaticUniverse，不读取或转移其他池资金，也不
建立独立黄金仓位。

## 页面与结果

`/pools` 的“预检策略”选择 `pool_momentum_topk` 后显示动量窗口、Top-K、调仓
频率和本金/成本输入（默认 60 / 1 / 5、1,000,000 CNY、佣金 3bp、最低佣金 5、
买卖滑点各 2bp、税 0，均为模拟假设）。预检仍由 Rust 计算共同窗口；提交后
页面显示冻结模板参数/成本，并链接到 `/backtests?task=…` 读取同一持久任务。
本模板使用 `full` 保存实际成交、费用、订单/延期/跳过和期末 pending 审计；
其它策略仍保持各自允许的保存级别。页面与结果只读取 Rust 持久值，不重算。

## 验证边界

`crates/prajna-experiment/tests/rotation.rs` 通过正式合成发布 → 冻结 →
`Experiment::execute` 覆盖：并列按 InstrumentId 选择、`top_k` 大于成员数时
按实际数量等权、不同 lot 的数量/余量现金/最低佣金、T 收盘到下一 Session
开盘、内部复权缺口导致零候选全现金并再入场、受阻买腿留现金、黄金仅入选后
持有，以及完整 Research binding/参数拒绝。`crates/prajna-research` 的
`strategy_variants` 覆盖同一排序/频率/全现金 Decision 的底层语义。
`apps/web/e2e/rotation.spec.ts` 经真实生产服务/CLI 提交固定池、冻结参数/成本、
执行、读取持久估值和 Full 成交，核对页面与保存定义并刷新重读。

公开证据仅使用合成数据与合成 adjusted/status 输入；真实行情、严格 PIT、
完整公司行动总回报、基准对照与盈利结论不在本契约验证范围。策略—基准比较
仍由已交付的 #375 契约承接（`single_etf_timing` 与旧 `s2_rotation`），本模板
的比较接入不在本票范围。
