# Universe 契约（v1）

状态：**冻结的实施契约**。本契约约束 Universe 首版模型、时间语义、实验引用、API 与前端；历史成分数据、API 和 UI 是否已落地以 `docs/STATUS.md` 为准。它不把 `sh000300` 指数价格视作沪深300成员数据，也不代表股票回测已经受支持。

## 1. 身份、版本与存储

- `universe_id` 是稳定 UUID；`version_id` 是每次发布生成的新 UUID。名称和简介属于可变定义元数据，不能作为身份。
- Universe 创建后有一个草稿；草稿可编辑、可删。发布把完整定义及成员复制成不可变版本，内容哈希为 SHA-256 对规范化 UTF-8 JSON（字段稳定排序、日期 ISO 8601、无多余空白）。发布后不得覆写或删除；归档只禁止新运行，不影响旧实验读取。
- 用户配置存为服务输出目录下 `universes/<universe_id>.json`，以原子临时文件写入后 rename。服务进程以单个写锁串行化 CRUD；该目录仅由运行中的研究服务写入。仓库 DuckDB 仍由现有 `ashare-warehouse` 写入流程单写者管理；serve 不写 DuckDB。历史指数来源事实不混入用户配置。
- 人工配置与报告文件不构成多进程共享数据库；并发运行的持久化、跨进程写入和远程写 API 不属于首版能力。当前服务无认证，写接口仅允许绑定 loopback 地址；绑定非 loopback 时拒绝启动写路由或启动服务。

## 2. 成员来源与时间

成员标识使用稳定 `instrument_id`，同时保留 `exchange`、`code`、名称快照及 `asset_type`。成员区间为半开区间 `[effective_from, effective_to)`；未给 `effective_to` 表示未设上界。日频候选的决策截止固定为 `Asia/Shanghai` 当日 15:00。查询日期缺失、证券身份有歧义或成员区间冲突时返回校验错误，不猜测。

- **manual**：草稿成员来自用户选定证券，可配置生效区间。发布时记录 `history_claim=retrospective_static`；将今天选择的名单用于过去日期，不得标作历史 PIT 成分。来源是 `user:<actor>`，服务目前无身份系统时使用 `local-user`。
- **index_history**：数据仓库中的每个成分事实必须包含指数代码、证券、有效区间、`published_at`（历史公开时刻）、来源引用、来源快照/文件哈希及验证状态；`observed_at` 只代表本地采集时间，不能代替发布时间。某截面只有在被来源证明为完整截面时，缺行才可表示不在该截面；缺文件/缺日期/局部数据绝不表示退出。
- 日 T 收盘按 T 决策截止时可获得的成员事件逐条重建当时状态：加入事件只有在其 `effective_from <= T` 且已公开时才生效；退出事件只有在其 `effective_to <= T` 且该退出已公开时才生效。不得先用今天完整的有效区间截断历史再忽略公告时间，否则迟到退出会错误改写公告前的候选集合。晚于截止时刻发布的信息最早从下一决策日可用；提前公告但尚未生效的成员不得提前进入。缺少可靠公告时刻的区间不得标 `verified_pit`。
- `pit_status` 取 `verified_pit`、`retrospective_static`、`unknown`。任一解析日来源不完整、可知时刻未知或有覆盖缺口时，结果按 `unknown` 标识；严格 PIT 运行必须拒绝该日期范围。成员加入/退出不是由行情覆盖或停牌状态推导。

## 3. 解析、数据覆盖与动态池

解析结果有独立 `membership_hash`（规范化的 `as_of`、instrument_id 排序列表、所用来源修订 hash 和 PIT 等级）。解析只决定每日候选成员掩码，不裁剪价格面板：开始日前保留足够历史用于 rolling 预热，退出后保留行情用于既有持仓估值、卖出及评价标签。因子 `forward_return` 仅用于评价标签，不进入成员解析、评分、排名或回测决策。

覆盖状态分为 `complete`、`gaps`、`unverified`。成员覆盖与行情覆盖分别报告，不能互相替代。当前 ETF 行情覆盖读取输出目录中创建时间最新且 SHA-256 与 manifest 相符的不可变 Parquet 快照；按所选证券逐日统计行数、首末日期、缺失日期、重复日期和 OHLC 异常，并返回 snapshot ID/hash。对照日历暂以所选成员实际出现日期的并集为准，因此可发现成员间不一致，不能发现全体成员同日缺失的市场级缺口；无任何成员行情时状态为 `unverified`，不能把“没有观察到日期”判为完整。该检查不证明交易日历、上市/退市、停牌或可交易状态。

缺少数据的日期范围必须返回具体缺口和来源，不得以前后成分补齐。严格 PIT 不完整区间拒绝；非严格手工回溯运行可允许并在报告标 `retrospective_static`。成员无行情时不生成行情值、不按零价处理；策略可仅用当日有效且有足够预热数据的成员评分。缺执行开盘 bar 时沿用现有未成交行为，持仓保留并以最近可用收盘价估值，报告必须披露未成交/陈旧估值；新报告逐仓有最近估值日期与陈旧自然日数、执行模式字段。旧快照无状态列时模式为 `legacy_bar_only`，不能据其成交推断可交易；这不是可交易状态证明。

**空池规则（确定语义）**：仅当已解析的成员集合确实为空、且当日轮到调仓时，生成明确空目标并进入 pending；下一有回测行情的开盘按现有成交规则尝试卖出全部持仓。成员集合非空但评分暂缺（如预热不足、成员缺行情）时，不得误判成空池，不创建空目标。卖出缺 bar 时持仓不消失、不按零价估值，结果须保留未完成目标及警示；空池不得静默变成暂停并继续声称目标仓位为空。若尚未到调仓点，不创建新目标。最后一个行情日产生的 pending 与当前引擎一致，不执行，并需在结果中标注。

## 4. 股票运行能力门槛

能力枚举为 `membership_ready`、`factor_research_ready`、`strategy_backtest_ready`，各含 `ready: bool` 和阻断原因。所有资产可管理；要运行股票因子，至少需股票行情快照和已验证日期/证券覆盖。要以股票池运行 ETF Rotation，当前一律拒绝，除非逐项验证股票行情、交易单位/整手规则、费用税费、停牌及涨跌停等可交易状态、公司行动/收益口径、缺 bar 处置，并通过时序与逐日账本金标准。沪深300成分为股票，指数成员数据 ready 不会自动令股票回测 ready。ETF Rotation 现有可运行门槛仍要求 ETF 范围及现有合格行情数据。

## 5. 实验冻结与兼容

新实验配置必须明确 `universe_id` 与 `version_id`，并冻结以下 `universe` 报告身份：`universe_id`、`version_id`、名称/来源/范围快照、定义 `content_hash`、解析口径版本、每日成员 `membership_hash`、来源修订/文件哈希、覆盖与缺口、`pit_status`、能力判定，以及配套行情 `snapshot_hash`。更改草稿、名称或发布新版本不会改变已有实验。

读取旧 JSON 时 universe 元数据字段为可选；不存在时映射为 `universe_status=legacy_universe_unknown`，不合成 ID/版本，不拒绝加载，也不允许被当成指定 universe/version 的匹配结果。筛选指定版本时只返回明确带有相同 `universe_id` 和 `version_id` 的新实验；旧实验只在“全部/未知”视图出现。

凡使用 Universe 的研究、评分或中间结果缓存，键至少包括 `version_id`、定义 hash、成员解析口径版本、来源修订 hash、查询日期范围和决策截止规则；缺任一项不允许共享缓存。运行产物自身冻结配置和快照，读取时不回查当前 universe 定义。

## 6. HTTP API v1

JSON 日期用 `YYYY-MM-DD`；错误统一为 `{"error":{"code":"...","message":"...","details":...}}`。列表排序稳定；版本按创建时间升序。分页使用 `limit`（默认 50，最大 200）和不透明 `cursor`；首版也可返回小型全集，但响应仍包含 `items` 和 `next_cursor: null`。

| 方法与路径 | 请求 | 成功响应 / 语义 |
| --- | --- | --- |
| `GET /api/v1/instruments` | `?q=<code-or-name>&asset_type=etf\|stock` | `{items:[{instrument_id,exchange,code,name,asset_type}],catalog}`；基于本地最新 ETF 研究快照按代码前缀/名称包含检索；当前不提供股票名称目录，股票允许手工填写，不按代码猜名称 |
| `GET /api/v1/universes` | `?asset_scope=&status=&q=&limit=&cursor=` | `{"items":[UniverseSummary],"next_cursor":null}` |
| `POST /api/v1/universes` | `{name,description,asset_scope,source_kind,source_ref?}` | `201 {universe,draft}`；创建草稿，来源 kind 为 `manual` 或 `index_history` |
| `GET /api/v1/universes/{id}` | — | `{universe,versions:[UniverseVersionSummary],draft?}` |
| `PATCH /api/v1/universes/{id}/draft` | `{name?,description?,asset_scope?,source_ref?,members?}` | `{universe,draft}`；`members` 是替换整个草稿成员集的完整列表 |
| `GET /api/v1/universes/{id}/versions/preview` | — | `{changed,current_version_id,next_version_number,changed_fields,draft_hash}`；比较草稿与最近版本，发布前确认用 |
| `POST /api/v1/universes/{id}/versions` | `{expected_draft_hash?}` | 变化时 `201 {version,created:true}`；相同则 `200 {version,created:false}` 且不新增版本；确认后的 hash 若过期则返回冲突 |
| `DELETE /api/v1/universes/{id}` | `{}` | `{universe}`；草稿物理删除，存在已发布版本则归档 |
| `GET /api/v1/universes/{id}/members` | 必填 `version_id`、`as_of` | `{universe_id,version_id,as_of,members,pit_status,coverage,warnings,membership_hash}` |
| `GET /api/v1/universes/{id}/coverage` | 必填 `version_id`，可带 `start,end` | `{universe_id,version_id,coverage,membership_coverage,market_data_coverage,snapshot,calendar_snapshot,calendar_status,pit_status,capabilities,sources,gaps,requested_range}`；ETF 行情覆盖来自 hash 校验后的本地快照，逐成员返回日期覆盖与数据异常。若快照含 hash 校验通过的官方交易日历旁车，按该日历开市日校验共同缺行情，并校验请求范围每个自然日均有日历记录；缺少或不完整时不得推断闭市或标记 complete |
| `GET /api/experiments` | 可选 `universe_id,version_id` | 现有摘要列表；筛选只匹配显式相同身份，省略参数保持旧行为 |
| `GET /api/v1/factor-reports` | 可选 `signal_key,universe_id,version_id` | `{items:[report summary]}`；新报告支持 universe 过滤 |
| `POST /api/v1/runs` | `{kind:"strategy"|"factor",universe_id,version_id,config}` | `202 {run_id,status:"queued"}`；能力不满足为 `422 capability_not_ready` |
| `GET /api/v1/runs/{id}` | — | `{run_id,status,progress?,result_id?,error?,universe?}`；首版运行 API 只有在实际有排队/状态持久化实现后开放 |

错误码至少包括 `invalid_request` (400)、`not_found` (404)、`conflict` (409)、`coverage_gap` (422)、`capability_not_ready` (422)、`storage_error` (500)。成员对象字段为 `instrument_id, exchange, code, name, asset_type, effective_from, effective_to, published_at, source_ref`。响应不得省略警告或用 `sh000300` 的行情覆盖伪造成员覆盖。

### ETF 行情日历覆盖语义

研究快照可携带 `trading_calendar.parquet` 旁车，旁车文件 SHA-256、日期边界、行数和来源写入 manifest 并随 ETF bars 一起冻结。仅导出经完整自然月验证的深交所官方日历；自然月必须包含该月每个日历日，不能以工作日推断交易日。请求日期范围只有在每天均有日历记录时 `calendar_status=complete`；任一自然日缺记录则为 `gaps`，不把该日当休市日。`market_data_coverage=complete` 还要求每只成员 ETF 在官方开市日均有有效 bar，且无重复日期/OHLC 异常。

旧快照允许没有日历旁车，但响应必须 `calendar_status=unverified`，覆盖不能声称已由独立日历证明。当前日历来源是深交所官方日历，用于五只 MVP ETF 的共同交易日对账；上交所交易日历尚未独立交叉核验，因此这是一项行情日期覆盖证据，不是跨交易所数据源完整性认证，也不证明历史上市状态、历史成员 PIT 或分红总回报。

## 7. 筛选与运行的交互边界

策略/因子页面分别明确两个独立操作：

1. **筛选已保存结果**：只向列表 API 传 `universe_id`/`version_id`，不会触发计算。
2. **发起新运行**：选择确切不可变版本，提交运行 API；能力未就绪时禁用并展示阻断原因。被筛选出的旧报告不得悄然成为重跑输入。

旧报告显示 `Universe 未知（旧实验）`；手工历史回测显示 `回溯静态集合`；历史指数资料未验证显示 `历史成分未知/覆盖不完整`。页面只呈现 API 的成分、覆盖与能力结果，不自行推导。

## 8. 管理页展示与归档语义

- 默认展示名称、简介、来源、资产范围、生命周期状态、普通序号版本（如“版本 1”）、成员与日期状态。稳定 ID、版本 ID、内容哈希属于关联、筛选和完整性校验字段，不在日常界面中占主要位置；需要排查或复制引用时放入折叠的“技术信息”。
- 归档不是删除：保留 Universe 配置、不可变版本及已有实验的身份关联；保留查询和历史结果筛选能力，但禁止修改草稿或发布新版本。只有从未发布过的空草稿才允许物理删除。归档状态应清楚标记。
- 推荐流程为创建草稿 → 编辑元数据和成员 → 检查日期成员、覆盖与运行能力 → 发布版本 → 在策略/因子新运行中选择确切版本 → 按 Universe/版本筛选已保存结果。当前最后两步中的新运行 API/队列尚未实现，不能把结果筛选误作重新运行。
- 发布只冻结当次定义快照，不锁死整个 Universe：发布版本 1 后，草稿仍可编辑，修改后再次发布生成版本 2；版本 1 的成员和定义不能改写。成员录入可按代码前缀或名称匹配本地 ETF 快照，选中匹配项后自动填入名称、市场和类型。股票名称目录未接入时仍可手工填写。
- 管理页发布按钮应在草稿编辑视图前部持续可见，并在操作附近显示进行中、成功或失败状态。PIT/覆盖解释解析可信度和数据完整性，能力状态解释允许的研究/回测类型；详细说明和指定日期成员表默认收起，用户按需展开查看。
- 成员表使用“代码 · 名称”展示证券身份，不在普通名单中拼接交易所前缀或内部 instrument ID。
- 发布前通过 preview 比较草稿与最近版本的规范化内容；有变化时明确列出变化项并确认，确认 hash 与发布时草稿不一致则拒绝发布。无变化时提示且不创建新版本；服务端发布接口本身也按内容哈希幂等去重。

## 9. 独立手算 fixture：3 证券 × 10 交易日

这是 Universe 成员与可知时点的契约 fixture，不依赖行情导出代码。日期 D1–D10 是依序交易日，决策截止为当日 15:00；取证券 A、B、C。指数初始完整截面 D1 发布且覆盖已验证，含 A、C；在成员表中表现为 A `[D1,D8)`、C `[D1,D6)`。调整事件如下：

| 证券 | 生效区间 | 公开时间 | 数据事实 |
| --- | --- | --- | --- |
| A | 初始成员自 D1；退出有效日 D8 | D1 14:00；退出 D8 16:00 | 初始完整截面；退出在当日决策截止后才公开 |
| B | `[D4,D10)` | D3 14:00 | 加入事件；D4 生效前已公开 |
| C | 初始成员自 D1；退出有效日 D6 | D1 14:00；退出 D5 16:00 | 初始完整截面；退出在前一日决策截止后公开 |

手算规则是只重建当时可知的成员事件，并按有效日应用已公开事件。因此候选集合：D1 `{A,C}`，D2 `{A,C}`，D3 `{A,C}`，D4 `{A,B,C}`，D5 `{A,B,C}`，D6 `{A,B}`，D7 `{A,B}`，D8 `{A,B}`，D9 `{B}`，D10 `{}`。C 的 D6 退出已在 D5 收盘后公开，故 D6 生效时可用；A 的 D8 退出在 D8 15:00 决策截止之后才公开，因此 D8 仍保留 A；D9 首次能使用该信息，故 D9 候选仅 B。

D10 候选为空时，回测明确生成空目标；若持仓为 1 股 B，下一可用行情开盘尝试卖出，若 B 缺该开盘则仍持有 B，继续用最近收盘估值并报告未完成卖出。D9 的 A 成员变更不会裁掉其出池后行情。入池前 A/B/C 全部行情仍可加载作因子预热；`forward_return` 只用于事后评价。

数据覆盖 fixture：D1–D10 完整截面及来源 hash 时 `coverage=complete`；缺 D6 截面/公告且无可证明的完整调整公告时 `coverage=gaps`、D6 及受影响后续范围为 `pit_status=unknown`，不得推断 C 在 D6 退出；严格 PIT 拒绝该区间。该规则与“有一行成员有效区间结束”区分：区间结束只有在来源完整且退出事件可信时才是事实。
