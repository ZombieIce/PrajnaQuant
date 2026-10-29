# 共同评价窗口与冻结复权研究输入（T08a #297）

本契约固定 T08a 的行为：单交易所池（或显式启用窗口的单证券）逐成员绑定 T07
不可变 adjusted version，按冻结复权序列的真实 Session 覆盖与全部比较参数的
最大预热计算共同评价窗口，把它写入 Experiment 定义并进入 Experiment/Run
身份；Rust 执行只评价窗口内 Session（预热无 Decision/Fill/绩效）。预检 CLI/
HTTP 与 `/pools` 页面由 [#356/T08b](https://github.com/ZombieIce/PrajnaQuant/issues/356)
按下述协议消费同一 Rust 规划结果。

成交与账户估值继续使用原始 Bars（Spec D5、[ADR 0030](../decisions/0030-supplier-adjusted-research-series.md)）；
复权序列只作研究价格输入与窗口覆盖依据，不进入成交价、估值、公司行动或分红现金。

## 选择输入

`prajna-freeze-selection` 与 `POST /api/v1/tasks/selections` 的 `Selection` 新增可选
`adjusted` 对象：键为完整 `InstrumentId`，值为 `adj:sha256:<64hex>`。

```json
{
  "dsv": "dsv:sha256:<64hex>",
  "pool": {"id": "dividend", "version": 3},
  "strategy": "buy_and_hold",
  "adjusted": {
    "510300.XSHG": "adj:sha256:<64hex>",
    "510880.XSHG": "adj:sha256:<64hex>"
  }
}
```

- **池提交必须绑定**：键集合必须与所解析 StaticUniverse 成员完全相同（无缺失、
  无多余、无重复）。单证券提交可省略 `adjusted`，此时保留 T04b/T04d 原始价
  Buy & Hold 旧定义与旧身份；只要提供 `adjusted`，该单证券任务也进入本契约的
  窗口/校验路径。
- 每个版本经 `prajna_data::adjusted_series::read` 读取并校验：可读、原始 DSV
  精确等于选择的 DSV、`instrument_id` 等于绑定证券、至少一个复权行。缺失
  （`adjusted_version_missing`）、损坏/非法版本（`adjusted_series_invalid`）、
  raw DSV 不符（`adjusted_raw_dsv_mismatch`）、证券不符
  （`adjusted_instrument_mismatch`）、空序列（`adjusted_series_empty`）全部
  fail closed；不自动选 latest，不回退其他版本或原始价。
- 池版本、DSV、逐成员版本、策略与成本共同固定输入；池/数据集/参数/版本任一
  变化都产生新的冻结定义与身份。

## 共同窗口、预热与评价窗口

以固定 DSV 的 Venue Session 轴和绑定复权序列的实际 rows 计算：

1. **成员研究覆盖**：每个成员在 Session 轴上的复权行存在性（缺失为 false）。
   原始 Bar 缺失不填补；复权行必然对应原始 Bar/Session（T07 导入校验）。
2. **共同数据窗口**：`common_start` = 所有成员首个复权行的最大日期，
   `common_end` = 所有成员最后一个复权行的最小日期。缺失 Session 不删除、
   不压缩日历、不填零，也不以原始价替代研究信号。
3. **参数预热**：按冻结定义的每个参数组合声明窗口：`buy_and_hold` 0；
   `ma_crossover` `long_window - 1`；`single_etf_timing` `ma_window - 1`；`s2_rotation` `max(short, long, vol, trend - 1)`
   （`trend` 可选；Momentum/Volatility 需要 `n + 1` closes，TrendFilter 需要 `n` closes）。
   Python、PIT 与无声明窗口的策略显式拒绝。预热 Session 数不等于观测数：
   `warmup_sessions = required_observations - 1`。
4. **参数就绪 Session**：从 `common_start` 起，首个使该参数所需尾随
   `warmup_sessions + 1` 个 Session 对所有成员都有复权行的 Session。复权
   缺口只会把就绪点后移，不删除 Session。
5. **评价窗口**：`start` = 所有参数中最大的就绪 Session；`end` = `common_end`。
   至少需要两个 Session（`evaluation_window_too_short`）；任一参数历史不足时
   该组合 `not_runnable`（`insufficient_warmup`），整个冻结提交拒绝
   （`evaluation_window_not_runnable`）。评价窗口内部的复权缺口保留为 warning
   与计数，不阻断，也不改变日历。
6. **计划输出**（`WindowPlan`，`common-window@1`）：语义版本、成员首末复权
   Session/行数、逐参数预热与可运行状态、最大预热、实际使用的预热 Session
   数、共同起止、评价起止、`runnable`、`blocking` 与 `warnings`。`selection::plan`
   以库函数形式提供同一计划（供 T08b 复用），缺失/非法绑定返回结构化 blocking
   而不是异常退出。

## 冻结定义与身份

窗口可运行后，选择定义新增两个字段：

```json
"adjusted_versions": {"510300.XSHG": "adj:sha256:<64hex>"},
"evaluation": {
  "semantic_version": "common-window@1",
  "warmup_sessions": 0,
  "common_start": "2026-01-05",
  "common_end": "2026-01-07",
  "start": "2026-01-05",
  "end": "2026-01-07"
}
```

两者进入 Experiment 的规范化身份和 Run Spec（`run:sha256`），后续执行不读取
latest、不以运行时重算结果替换冻结窗口。`Experiment::parse` 在执行侧重新读取每个绑定版本并
校验上述条件，同时要求 `common_start <= start < end <= common_end`、每个版本
的首/末复权行覆盖评价窗口；失败即拒绝定义（fail closed）。没有这两个字段的
旧定义保持原身份与原行为；只有 `adjusted_versions`、没有 `evaluation` 的对象不是可执行定义，
`Experiment::parse`/CLI/执行入口拒绝它（planning-only），持久化选择始终同时写入两者。PIT、多 Venue、
Python 与 Accurate 定义不支持共同评价窗口。

`single_etf_timing` 在原生 `Experiment::parse`/CLI 边界还以全部 list/grid 参数与
冻结复权覆盖重新规划并校验就绪条件：任何组合历史不足，或定义中的预热/共同
窗口/评价窗口与计划不一致，均拒绝；不默默改写冻结窗口或把无法就绪的参数
报告为成功现金回测。旧策略的定义与身份保持原行为。

## 执行边界

Fast Event 与 Vector 在 Run 开始时把完整 Panel 切片为 `[start, end]` 的账本
Panel：

- 账本、`sessions`/`account_valuations` 与 Summary 只包含评价窗口 Session，
  从窗口第一个 Session 开始，不经额外归一化；窗口之前不产生 Decision、
  Fill 或绩效点。
- Buy & Hold 的建仓锚点改到窗口第一个 Session（Decision 在该 Session 收盘，
  下一 Session 开盘尝试成交）；其它 native 策略仍在完整 Panel 上计算预热
  指标，只把窗口内 Decision 交给账本。
- 窗口末 Session 的 Decision 没有下一 Session，不成交（按 pending/期末处理）；
  窗口之后不执行、不估值、不进入结果。
- 成交价、数量、佣金/滑点/税、公司行动与账户恒等式完全沿用既有原始价账本；
  不改变 Fast/Vector 语义版本、成本或公司行动规则。窗口是身份承载的定义
  输入，不是引擎规则变化；没有窗口的旧定义结果不变。

## 错误与兼容

选择层新增错误码：`adjusted_version_missing`、`adjusted_series_invalid`、
`adjusted_raw_dsv_mismatch`、`adjusted_instrument_mismatch`、
`adjusted_series_empty`、`evaluation_window_not_runnable`。HTTP 映射：raw DSV/
证券不符 400，缺失/空序列/窗口不可运行 422，损坏对象 500；既有根/Origin/
幂等与 T01 提交行为不变。成功 envelope 在窗口路径新增 `adjusted` 与
`window`；旧单证券 envelope 字节级保持不变。

## 验证范围

公开验证只用合成数据：窗口数学手算矩阵（交集、晚上市/早结束、预热内缺口、
历史不足、单 Session、空覆盖）、逐成员绑定失败分支、定义身份/旧定义兼容、
真实 `prajna-experiment run` 的 Fast Event 与 Vector 合成 golden（复权覆盖
截断到第二 Session、预热 Session 被排除、手算现金/数量/NAV/成本、窗口末不
执行）。真实行情、复权来源、完整 PIT 与公司行动总回报未验证，仍只留本机。

## T08b 生产预检与页面提交

`prajna-freeze-selection --lake <LOCAL> --plan` 消费上述 Selection JSON，退出 0
返回 `{schema_version:1,data:{selection_plan_version:1,dsv,strategy,venue,universe,
adjusted,pool?,instruments,window,runnable,blocking}}`。预检不写 Universe/任务；
`window` 是上述 WindowPlan，缺绑定时为 null，`blocking` 包含 code/message。
窗口或绑定不可运行仍是成功读取的预检（runnable=false），不是命令错误。
未知池、DSV 缺成员、混合 Venue、损坏底层数据等无法读取选择的失败保持原错误
退出 2/envelope；不把错误伪装为空覆盖或零值。

Selection 可选 `parameter_space` 沿用 Experiment 的 native `list` / `grid`，
`strategy` 接受 buy_and_hold、ma_crossover、single_etf_timing、s2_rotation。只有 buy_and_hold
省略时使用旧唯一 `{params_version:1}`；其它策略必须显式给出参数并绑定复权版本。
参数由现有 native parser 验证，S2 沿用成员数/因子/策略校验；预热来自 Rust 声明，
不接受客户端 warm-up 数。买入持有仍是 entry_only/retry，其它模板为既有
full_target/skip，不改变执行实现。此处只提供统一选择输入，不交付策略模板产品页
或参数比较报告。旧单证券省略新字段时定义、身份及响应保持兼容。

`POST /api/v1/tasks/selections/preflight` 使用同一 Selection 字段（可省略
idempotency_key），沿用同一 dataset/output 根、大小、Host/Origin、no-store
和错误状态映射，经子进程调用 `--plan`，成功返回 CLI envelope，不排队。
`POST /api/v1/tasks/selections` 可附 `expected_window`，若与 Rust 重新读取同一
不可变输入得到的 WindowPlan 不一致，返回 409 selection_plan_changed，不创建任务。
预检与冻结共享 plan_window；提交持久化 adjusted_versions/evaluation，无 latest。

`/pools` 从所选 DSV 的复权目录按成员列出固定版本，不自动选择/替代。显示池
id/version、Universe、DSV、逐成员版本、共同数据窗口、最大/实际预热、评价窗口、
逐组合状态与阻断原因、缺口 warning。页面只渲染 Rust 结果；输入改变即使旧请求
返回也无法启用提交，必须重新预检。提交携带显示结果的 expected_window；刷新
保留显式选择但不复用旧预检，已保存任务仍消费固定定义，不跟随池草稿或目录更新。
公开验证为合成 CLI/HTTP/浏览器路径；真实行情与完整 PIT/总回报未验证。
