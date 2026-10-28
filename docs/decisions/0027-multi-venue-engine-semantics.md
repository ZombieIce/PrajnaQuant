# ADR 0027：多 Venue Fast Event/Vector 引擎语义

- 状态：Accepted implementation contract，2026-10-28（Issue [#294](https://github.com/ZombieIce/PrajnaQuant/issues/294)；增量拆分 C1/C2 与预算见该 Issue 的负责人裁决）。
- 关系：[ADR 0026](0026-multi-venue-research-panel.md) 固定共享 Session 轴面板；本文只固定 C1 的引擎/账本语义。生产 Experiment 定义、Run Spec 身份和 Accurate/adapter Run 边界由 T06b2 [#347](https://github.com/ZombieIce/PrajnaQuant/issues/347) 另行交付。

## 背景

MVP-7 的跨资产 ETF 池同时含 `XSHG` 与 `XSHE`。`load_panel_for_venues` 已能加载真实 Instrument 和完全一致的共享 Session 轴，但 Fast Event 的 `FastEventConfig` 与 `prajna-account::TradingAccount` 仍只接受一个 `VenueId`，任何其他 Venue 的 Order/Fill 都会返回 `WrongVenue`。负责人裁决 B3/B4 要求只扩展 Fast Event 与 Vector：单 Venue 结果和身份不变，仅沪深同池 Run 使用新引擎语义版本；Accurate/适配器遇多 Venue 必须显式拒绝。

本票只交付引擎/账本 seam，不改变 Experiment 定义输入或既有 Run Spec；C2 由 #347 承接。

## 决定

1. **Venue scope 来自面板与显式集合。** `FastEventConfig.venues` 和 `TradingAccount::venues()` 使用非空 `BTreeSet<VenueId>`；引擎校验面板 Instrument 的真实 Venue 集合与该集合完全相等，绝不静默选择第一个 Venue，也不从 Symbol 前缀推断 Venue。
2. **一个账本，不做分 Venue 账户。** 多 Venue Run 共用一套现金、持仓、应收和 Unallocated Capital；Order/Fill 仍按单一 `TradingAccount` 结算，不按 Venue 净额化或拆账。每次 Fill 的 Venue 必须属于该 scope，否则仍返回 `WrongVenue`；scale-18 half-even 及账户/VP 两层精确恒等式不变。
3. **只支持非 PIT、价格收益、full-target。** 多 Venue 语义仅用于共享 Session 轴的静态面板；PIT、公司行动、终止结算和 `entry_only` 在 C1 中显式拒绝，多 Venue 行动驱动应收不在本票（归 #306/#307）。缺失 Bar 仍是缺失，不补价、不伪造停牌或执行状态。
4. **新语义版本只用于跨 Venue Run。** 非 PIT Fast Event 的 `VectorParity` 为 `fast_event@13`，`Lot` 为 `fast_event@14`；静态 Vector 为 `vector@10`。单 Venue 既有版本（`fast_event@1..@12`、`vector@1`/PIT `@7..@9`）和既有结果不变；配置字段改型不进入 Run Spec/Execution 身份，因此旧身份不变。
5. **成本口径不变。** 佣金、最低佣金、税和滑点的计算仍按单 Venue 规则逐 Fill 执行；不引入汇率、跨 Venue 费用表或 Venue 特定交易规则，账户仍为单一币种。
6. **显式拒绝路径。** PIT/action/terminal 入口拒绝多 Venue 面板；外部 `run_lot_with_execution`（Accurate 边界）拒绝多 Venue；`RunPacket::validate` 继续拒绝多 Venue packet。错误必须可见，不得选一个 Venue 继续。
7. **Vector 是权重摘要。** Vector 可消费同一共享轴面板并使用 `vector@10`，但仍不声称现金/持仓守恒；与 Fast Event 的对拍只在比例成本、full-target、共同估值点的子集上进行。

## 验证与后果

- 合成 `publish_dataset` + `cn-stock-etf-daily@1` 面板经 `load_panel_for_venues` 进入两个引擎；手算 2 Venue × 3 Session 的现金、持仓、佣金和权益恒等式，并在 account 层用 `add_receivable`/`settle_receivable` 核验应收。
- 现有单 Venue `prajna-account`/`prajna-research`/`prajna-experiment` golden、Fast/Vector 对拍和 Run 身份测试继续通过；本票未改 Experiment 定义或 Run Spec。
- 不证明真实沪深日历、真实行情、多 Venue 公司行动、跨币种或生产 Experiment Run；这些分别属于 #295/#347、#306/#307 和真实证据流程。C2 完成前，生产 Experiment 仍不能提交沪深同池 Run。

## 被拒绝的备选方案

- **每个 Venue 一个 TradingAccount 再汇总**：会拆分现金与资本分配，无法保持单一权益恒等式，也会产生第二套账本语义。
- **沿用单一 `venue` 并只放宽 `WrongVenue`**：会让账本失去适用范围，任何 Venue 都能混入，且无法在结果中声明真实 scope。
- **为多 Venue 复用单 Venue 语义版本**：违反 ADR 0018 的“语义变化必须提升版本”；会让相同 Run Spec 在旧/新执行中产生未解释差异。
- **扩展 Accurate/适配器到多 Venue**：B3 明确排除，且会扩大 Backend 与账本契约，超出本票。
