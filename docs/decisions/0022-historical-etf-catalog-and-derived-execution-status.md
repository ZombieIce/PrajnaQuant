# ADR 0022：历史 ETF 名录、PIT Universe 与推导开盘执行状态

- 状态：Accepted as target architecture，2026-10-14（spec [#171](https://github.com/ZombieIce/PrajnaQuant/issues/171) P7～P15，设计票据 [#173](https://github.com/ZombieIce/PrajnaQuant/issues/173)）；部分实现但**未通过真实数据门禁**。仅对新 MVP-6 路径修订 [ADR 0017](0017-vector-engine-semantics.md)；旧 Factor、Static Universe 和既有结果不变。

## 2026-10-16 数据源契约 v3 修订

依据 [#171 负责人裁决](https://github.com/ZombieIce/PrajnaQuant/issues/171#issuecomment-6209686017)（以 #171 最新“数据源契约 v3”评论为准），以下条款取代下文同主题的原设计；v1/v2 数据与结果不回改。产品验证声明来源下的 PIT、时序、账本与 fail-closed 处理正确，不证明供应商自身正确或全集性，不称“P0 已证明解决”。

- **P5/P7/P11**：TDX g4day 是行情主源，范围为配置的深市 ETF 代码段（含 158xxx）。腾讯未复权 `day` 只在 TDX 缺失 Instrument/Session 时补位，每根 Bar 保存来源；TDX 无成交观察阻止补位。取消双源完全一致门槛；跨源差异保留诊断，不能覆盖主源。源内相同主键的不同修订仍报错。补位成交额缺失，不估算、不补零、不缩短或跳过最近 20 Session 窗口；缺失窗口输入不能通过流动性条件。
- **P12**：参考价是 TDX 昨收（交易所除权参考价），不是前一根 Bar 的 close。补位无参考价；未知参考价或 PIT 规则时保留 UNKNOWN，不能借腾讯 close 猜测。该字段供方向门禁及公司行动发现读取，不改变信号 T close、下一 Session open 的执行序列。
- **P7/P8**：目标名录由 TDX 逐日代码表重建，缺席间隔与 cninfo 法定全称变化切分生命周期。目标宽基筛选使用 cninfo 法定全称与版本化国内宽基指数名/别名匹配，按公告可知时刻生效，不用场内简称；缺失/无法匹配期间排除并报告。具体实现由 #203 承担。
- **P10**：TRADABLE 允许 TDX 或补位正成交 Bar，加已知开盘停牌排除和 P12 门禁；公告覆盖开盘为 HALTED，无 Bar 且无停牌公告为 UNKNOWN。#205 实现显式 `MarketEvidence.semantic_version = "szse-derived-open@2"`，省略仍保留旧版身份/双源逻辑；v2 只消费 v3/v4 的来源参考价，并记录官方规则文号/类别/生效区间，越界行情报错。补位参考价缺失仍 UNKNOWN，Eligible 固定窗口不跳过缺金额 Session；cninfo 分类风险信号按 P6 日历连接。仅合成验证，契约见 [派生输入](../derived-etf-input-contract.md)。

2026-10-21，[#252 / ADR 0024](0024-real-data-opening-evidence.md) 为真实开盘输入增加 opt-in `szse-derived-open@3`，只部分取代下文 P10 的状态推导方式；@1/@2 身份与行为不变。@3 从含公司行动的 `szse-etf-frozen@7` 接受经绑定的 @4 行情投影，按正成交日线 Bar 的 `open` 作为首笔成交价，并将行情状态的可用时刻设在 Session 收盘。它不要求公告覆盖证明；其余量价、限价规则、停牌冲突与 UNKNOWN 边界见 [ADR 0024](0024-real-data-opening-evidence.md)。真实行情复核仅限本机，不能据此声称官方队列成交或真实收益已验收。

## #217 SZSE 基金限价规则目录补充

`szse-fund-limit-rules@1` 将已核验的 SZSE 法规规则按 `[2022-10-01, 2026-10-01)` 版本化，包含 2020/2021 规则组合、2023-04-10 规则切换及 2026-07-06 规则切换。公开配置保留各行的法规文号、官方来源链接/章节、生效区间、10%/20%类别、0.001 tick、舍入和最小一 tick 价差处理；原文与 Raw hash 仅留本机。固定名单五只 ETF 在覆盖窗口逐只列为类别 `UNKNOWN`，该窗口说明不代替上市/终止日期，不从代码或简称推断成员。一级归属须有深交所直接证据；二级归属须把基金合同/招募说明书中的跟踪指数与深交所对应指数类别条文共同绑定；详见[规则目录与来源](../szse-fund-limit-rules.md)。

## 2026-10-20 v2 类别赋值生命周期绑定修订（#250）

公开 `category_assignments` 继续以六位 SZSE 代码为键，不写入生命周期 ID。解析时按 `Lifecycle::validity` 把赋值绑定到 Catalog 生命周期：Session 必须落在生命周期有效区间；法定全称变更的后继以 `identity_available_from` 标记同一基金，在各自区间内绑定；被终止后的复用代码属于不同基金，一条赋值区间同时覆盖两个基金时相交格保持 `CategoryUnknown`，不报错也不任选其一。`official_rule.category_evidence` 新增可选 `lifecycle` 字段记录实际绑定，`supports` 同时校验公开赋值身份和该绑定。v1 线格式不受影响（省略该字段）；v2 解析始终写入，缺失即 fail closed 为 `UNKNOWN`。因此 v2 规则证据、派生市场与覆盖身份变化，公开 v2 配置和 v1 身份不变；旧 v2 证据需按新 schema 重新生成。该修订不改变 P10 的执行状态语义。

法规虽定义特例类别，但本次未取得五只基金合同/招募说明书 Raw，也未验证官方历史逐基金名单或 PIT 类别赋值。公开目录的类别赋值表因此保持空；五只代码在配置覆盖区间逐条标成 `UNKNOWN`。缺失、重叠、未知或迟到的类别证据仍解析为 UNKNOWN，不从代码/名称推断，也不将法规默认比例直接分配给个券。测试只用合成类别和行情数据，不表示真实基金成员、实际可成交或收益已验收。较早的[规则与许可审计](../research/mvp-6-rules-license-audit.md)记录的是其 2026-10-14 检索时点，保持历史结论不回写。

#201 实现 `szse-etf-frozen@3`、独立来源/参考价表及受 #172 §3 约束的补位采集工具，只经合成数据验证。#203 实现 `szse-etf-frozen@4` 的逐 Session TDX 代码表和 Catalog v2 生命周期/法定全称消费；v1-v3 Normalizer 与 Catalog v1 身份保持不变。v2 只按明确版本化的法定全称/别名匹配宽基，真实名单仍需官方证据。以上未用真实数据重建或通过真实数据门禁。#202/#204 负责新版公告与公司行动契约，三方一致性不由行情 Normalizer 单独证明。采集须国内直连、同域串行间隔至少 1 秒、指数退避最多 3 次重试、如实 UA、无伪造 Cookie/Referer，连续访问拦截停源；真实字节、hash、覆盖与日志只存本机。

## MVP-6 固定名单 Catalog 组装（#221）

`pit_catalog_assembly` 只读本机已发布的 `szse-etf-frozen@4`、公开 `configs/mvp-6/identity.csv` 与本机 `facts/citations.csv`，离线生成 Catalog schema v2 输入并构建 Catalog。该组装是固定名单 Catalog：仅身份表中的代码进入 Catalog，日 TDX 代码表在 v2 边界推导前收窄到这些代码；名单代码缺身份行时记录排除，不虚构生命周期。代码复用须有代码表缺席间隔；连续存在只能由显式 `change_kind=legal_name_change` 且有证据的法定全称变更切分。该标记同时是 #229 唯一允许的非终止重叠例外，v4 Normalizer 在前一身亡日期处截断；未标记的续接行仍按重叠生命周期报错。每行还需一条本机 `contract` 引用，其 Raw hash 作为 `contract_raw_sha256`；名称/类别/指数引用必须落在同一 Raw 页。公告日期来自冻结 Raw 的 `finalpage/YYYY-MM-DD/` 路径或匹配的 `announcementTime`，不使用本地 `observed_at` 或生效日；无法取得或存在歧义时失败。更名公告晚于新全称生效日时，`listing_date` 仍是生效 Session，`identity_available_from` 取 P6 首次可知 Session；`resolve_row`、Normalizer range、derived-market cell 与 UNKNOWN coverage 从该 Session 开始，前一身份不延伸，空档逐 Session 计一次 `UNKNOWN`；fixed list 成员资格仍按官方日期，空档不选成员。上市/终止与代码表差异按证据等级记录：官方证据优先并披露，二级不一致或官方存续期外有价 Bar 则排除该 ETF；官方存续期内、TDX 代码表退出之后的有价 Bar 记为 `priced_bar_outside_tdx_presence`（unresolved）并保留该 ETF，对应 Session 记 UNKNOWN；正式身份完全没有 TDX 代码表时保留其官方存续期，Catalog 披露 `lifecycle_without_code_table_identity`，fixed list 仍保留该代码并计 UNKNOWN；二级身份没有代码表佐证时按 P27-7 排除该二级生命周期；同一复用代码的 official 生命周期仍保留，只有全部行均为二级时才整代码排除（`secondary_code_table_absent`）。空 orgId 按 P27-19 排除该 ETF。每个排除代码写入 `mvp6_excluded:<code>:<reason>` 覆盖缺口，`fixed_universe` 将其转为逐代码排除并写入 `fixed_list_exclusions`，其他缺口继续整次失败。官方上市日早于 TDX 首次出现时保留为 `listing`，物理 `identity_start` 仍是 TDX 边界；TDX 首次出现早于官方上市日时保留该身份并把差异记为 `listing_date_vs_code_table_first`，只有官方存续期外存在有价 Bar 才排除该 ETF；代码复用的后继身份同样允许代码表先出现、后到官方上市日。固定名单成员资格按公告上市/终止日：TDX 代码表在官方终止前退出只形成 `UNKNOWN` 缺口，不提前移除成员；前一身份在公告终止日或下一身份的官方上市日（取较早者）结束，`identity_end_exclusive` 只是物理价格边界。v2 构建器把最早已知全称视为初始身份，不因首个属性晚于代码表首次出现而虚构第二个身份边界。UNKNOWN 覆盖分母按公告上市/终止日或下一身份边界计，不裁剪到 TDX 代码表首末次出现；代码表缺席但仍在法定存续期的 Session 计入 UNKNOWN。该组装不改变 #203 的 v1/v2 Catalog 身份或语义；仅合成测试验证，真实身份表与 v4 结果未验证。

## 背景与证据边界

当前 `prajna-research` 使用 Static Universe；Domain 的 InstrumentId 已区分 Venue，但裸交易代码不能区分代码复用的不同生命周期。日线和当前在市名录不能证明历史成员、类别或开盘可成交性。交易所没有可下载的基金逐日官方行情/停复牌全集，选择以官方公告和双源行情推导，而不是等待不存在的状态产品。

[#172 审计](../research/mvp-6-source-gate-audit.md)交付了未决证据：620 个代码至少一个扫描窗口未决，公告无完整分页，双源样本 0/30 完成，尚不能证明代码复用或未复用。[规则与许可审计](../research/mvp-6-rules-license-audit.md)也未取得全期间规则和用途授权链。以下是采用的设计，不是补齐这些证据；不缩短 2022-10-01～2026-09-30 数据窗口（决策自 2023-01-01 起），不自动替换来源，不启动未经授权的补抓。

## 决定

### 历史名录、身份和 PIT 属性（P7、P8）

Historical Instrument Catalog 从 cninfo 上市、终止/合同终止公告与 159000～159999 历史行情扫描交叉重建；当前交易所名录仅作交叉核对。须包含窗口前已上市及窗口中已终止产品，不以成功返回行情的代码集充当全集。保存双向差异表、逐项解释、遗漏/冲突范围、公告分页与附件覆盖、来源 Raw Object hash、规则版本和 Catalog 版本。空响应不是从未上市，历史行情不是基金身份或完整生命周期证明。

内部 InstrumentId 必须一对一指向一个有证据的上市生命周期；保留 native symbol 与 Venue，并显式映射生命周期和证据，不从 ID 字符串推断属性。不同期间复用相同代码必须分配不同身份，不跨期拼接价格、因子、公司行动或持仓；无法归属的行隔离并报告，不能就近连接。确切编码及 schema 迁移由实现票据定义并版本化，不改变旧 ID 的含义。

上市/终止、基金类别、标的指数分别保存生效区间和可用时刻，来源取上市交易公告书、合同/招募说明书及变更公告；同一事实的冲突须显式裁决，不能“最新值覆盖全部历史”。公告只有日期 D 时，按 P6 在 D 收盘后才可知，最早用于 D 之后下一交易日收盘决策；`observed_at` 只是本地采集时间。精确发布时间有证据时使用该时间，未知不推定为开盘前。

Point-in-time Universe 将固定筛选规则作用于有覆盖证明的交易所历史全集：当时已上市且未终止、当时已知为股票基金、标的指数属于版本化国内宽基指数清单。不得按名称关键词、今天的类别或事后挑选的 ETF 建池。冲突/无证据的属性期间排除并列出原因；若因此无法证明声明范围的成员完整性，运行失败，不能只排除未知项后宣称全集 PIT。停牌、缺 bar 或历史长度不足不删除历史成员。

### Eligible Set 与可用时刻（P9、P11）

每个决策 Session 从 Point-in-time Universe 独立生成 Eligible Set，记录每个被排除标的的原因：

- 上市满 61 个 Venue Session，60 Session 动量可用；窗口按交易所 Session，不按观测到的 bar 数。
- 最近 20 Session 平均成交额至少 1,000 万元；成交额只取 TDX，不从腾讯估算。已证实 HALTED 日成交额计 0，UNKNOWN 不补零且当天不合格。
- 当天 Derived Execution Status 允许执行，所需数据与 Factor Values 在决策收盘前已可用。历史长度、窗口与流动性阈值进入 Run Spec，小窗口金标准可显式改参数。

Eligible Set 只是可评分/可新买集合，不是卖出许可，也不保证下一开盘成交。Python As-of View 同时展示当时的 PIT Universe 和 Eligible Set；Strategy 输出仍为纯 Decision，无账户信息。两引擎消费相同集合、相同 Risk Exit 和执行门禁，不在 Python 再实现核心计算。

### Derived Execution Status、涨跌停与 fail-closed（P10～P13）

沿用 D7 的 `TRADABLE / HALTED / UNKNOWN`，增加按 Instrument/Session 的证据引用、适用时点、推导规则版本和理由。判定对象是开盘，不是全天“有过交易”：

| 状态/门禁 | 必需证据与行为 |
| --- | --- |
| HALTED | 官方公告明确覆盖开盘竞价或上午开盘时段；无行情不能单独证明停牌。若正成交等证据与公告时段无法协调，记冲突 UNKNOWN，不擅自忽略公告。 |
| TRADABLE | 两源规范化 OHLC 和成交量完全一致、有正成交，公告覆盖已核验且无开盘停牌证据，PIT 价格限制规则明确且通过对应方向门禁。仅日内复牌/当日有量不足以证明开盘可交易。 |
| UNKNOWN | 公告覆盖不足、两源缺失/不一致、单位不明、开盘时段不明、历史规则不明或证据冲突；不新买、不成交，不默认成 TRADABLE。 |

TDX 与腾讯须证明来源独立性及单位约定，不能因域名不同就认定上游独立。OHLC 按当时最小价格变动单位、成交量按整数份完全比较，不设百分比容差；修订生成新 Raw Object 和 Dataset Version。保存各证据可用时刻：日线双源对拍通常收盘后才能完成，是离线执行证据，不能提前暴露给开盘前或此前的 Strategy。最终推导值晚于决策收盘才可用时不能进入当次 Eligible Set；不得倒填可用时刻来满足日频策略。

按除权除息调整后的前收盘参考价及当时有效的涨跌幅、tick 和舍入规则计算开盘限制：涨停不买，跌停不卖，另一方向仍须通过其他门禁。三态不是唯一买卖许可，须另存方向性阻塞理由；不能把涨停误作全天双向停牌。规则版本须附官方文号、来源定位、生效/废止区间、类别映射、首日例外和参考价/舍入规则；原文与 Raw hash 只保存在本机，不进入公开规则表。不得套用个股 IPO 首日豁免或硬编码全窗口 10%/20%。规则未知即 UNKNOWN。

日线一致性和公告排除法只是 P10 的离线推导，不是官方开盘状态或真实排队成交证明，不能用于声称实盘可成交。若不足以确认开盘状态，保持 UNKNOWN；不以“有量”替代开盘证据。

单标的 UNKNOWN 不新买且不在当天 Eligible Set；已持仓需卖出而 UNKNOWN/HALTED/跌停时，沿用 [ADR 0005](0005-defer-rebalance-on-unavailable-exit.md) 整次延期、零买卖腿，保留 pending 和未知格子轨迹。估值可用有效原始价或显式陈旧估值，不是成交许可；不得填零价。全市场某 Session 缺失或声明窗口覆盖不完整则运行报错，不能以全 UNKNOWN 零成交报告成功。

### ADR 0017 的局部修订与 Risk Exit（P14、P15）

新增基于 [ADR 0023](0023-etf-corporate-actions-and-total-return-ledger.md) Total Return Index 与 Derived Execution Status 的动量 Factor 版本：高动量优先；N Session 收益取 `TRI[t] / TRI[t-N] - 1`，需 N+1 个 Session 端点。已证实 HALTED 的行情缺口沿用指数水平，不跨越 UNKNOWN 寻找更早价格；窗口内任一所需状态/输入 UNKNOWN 则 `missing_input`。停牌期间若有已生效且已公告公司行动，先按 ADR 0023 处理权益中性调整；无法建立一致参考价时 UNKNOWN，不凭停牌掩盖除权。Factor 可用时刻为所有依赖最晚可用时刻，并保存字段、窗口、方向、缺失规则、来源和版本。

这只替代新 Factor 中 ADR 0017“任一所需 bar 缺失均 missing_input”的 HALTED 分支；不修改旧 Factor、旧缓存键或 raw open-to-open 结果。新 Factor、Catalog、Universe、状态规则与数据身份均须进入各自版本/完整输入身份，避免命中旧缓存。6a 保持价格收益工程验收；不能在 TRI 未实现时宣称 P14 已完成。

持有人大会计票停牌提示、合同终止议案、清盘提示或正式终止信息达到可知时刻后，由风险门禁产生 Risk Exit Decision，不等待每 5 Session 的普通调仓；仅净值低于 5,000 万提示只标注。退出标的禁止再新买；风险覆盖不能被之后普通 Strategy Decision 恢复权重。下一明确可交易且允许卖出的 open 执行，阻塞时继续 pending，不强卖、不按最后价虚构成交；普通调仓仍受整次延期规则约束。最终无法退出的持仓进入 ADR 0023 的终止清算，而非从结果删除。

事件序列小例：S1 收盘风险公告只有日期 S1，故 S1 不触发；S2 收盘信息可用，风险门禁覆盖策略将 A 目标置零；S3 open 停牌，整次调仓延期且不买 B；S4 open A 明确可卖，才执行退出。即使 S3 收盘有新普通信号，也不得取消 A 的风险退出。公告时间只有日期时不得在 S2 open 提前卖出。

## Considered Options

- 当前名单加上市/退市过滤：保留幸存者与事后筛选偏差，拒绝作为 PIT Universe。
- 名称关键词分类、全历史套用当前类别/指数：无 PIT 属性证明，拒绝。
- 等待两市官方逐日状态全集：没有该基金数据产品；改用可审计推导，但不降低证据门槛。
- 缺行当停牌、当天有成交当开盘可交易、单源或“基本一致”即放行：无法排除复牌时段、单位与供应商错误，拒绝。
- UNKNOWN 填零、从 Universe 删除或风险披露后继续宣称 P0 已解决：混淆成员、可评分和执行状态，拒绝。
- 新 Factor 静默继承旧版本，或 Risk Exit 等待正常调仓：分别污染缓存/重放、延长已知终止风险，拒绝。

## 后果与后续验收

需新增历史生命周期、双时态属性、推导状态/证据、方向门禁与风险轨迹契约；本 ADR 不发布实现 schema。实现须覆盖代码复用、迟到公告、属性变更、上市前/终止后、61/60/20 Session 边界、空 Eligible Set、开盘停牌但盘中复牌、双源冲突、涨跌停双方向、UNKNOWN 全延期和无全市场 Session 等合成场景。空合格池可持现金，不等于覆盖失败可成功。

[#171](https://github.com/ZombieIce/PrajnaQuant/issues/171) P24 的 3 ETF × 10 Session 金标准与本地真实证据、P25 重放仍是后续验收，不由设计替代。#172 未决项和许可门禁保持关闭；只在证据范围内声称 PIT，6a 不解释为策略历史业绩，6b 最终总回报另行验收。旧 A 股系统冻结为对照。
