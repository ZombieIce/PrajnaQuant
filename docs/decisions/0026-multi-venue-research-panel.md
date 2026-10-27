# ADR 0026：沪深同池研究数据面板与共享 Session 轴

- 状态：Accepted implementation contract，2026-10-27（Issue [#293](https://github.com/ZombieIce/PrajnaQuant/issues/293)；拆分与 B1/B2 裁决见 [#286](https://github.com/ZombieIce/PrajnaQuant/issues/286#issuecomment-6412305939)）。
- 关系：[ADR 0015](0015-mvp-0-data-contracts.md) 固定 Dataset 表契约；本文只固定研究面板如何从已发布 Dataset 选择 Venue 和形成共享 Session 轴。混合 Venue 的执行语义不在本文范围。

## 背景

MVP-7 的跨资产 ETF 池同时含 `XSHG` 与 `XSHE` Instrument，而现有 `prajna-research` 面板入口 `load_panel` 按单个 Venue 过滤 instruments、sessions 与 bars。负责人已裁决：新数据使用真实 `XSHG`/`XSHE` 身份，不把 `SZSE` 回改，也不以伪造 Venue ID 绕过单 Venue 面板；同池证券保留各自交易所，共用一条交易日轴，评价窗口内沪深 Session 日期集合和开收盘时刻必须完全一致，不一致是结构性错误，不自动补齐，也不把缺一侧日历当作停牌。

#330 的 `cn-stock-etf-daily` v1 已把同一固定日历版本的各 Venue Session 写入 Dataset；`instruments` 保留真实 Venue。面板层需要在不改写 Dataset、不生成第二套量化核心的前提下，为沪深同池研究提供确定性的加载与校验边界。

## 决定

1. **Venue 身份来自 Dataset。** 面板只接受 Dataset `instruments` 与 `sessions` 中出现的真实 Venue；不把旧 `SH`/`SZ` 前缀、`SZSE` 或合成 Venue 映射成 `XSHG`/`XSHE`，也不为缺失 Venue 创建 ID。Instrument 的 Venue 必须与其 `venue_id` 行一致。

2. **显式 Venue 集合入口。** `load_panel_for_venues(lake_root, dsv, venues)` 接受非空、无重复的显式 Venue 集合。每个被选 Venue 必须在同一 Dataset 中至少有一个 Instrument 和一条 Session，否则返回结构性 `InvalidVenueSelection`，不把空集合当作合法空池。

3. **共享 Session 轴。** 选定 Venue 的 Session 日期集合必须完全相同；对每个共同日期，`ts_open` 与 `ts_close` 必须完全相同。比较使用 UTC 纳秒时刻，不按本地日期或字符串重新推断。任一差异返回 `InconsistentSessionAxis` 并指出 Venue 与日期。面板不补齐缺失日期、不删除只在一侧存在的日期、不压缩日历，也不从价格日期推断 Session。

4. **缺失行情保持缺失。** 某 Instrument 在共享 Session 无 Bar 时，面板网格保留该行并写 `has_bar=false`、价格 null、`close_available_at` 保持源值或 null。不得补零价格、放宽可交易性、生成停牌或执行状态，也不得因缺 Bar 删除 Session 或 Instrument。执行状态仍来自 Dataset 的独立 `execution_status` 表；缺少一侧日历是结构性错误，不是 HALTED/UNKNOWN 状态。

5. **兼容与身份。** 既有 `load_panel` 保持单 Venue 的 Instrument 集合、Session 轴、网格与错误语义不变；单 Venue 的旧 SYNTH、MVP-6 `SZSE` Dataset、Experiment/Run 身份、Factor Cache 身份和已保存结果不受本 ADR 影响。新入口不改变 Dataset 内容、不写文件、不选择“最新”版本；它按调用者给出的精确 DSV 读取。

6. **职责边界。** 本文只交付数据面板契约。Fast Event/Vector 对混合 Venue 面板的接受、执行语义、手算账本 golden 与新引擎语义版本由后续多市场引擎语义 ADR（Issue #294，待交付）规定；Accurate 维持单 Venue。页面、池版本、共同评价窗口和预热属于后续票据；本 ADR 不改变它们，也不把面板 Session 轴自动截成某个窗口。

## 后果与限制

`load_panel_for_venues` 的验证使用完全合成的、经正式 `publish_dataset` 与 `cn-stock-etf-daily@1` 发布的多 Venue Dataset；覆盖正常沪深同池、缺单侧日期、开/收盘时刻差异、缺 Instrument 和 Venue 选择负例。没有读取或公开真实行情、原文、覆盖或身份，也没有证明真实沪深日历在所有历史区间相同。

面板的 Session 轴就是该 Dataset Version 的轴；若调用者需要更窄的评价窗口，必须由后续票以显式窗口或相应 Dataset/池输入定义完成，不能在本层悄悄裁剪或补齐。当前 #289 生产 CLI 按单证券发布 Dataset；把多个单证券 Dataset 装配为池输入不在本文范围，需由 T05/T06c 明确。面板加载成功只证明数据身份、Venue 与共享轴校验通过，不代表混合 Venue 回测、总回报、公司行动或业绩已可用。
