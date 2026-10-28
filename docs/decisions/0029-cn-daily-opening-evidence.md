# ADR 0029：沪深日线固定开盘状态证据

- 状态：Accepted implementation contract，2026-10-28；Issue [#350](https://github.com/ZombieIce/PrajnaQuant/issues/350) D1–D6。

## 决定

生产日线新增显式 opt-in `cn-stock-etf-daily@2` 与 recipe schema 2，复用冻结 v1 行情转换。状态使用带 schema/method/source/extraction 版本的本机 JSON 快照，绑定行情 Raw、日历、证券、Instrument/Venue、发布日期及适用开盘区间；引用原文字节和定位，分别保存历史 available_at 与本地 observed_at。全部内容身份进入 manifest/rebuild；不能对旧 DSV 后补表。

不建采集器，不从 bar、正成交量、日历或“未见停牌公告”推导可交易。状态事实须有必要原文引用；声明历史可用时刻不得早于任何必要证据，任一必要时刻缺失则 Unknown。晚到保留真实时刻，CONFLICT 映射 Unknown；相同重复去重，矛盾重复或身份/日期/来源引用不符拒绝发布。缺失事实规范化 Unknown/false/null；有效空快照允许全 Unknown，未选择快照拒绝。

D7 及执行器不变：整表缺失报错，单行缺失拒绝，开盘须同时有 bar、可交易标记与不晚于开盘的历史状态时刻。行情 available_at 仍 Unknown，不使用状态时间替代行情时间。证券条款、日历、状态和行情覆盖分别记录。

## 验证及限制

完全合成沪深单 ETF 使用正式 publisher CLI、reader、Experiment CLI 与持久结果读取；下一开盘执行/现金/数量/费用/NAV 对照独立手算，Vector 仅对照已有共同语义。v1/旧 recipe 和 MVP-6 SZSE 身份保留；修订新建 DSV、移除原输入仍可重建。真实状态来源内容正确性、历史完整覆盖及发布时间未验证；文件导入/hash 验证不认证市场事实。#340 在本前置完成后复验 HTTP/T01 产品链。

输入字段、失败与版本兼容详见[日线契约](../contracts/shanghai-shenzhen-daily-normalizer.md#opt-in-v2-opening-state-evidence)。不采用 bar-derived 或默认历史开盘前时刻：两者会把缺证变成未经批准的研究假设。
