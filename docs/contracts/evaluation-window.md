# 共同评价窗口与冻结复权研究输入（T08a #297）

本契约固定 T08a 的行为：单交易所池（或显式启用窗口的单证券）逐成员绑定 T07
不可变 adjusted version，按冻结复权序列的真实 Session 覆盖与全部比较参数的
最大预热计算共同评价窗口，把它写入 Experiment 定义并进入 Experiment/Run
身份；Rust 执行只评价窗口内 Session（预热无 Decision/Fill/绩效）。预检 CLI/
HTTP 与 `/pools` 页面属于 [#356/T08b](https://github.com/ZombieIce/PrajnaQuant/issues/356)，
不在本契约。

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
   `ma_crossover` `long_window - 1`；`s2_rotation` `max(long, vol, trend) - 1`。
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
latest、不重新推导窗口。`Experiment::parse` 在执行侧重新读取每个绑定版本并
校验上述条件，同时要求 `common_start <= start < end <= common_end`、每个版本
的首/末复权行覆盖评价窗口；失败即拒绝定义（fail closed）。没有这两个字段的
旧定义保持原身份与原行为；只有 `adjusted_versions` 的“计划种子”仅用于
`Experiment::plan` 推导窗口，持久化选择始终同时写入两者。PIT、多 Venue、
Python 与 Accurate 定义不支持共同评价窗口。

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
