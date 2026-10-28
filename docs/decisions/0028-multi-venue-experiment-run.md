# ADR 0028：多 Venue Experiment 定义、Run Spec 与 Run 边界

- 状态：Accepted implementation contract，2026-10-28（Issue [#347](https://github.com/ZombieIce/PrajnaQuant/issues/347)）。
- 关系：[ADR 0026](0026-multi-venue-research-panel.md) 固定共享 Session 轴面板，[ADR 0027](0027-multi-venue-engine-semantics.md) 固定 T06b1 的引擎/账本语义；本文只固定生产 Experiment 定义、Run Spec 身份与 Run 边界如何消费前两者。T06b1 的 C1 预算与拆分见 #294 的负责人裁决。

## 背景

`load_panel_for_venues` 与 `fast_event@13/@14`、`vector@10` 已就绪，但生产 `prajna-experiment run` 的定义仍只接受一个 `venue` 字符串：`EngineSettings` 按单 Venue 过滤 Instrument 并构造单 Venue `FastEventConfig`，面板仍用 `load_panel`，`Universe` 校验只允许唯一 Venue，Accurate 在 Experiment 边界没有显式多 Venue 拒绝。ADR 0027 明确把 C2 留给 #347。

## 决定

1. **显式 Venue 集合。** 定义必须给出 `venue`（旧单 Venue 拼写）或 `venues`（数组）之一，不能同时给；集合非空、无重复、每项是合法 `VenueId`。单元素集合归一化为旧的 `venue` 字段，多 Venue 归一化为排序后的 `venues` 数组；因此 `venue:"X"` 与 `venues:["X"]` 的 Experiment/Run 身份相同，旧单 Venue 定义、Run Spec、Experiment/Execution 身份与已保存结果不变。
2. **Run Spec 记录真实 scope。** 单 Venue Run Spec 继续使用 `venue` 与既有语义版本；多 Venue Run Spec 写完整 `venues` 集合与 T06b1 版本：Fast Event `vector_parity` 为 `fast_event@13`、`lot` 为 `fast_event@14`，静态 Vector 为 `vector@10`。Run ID 仍由规范化 Run Spec 的内容地址得到；不按 Venue 拆分 Run 或身份。
3. **生产面板入口与结构性错误。** Experiment 执行只通过 `load_panel_for_venues` 加载所选集合；每个 Venue 必须有 Instrument 与 Session，共享 Session 轴的日期集合和开收盘时刻必须一致。缺 Venue/Instrument 或轴不一致是结构性错误：整个调用失败且不写 Execution，不降级为单个 Run 的 failed 状态，也不静默选第一个 Venue。
4. **Universe 与 Venue 一致。** Static Universe 的每个成员必须属于所选 Venue；Universe 成员不在 Dataset、成员 Venue 未被选择、显式 lot 不属于所选 Venue 都显式失败。多 Venue 只支持非 PIT、价格收益、`full_target` 的 Fast Event/Vector；PIT 仍要求唯一 `SZSE`，Python 策略仍限单 Venue。
5. **Accurate 与 adapter 边界。** `accurate_event` + 多 Venue 在定义解析时拒绝；adapter `RunPacket` 继续要求单 Venue。不在本票扩展 Accurate 能力。

## 验证与限制

- 合成 2 Venue × 3 Session Dataset 经正式 `publish_dataset` 发布，CLI 创建/重放/读取持久结果；手算现金、495 股/交易所、佣金 50、9,900/9,999 权益作为独立 golden。负例覆盖空/重复/冲突 Venue 字段、缺 Venue、共享轴不一致（结构性失败且无存储写入）、Universe/Venue 不匹配、Accurate 与 adapter 拒绝。
- 未读取或公开真实行情、日历、账本/NAV、hash 或 Run/Execution 身份；无页面/浏览器验证。真实沪深日历一致性、多 Venue PIT/公司行动/终止结算与多 Venue Python 策略不在本文范围；多 Venue PIT fixture 仍按 #347 的 S2 记录 defer 到 #307 方向。

## 被拒绝的备选方案

- **把 `venue` 改成数组或让其缺省取 Dataset 第一个 Venue**：破坏旧单 Venue 身份，并引入静默选 Venue。
- **按 Venue 生成多个 Run/Execution，或多 Venue 复用单 Venue 语义版本**：前者改变身份与比较语义，后者违反 ADR 0018 并让相同 Run Spec 在不同引擎语义下产生未解释差异。
