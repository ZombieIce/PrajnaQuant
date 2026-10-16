# 深市 ETF 派生执行证据、Eligible Set 与动量输入

实现位置为 `prajna-research::{etf_market, etf_eligible, etf_momentum}`；对应 #176、#171 P9～P14 与 ADR 0022。它是离线证据输入构建契约，不是实时开盘状态源。旧 D7、旧 Factor、现有 Engine/Experiment API 不变；#177 负责逐 Session Engine/Run Spec 消费，#178 负责 Python。真实来源覆盖、授权与完整公告提取仍受 #172 门禁约束。

## 行情输入 v3（#201）

`NormalizerRegistry::with_builtins()` 新增 `szse-etf-frozen@3`（默认范围 158000–159999）。v1/v2 的配置 hash、schema 和结果不变。v3 使用 `SzseEtfFrozen::v3(Vec<(u32,u32)>)` 配置六位代码的闭区间；排序规范化后进入配置 hash，空/重叠/非法范围拒绝。自定义配置须在空 `NormalizerRegistry::new()` 中登记，并用同一配置发布/重建；内置配置不匹配时重建明确报 `ConfigHashMismatch`。调用者应将范围配置与运行输入一起冻结，不用默认 registry 重建自定义 DSV。

v3 Bars 仍为原始价 D7 schema，另有 `bar_sources@1` Parquet：主键 `(instrument_id, session_date)`，每根 Bar 恰有一行，字段 `source`（`tdx` / `tencent_fallback`）、`source_raw_sha256`、可空 Decimal128(38,18) `reference_price`。后者解码 TDX `.md1` offset 4 的昨收，按 0.001 价格刻度校验，是交易所除权参考价；分红/折算跳变不得用上一 close 替代。来源表进入 Manifest、DSV、发布校验和 Raw 重放；无成交记录不产生 Bar。裸代码仍需 Catalog 映射为生命周期身份。

逐格选择：有 TDX 记录优先（含无成交）；仅无该代码/日期记录时，腾讯正成交未复权 `day` 可补位。腾讯不能覆盖 TDX，也不再作为 v3 发布门槛。`source_crosscheck` 只保存诊断，不将腾讯补位伪称 TDX 匹配。TDX 主源 Bar 成交额取 offset 72；补位 `amount=null`、`reference_price=null`，不估算成交额或参考价。最近 20 Session 成交额任一缺失使窗口不可用；不得改成最近 20 根非空 Bar 或以 19 Session 均值通过筛选。`available_at` 仍 Unknown，不以本地 `observed_at` 倒填历史可用时刻。正成交 TDX 行缺少正数、有效 tick 的参考价时以 `invalid_reference_price` 拒绝规范化（源字段损坏不伪称可用）；腾讯补位的参考价缺失则保留 null，交下游 UNKNOWN 门禁。TDX 源内价格/成交额/参考价修订冲突报错；修订原文须单独生成新 DSV。

`szse-derived-open@1` 的双源逻辑保留不变；#203 已接入 v4 代码表与 Catalog v2，#205 的显式 v2 状态消费见下节。#204 接新浪事件与三方一致；不能将新 DSV 发布成功称为真实执行状态或真实总回报验收通过。v3 暂保留 v2 的结构化 `corporateActions` 校验，不增加 PDF 解析器。

## 执行状态 v2（#205，显式 opt-in）

`MarketEvidence.semantic_version = "szse-derived-open@2"` 选择新契约；字段省略或 null 仍使用原 `szse-derived-open@1`，且序列化时不增加字段，旧身份不回改。不识别的非空版本拒绝反序列化。v2 仅接受 `szse-etf-frozen@3/@4`；从冻结 Raw 重建并校验原有四表及 `bar_sources` logical hash，来源逐格与 Bars 一一对应。v4/Catalog v2 的代码复用、名称变更身份边界限制格子日期，不生成跨生命周期的状态。市场日历必须与 Catalog 相同；不再以缺腾讯或当日全市场没有正成交推断覆盖失败，生命周期内缺 Bar 单独保留 UNKNOWN。

- 正成交 TDX 或腾讯补位 Bar、公告覆盖证据、已知 PIT 限价规则及来源参考价齐备，且无已知开盘停牌，才为 TRADABLE。取消双源匹配与来源独立性门槛；`source_crosscheck` 仅诊断。`source_independence` / 手填 `reference` 不能替代 v2 所需证据。
- 参考价**只读 TDX `bar_sources.reference_price`**，不是前一 close；旧 `OpeningEvidence.reference` 不参与 v2 限价计算。腾讯补位无参考价，保持 UNKNOWN，不能用手填参考价修复。成交额仍为 null，不填零、估算或跳过 Session。
- `LimitRule` 保留版本、半开生效区间 `[effective_from, effective_until)`、比例 bps、tick、首日适用及舍入，v2 另要求 `official_rule: {"document_number": "...", "fund_category": "..."}` 和对应官方条文 `Evidence`。规则证据必须在开盘前可知；缺文号/类别、未生效/已废止/迟到或未知规则均 UNKNOWN。规则按生命周期/日期显式提供，不从代码猜类别，不默认套 10%/20%；本票不提供未经核验的真实规则表。
- 用来源参考价精确计算并按 tick 四舍五入。开盘涨停不买、开盘跌停不卖；不改成双向 HALTED。另对已有 Bar 的 OHLC 校验 tick 与上下限，越界直接报 `prices_outside_pit_rule`，即使另有开盘停牌证据也不能吞掉价格矛盾。
- 官方证据在开盘前已知且明确覆盖开盘时，HALTED，金额按 0。`OpeningOnly` 可与下午成交协调；`FullSession` 与正成交矛盾为 UNKNOWN。未在开盘前可知的停牌证据不用于宣称已知停牌，保持 UNKNOWN；无 Bar/无公告不能当停牌。
- 规则/覆盖/行情可用时刻缺失或冲突时不成交。日线状态的 `available_at` 最早为当天 close，并取相关依赖最晚时刻；HALTED 不需要行情可用时刻，以停牌证据与当日 close 为界。`known_by` 仍保护收盘输入，`permits` 仍只是离线执行复核，**不是开盘前可知状态**。

Eligible Set 沿用 P9 的 61/60/20 Session 和 1,000 万阈值，以及完整固定 Session 窗口；任一 UNKNOWN 或缺成交额使该窗口不可用。v2 市场身份进入 TRI、Factor Cache、Eligible Set 与 PIT Run 身份；不改旧动量或引擎账户/成本算法。

### cninfo Risk Exit 连接

`RiskSignals::build_from_cninfo(catalog, notices, index, raw)` 消费 #202 的 `IndexDataset`；Experiment 的 `pit.cninfo_index` 可选字段连接到同一构建器，CLI/Python 宿主共用 Rust 结果。省略字段仍走原 `risk_notices` 路径。要求支持的分类版本、覆盖 Catalog 的公告窗口、无索引/基金 unknown、有效 Raw 引用，以及一致的代码/公告 ID/分类。未知分类版本、覆盖缺口、无法唯一归属生命周期均明确报错，不就近连接代码复用期间。

分类中的持有人大会产生 `holders_meeting`，大会停牌或计票停牌产生 `vote_suspension`，合同终止产生 `termination_proposal`，清算提示产生 `liquidation_notice`；正式终止仍可由 Catalog 生命周期补入。普通报告/普通停复牌不单独触发风险退出，`low_nav_warning` 仍只标注。这里的标题类别仅用于风险提示，**不据此推断实际开盘停牌区间或清算金额**。

#202 的毫秒公告时间尚无可用时刻/精度证明，转换为上海当地日期 D，再按 P6 取 D 后下一 Session close；不直接把 `announced_ms` 或 `observed_at` 当可用时刻。分类与日期规则写入风险证据 locator，公告 Raw/ID/可用时刻随 RiskSignal 进入 Run 身份。已有持久风险退出逻辑不变：在严格晚于可用时刻的下一明确可卖 open 退出，HALTED/UNKNOWN/跌停持续等待；普通调仓不得恢复风险持仓。

合成事件序列：S3 日期的终止议案在 S4 close 可用；S4 open 可以执行 S3 close 已形成的买入，不能偷看 S4 close 风险。S5 HALTED、S6 UNKNOWN、S7 跌停，均不卖；S8 open 明确可卖后才退出。该输入在 Experiment 的 Vector/Fast Event 两条路径均验证；不改变费用、现金、持仓、NAV 或原有 next-open 时序。真实规则、公告提取、来源正确性与全集性没有由合成测试证明。

补位采集命令（只在本机真实数据任务运行，本票测试不联网）：

```sh
python3 scripts/collect_tencent_fallback.py /local/missing-cells.json --network-path '实际国内直连路径'
```

本地 plan 为 `[{"symbol":"158001","session":"2026-01-05","tdx_observation":"missing"}]`，由 TDX 缺格规划器提供；无成交不能标为 missing。请求仅一个代码/Session、空复权参数；`--code-range FIRST-LAST` 可重复，与 Normalizer 范围保持一致。响应原文（含失败）保存 `~/PrajnaData/mvp-6/raw/tencent-responses/`，逐请求日志保存 `acceptance/tencent-requests.jsonl`，均不可入库公开。成功文件再交 `freeze_raw`（工具本身不冒充 RawStore）。工具禁用环境代理与重定向，单实例串行 ≥1 秒、指数退避最多 3 次重试，连续两次 403/501/验证码后本实例停源为 unresolved；每次请求记录 URL/参数/观察时间/HTTP 状态/字节数/hash/实际网络路径。只运行一个该域采集实例；不得通过重启绕过访问拦截。国内网络路径须由运行环境保证，字符串说明本身不证明出口归属。

## 构建与冻结（v1 原契约）

1. `HistoricalCatalog::build/load` 验证 #175 Catalog、生命周期身份和 Raw 原文。
2. `DerivedMarket::from_dataset(lake_root, dsv, catalog, MarketEvidence)` 读取冻结 Manifest，从 Raw 重跑 #174 `szse-etf-frozen` Normalizer，核对 instruments/sessions/bars/source_crosscheck logical hash。规范化价格按 0.001，腾讯手数乘 100 转份，与 TDX OHLC/份数精确一致；成交额仅取 TDX。代码按当日生命周期映射，不跨代码复用连接。Dataset 与 Catalog 的 Session 时间必须一致。
3. `MarketEvidence` 是从冻结官方附件明确提取、裁决的输入，需保存 JSON 原件供重放。每个开盘格子的 `OpeningEvidence` 引用公告覆盖、来源独立性、开盘停牌时段、PIT 规则和除权息调整后前收盘参考价。全部文档引用沿用 Catalog 的 `Evidence`（Raw hash、公告 ID、定位、发布时间精度）；读取时校验 Raw 完整性。没有证据的格子保持 UNKNOWN，不使用标题关键词断言实际停牌时段。
4. `DerivedMarket::version()` 绑定 DSV、Catalog、排序后的完整证据、推导语义版本 `szse-derived-open@1` 和全部派生格子。调用者将输入证据与版本一起冻结；导出格子用 `market.cells().values()`，不要直接 JSON 序列化 tuple-key Map。真实逐日价格只存本地，不提交到 public 仓库。

`MarketEvidence.start/end` 必须处于 Catalog 覆盖内；非空 `coverage_gaps`、全市场任一 Session 缺 TDX 行、日历不符、无归属行情或重复裁决都会报错。单标的缺行/来源冲突不会被补成停牌。空 Eligible Set 可以成功，但缺市场 Session 不可以成功。

## 开盘状态与方向（v1 原契约）

- 两源精确匹配、有正成交、公告覆盖与来源独立性有证据、PIT 限价规则明确时，才能推导 TRADABLE。
- 官方明确开盘停牌为 HALTED。`OpeningOnly` 可以与下午成交协调；`FullSession` 与正成交冲突为 UNKNOWN。HALTED 成交额按 0 参与流动性窗口。
- 规则按生命周期/日期显式选择，含版本、官方证据、生效/废止区间、首日适用、tick、涨跌幅和舍入。当前仅支持 `nearest_tick_half_up`；参考价先除权息调整，再精确计算 `reference × (10000 ± rate_bps) / 10000`、按 tick 四舍五入。无法证明规则或参考价时 UNKNOWN；没有默认 10%/20% 或首日豁免。
- 开盘涨停仅 `can_buy=false`，跌停仅 `can_sell=false`。不是双向 HALTED。原因、上下限、证据 hash、规则版本与实际可用时刻随格子返回。

`market_available_at` 是两源历史输入真正可用的时刻，不能取本地 `observed_at` 代替。日线对拍最早在当日 close 可观察，推导可用时刻取所有依赖最晚时刻；None 仍未知。公告 DateOnly(D) 最早为 D 后下一 Session close。`known_by` 控制收盘筛选，`permits`/`rebalance_gate` 是**离线执行复核**，不得作为开盘前 Strategy 已知信息或真实排队成交证明。

手算事件序列：S1 close 形成 B 目标；S2 open 已持有 A，但 A 为 UNKNOWN，则 `rebalance_gate(S2, required_sells=[A], requested_buys=[B])` 返回 Deferred，零买卖腿。S3 open A 明确可卖后重试。若仅 B 为 UNKNOWN、没有被阻塞卖腿，Permitted 的 `allowed_buys` 排除 B。所需卖腿（含减仓）、pending 保存/信号替换和实际账户执行由 #177 负责，不在此门禁复制账户核心。

## TRI 接口与 Factor Cache

`TotalReturnIndex::build(market, provider, empty_actions, raw_store)` 仅接受两种互斥输入：

- `TotalReturnProvider` 提供已计算的正数指数水平、每点可用时刻与完整输入身份。包括 HALTED 日也必须明确提供，以免携带发生公司行动的旧参考值。实际点值（IEEE 754 bits）及可用时刻也进入身份，避免 provider 身份误复用。
- `EmptyActionCoverage` 显式覆盖全部生命周期与日期，附版本、Raw 证据 hash 和可用时刻。Raw 缺失/损坏会拒绝。6a 中已声明公司行动为空时指数等价原始价格；HALTED 延续上一水平、UNKNOWN 打断延续。这个显式工程输入不证明真实历史公司行动为空。#180 增加基于 v2 DSV `corporate_actions` 表的顺序 TRI；#181 将规范化事件与 TRI 完整性报告接入 action-aware PIT Fast Event/Vector seam，输出 Return Basis、税率、逐标的覆盖/不完整原因和事件账本。合成 3 ETF × 10 Session 覆盖分红、应收不投资、折算取整、清算与 Vector/Fast 逐日对拍；`observed_raw_set_only` 仍不证明真实公告全集。

`szse-etf-frozen@2` 仅当 cninfo 结构化索引行包含显式 `corporateActions` 事实时才产出事件表；分红缺日期/金额、折算缺日期/比例、清算缺生效日/结算金额、未知字段或未结构化的分红/折算/终止公告均 fail closed。保留来源 `announcementTime` 的精确 `announced_at`，另按冻结日历的下一交易日收盘计算保守 `available_at`。当前没有 PDF/附件解析器或公告全集/分页覆盖证明，v2 TRI 不应被解释为真实公告已完整。`TotalReturnIndex::build_from_dataset` 校验 action Parquet 的逻辑 hash/行数与 DSV，验证 Raw hash，再按事件日期顺序累计；分红使用 `(P[t] + d) / P[t-1]`，折算使用 `r × P[t] / P[t-1]`。公告可用时刻传播到事件点，因此晚到公告不能成为更早决策的可用因子值。同一 Instrument/事件日有多个事件时，因为当前输入未表达事件之间的权益份额基准与顺序，构建器 fail closed。规则版本和跳变阈值、事件行、数据集版本及生成点值进入 TRI 身份；Factor Cache 与 PIT Run 身份引用该版本。阈值以上且未被事件金额/比例解释的价格跳变在对应点缺失，并进入逐 Instrument 的 integrity 报告；报告的 `observed_raw_set_only` 不是完整性证明。

`EtfMomentum::new(N)` 的独立身份为 `etf_tri_momentum@1`，高值优先，值为 `TRI[t]/TRI[t-N]-1`，需要 N+1 个交易所 Session。整个窗口内任一状态/指数输入 UNKNOWN 或缺失则 `missing_input`，不跨缺口找更早价格；HALTED 平持。依赖可用时刻未知为 `unknown_availability`；成功值可用时刻取全部依赖最晚值，且不早于观察 Session close。输出复用既有 Factor Values schema，非 ok 行 value/available_at 均 null。

`EtfMomentum::cached` 复用 `FactorCache` 的校验、Parquet 和原子发布。独立 `FactorKey::for_etf_input` 绑定新因子版本/参数、DSV、Catalog、状态完整输入、TRI 输入、Universe 版本、实际成员、Values schema 和实现版本；旧 FactorKey 不变。缓存命中前也校验 TRI 与市场身份/成员，不能用另一市场的指数命中旧条目。

## Eligible Set

`EligibleSet::build(catalog, market, tri, session_date, params)` 自行生成该日 PIT Universe、计算新动量，避免传入无身份的外部评分。默认上市满 61 Session、60 Session 动量可用、最近 20 Session 平均 TDX 成交额 ≥ 10,000,000 元、当天可买且全部输入在决策 close 已可用。

上市时长与窗口按 Catalog 交易所 Session，不按已观测 bar 数。金额使用 fixed-point 整数比较总和与阈值 × 窗口，不作浮点近似；HALTED 为 0，UNKNOWN/缺成交额不补 0。小窗口 fixture 可显式改参数。历史窗口中晚到证据同样不得进入当次决策。

输出保留原 Universe/Run Spec 片段、Eligible 版本、市场版本、Factor Cache key、全部参数、成员和逐标的排除原因；应由 #177 原样进入 Run Spec 与全部 ResultLevel。Eligible 只是收盘候选池，不能保证下一开盘可买，也不替代持仓卖出门禁。

## #177 Engine / Run 消费

`prajna-experiment` 的显式 `s2_pit_rotation` 接入上述构建产物，顶层 Universe
为 Catalog content identity，不改变旧 Static Universe 路径。参数 lookback/Top-K/
cadence 与上市、金额阈值和所有输入/规则版本均进入身份；每 Session Universe、
Eligible（含缺失/排除诊断）在所有 ResultLevel 的 PIT Run Spec 保留。
HALTED/UNKNOWN/涨跌停受限格子另存 `execution_exclusions`，不因其已离开 Universe
而丢失已持仓的未知执行格子。具体定义和存储入口见
[`prajna-experiment`](../crates/prajna-experiment/README.md)。

新路径先按 full_target 的费用、实际数量与 lot/cash 约束完成 sizing，再由共享
`ExecutionState::resolve_sized` 提交状态：实际必需 sell 被阻塞时整次常规目标延期，
无常规成交或扣费；实际 buy 被阻塞时冻结该标的已有数量（未持有则保留现金），
移除该腿费用后重算其他腿，不重归一目标。不能以目标成员身份或名义权重变化代替
实际买卖方向；Vector PIT/Fast 共用同一 F8 金额/数量 planner，按实际现金、数量、
原始估值价算权益、费用基数与分项费用，再用扣费后权益乘目标、除原始开盘价，
以相同操作次序按 Domain scale-18/half-even 舍入。不能先舍入归一化费用或市值，
不能从浮点权重/NAV 反推 sizing 输入，也不设 no-trade epsilon。精确零数量增量但有费用的
F8 fee-only settlement 按其 fee_side 门禁；所有可表示的非零增量仍判实际方向。
Fast Retry 只补足缺失的绝对目标增量，保留其他持仓；Skip 不重试。
PIT Vector 的显式 `vector_pit.initial_capital` 为所对拍 VP 的资本，进入身份；
其内部精确现金/数量只用于 sizing 与方向判定，不是第二套结算账本。
语义版本 `pit-s2@4` 进入身份，避免重用旧方向判定结果，
旧 Static Universe 路径不变。每个新 close 的常规
Decision 替换旧 pending，但不取消独立持久 Risk Exit。风险退出优先于常规 open 尝试，
逐个风险标的等待明确可卖 open，不等待 cadence、不受其他持仓延期影响，卖后保留其他
持仓数量。下一 open 必须严格晚于风险证据可用时刻；DateOnly 沿用 Catalog 的保守规则。
低净值提示只标注；未退出风险腿在期末仍报告 pending。

持仓方向回归见 `tests/pit_sides.rs`：10,000 元、A 8,000 份、现金 2,000，
目标 A/B 各 50%、零费用且 A 涨停时，允许卖 A 3,000、买 B 5,000，
最终 A/B 各 5,000、现金 0；A 从 50% 增至 100% 的跌停买入不被卖方向阻塞。
另验 1% 费用、费用导致名义增持变成实际减持、整次延期和持仓增量 Retry；
lot 与 common parity 均有独立手算数量/现金断言，不只互相对拍。

Fee-only 边界：平价 1、资本 10,000、佣金 1%，先持有 A 权重 0.2997，
后调至 A=0.3/B=0.0997。common parity 的开盘前权益 9,970.03、
A=2,988.017991；费用 9.97003 后 A 数量精确不变，A 的 buy-side fee-only
扣费 0.02991009、B 买入 993.017979009（费用 9.94011991），
现金 5,979.023999991、权益 9,960.05997。A 跌停允许该买方向结算，
不能因浮点差 −5.55e−17 判成受阻卖出。另验涨停跳过该结算后重算 B、
正/负浮点零边界与真实小于 1e−12 的卖增量仍整次延期。

相邻有效输入只把 B 改为 `0.09970000000000002`，B 费用即为
9.940119910000001994、总费用 9.970030000000001994、A 目标数量
2,988.017990999999999402：实际减少 598 个数量 quantum，跌停时整次延期。
同一目标在资本 1 时数量增量为零，而资本 10,000、价格 2 / 7.5 时分别减少
299 / 80 quantum。资本、价格、佣金/税/滑点分项矩阵另验逐日现金/数量守恒和
Vector/Fast NAV/权重对拍；只验证合成工程口径。

手算 3×10 用例（全部平价 1，只有 S7 的 A open=0.9）：S3 close 选 A；S4 open
以 10,000 元、lot100、3bp/min5、2bp 滑点买 A 9,900 份，现金 93.02、NAV 9,993.02。
S4 close 终止议案可知；S5 HALTED、S6 UNKNOWN、S7 跌停均不卖。Universe 在 S5
移除 A，常规 pending 由 S5 的 B 换成 S7 的 C，但风险仍等待。S8 明确可卖后先以
0.9998 卖 A，扣最低佣金 5，现金 9,986.04；再买 C 9,900 份，现金 79.06、NAV
9,979.06。S7 open 的 NAV=93.02+9,900×0.9=9,003.02，close 回到 9,993.02，
不把跌停或 UNKNOWN 估值成零。累计三笔佣金 15、滑点 5.94。
对拍忽略 lot/min5：S4 A=9,995；S8 卖后现金=9,990.0025，买 C 后 NAV=
9,985.00749875。Vector 每行含 next-open return，按
`nav/(1+gross_return)` 与 Fast 同 Session 的 open-after 权益比较。

## 验证与边界

完全合成 fixture 覆盖 3 ETF × 10 Session、61/60/20 边界、精确阈值、开盘停牌但下午成交、全天停牌与成交冲突、涨跌停方向、来源冲突/缺行、全市场缺 Session、晚到证据、UNKNOWN 整次延期门禁、缓存隔离、原文篡改与 DSV 重放。#177 另验证逐日 lot 账本守恒、Risk Exit/cadence 分离、延期替换、fewer-than-Top-K/现金、Vector/Fast 共同语义日对拍、Experiment 身份及 Summary→Full 提升。前者仍是 #176 输入构建的验证范围；后者只是新增合成工程验证，不等于真实市场 P0、真实总回报或父票据 #171 的最终验收。
