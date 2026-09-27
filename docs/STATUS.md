# Project Status

实现扫描日期：2026-09-25；目标路线更新：2026-09-26。状态含义：Implemented=代码存在；Partially Implemented=只覆盖部分契约；Planned=有明确路线但无实现；Missing=未见实现；Unknown=仓库证据不足。**Implemented 不表示量化正确性已被充分测试或数据已被验收。**代码仓库已关联 GitHub `ZombieIce/PrajnaQuant`；本机被忽略的数据仍不是可交接资产。

## Target Architecture Baseline (2026-09-26)

2026-09-27 POC 构建资源边界已实现：默认应用仍启用 `app` feature；使用 `--no-default-features` 的 benchmark 依赖图排除 DuckDB 和仓库数据库 crate，并复用 workspace `target/`。warm-cache dev/release 构建及空间增量见 [原始记录](../poc/poc0-benchmark/README.md#shared-lightweight-poc-build-path)；cold build 未测，未清理缓存。测量时按 10 GiB 保留线执行闸门。

用户已确定新主路线为 Rust-first、多市场、三级回测引擎与未来实盘的平台；现阶段先做 [`POC-0`](poc-0-benchmark-spec.md)。完整目标见根目录 [`ARCHITECTURE.md`](../ARCHITECTURE.md)，新交付顺序见 [`product-roadmap.md`](product-roadmap.md)。目前没有 Vector / Fast Event / Accurate 三级实现、Nautilus Adapter、持久 Factor Cache、跨市场账户、Python/PyO3 策略入口或 Live Runtime。现有 React 页面是已实现资产，新平台 MVP 不以 Web 为验收项。以下扫描记录描述旧 A 股纵向场景的代码与数据证据，不能升级为新目标能力。

POC-0 工作范围与验收已发布为本地 [`ready-for-agent` spec](../.scratch/poc-0-benchmark/spec.md)。独立 [`poc/b1-layout`](../poc/b1-layout/README.md) 标量读取 smoke 只对拍三种布局的动量；票据 02 的 SoA、票据 03 的 Arrow 完整 S2 候选和票据 04 的 Polars 候选已验收。票据 05 的版本化 Parquet 写入/复读、列/row group 裁剪与三候选对拍已由用户验收：10,000,029 行、404,509,661 字节的源文件在本机 Linux VM 以 Docker 256 MiB cgroup 限制成功复读，子进程 RSS 58,933,248 字节，容器峰值触及 256 MiB、无 OOM。macOS `RLIMIT_AS` 失败记录仍保留。票据 06 已完成确定性 64 × 252 合成面板的 S2 参数扫描与冷热因子缓存对比，正确性和 checksum 通过；Custom SoA 结论为 `defer`，miss/hit 两线程吞吐相对最佳替代分别 +8.99%/+0.32%，未达到 20% 采纳门槛。报告中的全进程 RSS 为 107,905,024 字节；不代表真实市场/PIT。B2 只有票据 07 的最小原型，B3 未启动。

2026-09-27 票据 05 最初在依赖解析阶段受阻；现以最小本地 `polars-io` vendor 补齐离线 Parquet 路径。实现、资源实测、macOS 受限内存启动失败与随后 Linux cgroup 复读通过的证据见 [票据 05](../.scratch/poc-0-benchmark/issues/05-parquet-and-out-of-core.md)。

2026-09-27 实现与验收：票据 04 Polars 已由用户确认并标记 `resolved`。`--candidate polars` 通过 Polars lazy expressions 计算加权因子评分、排序、TopK、按实际入选数量均分的目标权重及下一交易日组合收益；正确性通过后才测量。报告保留五个原始样本、阶段计时、DataFrame 构造时间和投影 checksum。固定 3 ETF × 10 日 fixture 仅验证语义与 CLI，不支持布局决策。当前工作树也包含此前已有的 Arrow 候选改动，最终提交须保留其独立归属；验证状态以本轮 HANDOFF 为准。

POC-0 票据 01 已验收：新增固定 3 ETF × 10 个交易日的版本化 fixture、独立排名/信号/事件账本金标准，以及 correctness-first 的 `benchmark-poc0` CLI。金标准通过后会记录 Rust 参考回测的原始耗时；不通过时输出差异报告并跳过计时。机器/代码/数据 provenance 已进入报告；该票据尚未测峰值 RSS，报告将其明确标为未知。这不是 B1 完整布局比较；当前实现位置和复跑方式见 [`POC-0 harness`](../poc/poc0-benchmark/README.md)。2026-09-27 复验：全工作区 83 项通过、1 项按既定条件跳过，格式、Clippy 与 release CLI 均通过。

POC-0 B2 票据 07 已于 2026-09-27 复核验收并标记 `resolved`：最小 Fast Event S1 Buy & Hold 原型通过固定输入与变体 fixture 检查停牌拒单/重试、缺 bar 沿用估值、末日未执行目标、逐日 NAV 恒等式、重复账本 checksum 和固定成本（包括非零买入税）；报告分列初始化、事件处理和端到端五次耗时。独立手算金标准逐日现金/持仓/NAV 与非零买入税变体均与报告数值一致。它仍不是完整 Fast Event Engine，也只有微型合成样本，因此不能支持性能选型。

POC-0 已按获确认的拆分发布为 [13 张本地执行票据](../.scratch/poc-0-benchmark/issues/01-fixed-dataset-and-benchmark-entry.md)。01–06 为 `resolved`；03 的验收限于 Arrow 候选实现与正确性，release 结论由票据 06 的同条件 sweep 补齐。06 对本轮合成目标负载得出 `defer`，不是平台级布局裁决。07 的最小原型仍未作为完整 Fast Event 验收。

完整的问题排序、解决路径及验收判据见 [`priorities.md`](priorities.md)。这里保留按领域归类的事实状态。

## Current Phase

此前 A 股股票/ETF 日频 + Web 的初级产品路线已归档到 [`legacy-ashare-roadmap.md`](legacy-ashare-roadmap.md)；当前仍是本地 ETF Rotation MVP 加 A 股仓库。Batch 2 已有显式证券增量同步/失败恢复/不可变发布代码，隔离股票+ETF 真实小样本通过；已发布快照的只读证券/日线 API 和 K 线页面在合成目录通过。Batch 3 的日更身份导入与 ETF 诊断作业已有代码；生产同步/调度、真实状态、可信历史作业、Python 研究包和远程认证部署仍未验。五 ETF 真实状态覆盖不足，旧实验仍 `legacy_bar_only`，新 `status_gated` 占位跑全 UNKNOWN，P0-3 开放；股票回测没有开放。

## Batch 2 Integration Acceptance (2026-09-25)

总评**部分通过**；要求、代码/测试/真实样本/浏览器证据及限制见 [`handoffs/batch2-integration-acceptance.md`](handoffs/batch2-integration-acceptance.md)。A：241×5 状态格全 UNKNOWN，只有一条无交易所原文的盘中限制二级消息；同价面板/Universe 复跑 0 成交、24 个 unknown 拒单，只验证缺证拒单。B：仓库迁移 007 和 `sync-daily`/`publish-sync` 支持显式证券增量、修订回看、重试/断点、审计和不可变快照；隔离真实样本重复同步不增有效修订，生产状态 Unknown。C：新增 `/api/v1/instruments` 通用发布快照搜索、`/api/v1/daily-bars` 分页与 `/market` 页面；B 格式的发布指针与 C 只读 API 已接通并测试固定版本。B 真实小样本证券目录缺身份/分类，因此生产股票/ETF 行情页面仍未验。调度脚本未安装/启用。


## Batch 1 Integration Acceptance (2026-09-24)

验收结论为**部分通过**，逐项代码/测试/真实数据/API/浏览器证据见 [`handoffs/batch1-integration-acceptance.md`](handoffs/batch1-integration-acceptance.md)。A 执行延期与审计、B 通用冻结快照/状态输入、C 发布版本/固定快照复跑在共享工作区完成集成。最终真实实验 `4e076758-8069-4e0e-9269-1d3f9d574159` 使用 Universe `d8811237-6c37-4189-86b8-9b05fbccc405` 的版本 `3fcfed9c-b8c1-4cce-a933-bc2b79ebf0da`、快照 `fe1dd8a3-d227-4e06-bb98-f0a6b3be0f34`；241 日、32 笔成交、逐日账本最大 NAV 误差 0。快照没有交易状态列，报告 `execution_status_mode=legacy_bar_only`，所以真实执行状态仍未验收。五 ETF 行情日期覆盖完整，但逐行来源缺失使总体审计为 `unverified`；Universe 成员是 `retrospective_static`，实验成员覆盖为 `gaps`，不升级为 verified PIT。

## P0 — Correctness

1. **历史 ETF universe 的幸存者偏差（已确认机制）**：`sql/schema.sql::research.etf_daily_bar` 按 `core.instrument.asset_class` 当前值过滤历史；`data.rs` 对该视图全量出快照。无交易所历史名录、上市/退市有效期和可知时刻，也无最短上市历史门槛。历史回测的选股全集不可信为 PIT；以当前观察池解释结果。
2. **ETF 原始价不是总回报（已确认机制）**：`data.rs` 读未复权 open/close，`backtest.rs` 未处理分红/拆分/现金派发，实验假设也明确分红只在源价体现时才反映。长期收益/排名可能失真；影响大小待逐产品验证。
3. **Top-N 目标与实际持仓偏离（超额持仓触发机制已有保护，P0 暂不关闭）**：旧仓无执行日开盘 bar 时曾跳过卖出、仍用余现金买新目标。本机 5 个旧 Top-5 实验的 `equity_curve.positions` 最大分别为 7/6/7/7/7。现规定：任一需要卖出的旧仓缺当日 open 或状态不明确/不可交易时整次调仓零成交并保留待执行目标；目标内旧仓缺 open 时也保守延期；每次延期写入 `backtest.rebalance_deferrals`；期间新的调仓信号替换旧目标。独立手算场景验证旧仓 A 缺价时不再买入 B、最新信号 C 替换 B 后才完成 A→C。新快照已带状态门槛；状态拒绝和目标买腿缺 open/状态拒绝均写入 `backtest.unexecuted_orders`。报告新增明确的 `execution_status_mode` 和逐仓 `mark_date`/`stale_calendar_days`，旧报告缺这些字段时显示未知；旧 Parquet 快照继续 bar-only 兼容；目标买腿单独被跳过时策略可能暂时少于 Top-N。超额持仓触发机制有测试保护，但真实状态源完整性、旧快照风险和目标/实际成交语义尚不足以关闭 P0。

P0-1 历史 ETF PIT、P0-2 公司行动/总回报仍未解决；P0-3 超额持仓触发机制、交易状态 fail-closed 和未执行腿审计已有代码及合成测试，但状态来源历史覆盖/可知时点、旧快照兼容路径与目标 underfill 语义仍未验收，因此继续标为未解决。静态检查**没有找到**未来收益进入策略分数、rolling 读未来下标、反向 shift、backfill 或全样本归一化。严格 PIT、分红和真实市场交易可执行性仍未被完整证明。`docs/time-model.md` 细述时序和未决边界。

## P1 — ETF Rotation MVP

轮动评分、趋势过滤、Top-N、等权目标、下一行情日开盘成交、成本和参数网格已实现；因子/策略配置没有正式解耦；Universe 已有管理 API 和版本感知 runner 入口，但既有 CLI/Web 尚未连接运行入口，benchmark、归一化、权重模式未可配置。常规仓库快照运行需要合格 ETF 历史和至少 1000 条沪深300基准行；本批隔离五 ETF 场景明确不使用基准。优先任务见 HANDOFF，仅推荐一个。

## P2 — Research Platform

Signal Registry Lite、单因子 Pearson/Rank IC、分组/multi-horizon 报告、结果读取 API 与 ECharts 图已实现一部分；新增的 Universe 写 API 仅管理配置，不启动作业。通用因子版本、参数元数据、缓存、因子相关矩阵、因子值分布、组合换手、研究 Python 包、实验服务 API 均缺失。当前报告的自相关与 Top 分位成分更换率是部分诊断，不是组合换手。

## P3 — Future

严格 PIT ETF/股票数据、财务披露时点、行业/指数历史成分、分钟/实盘、分布式执行、生产级数据发布和十年数据查询优化仍是未来方向；旧设计文件详述目标，不能视为当前状态。

## Completed

- Rust 仓库：原文/hash/run、版本化日线、ETF 当前分类、基础日历/状态/复权因子表与研究视图、Parquet 导出。
- Rust 研究：快照 manifest/hash、带日历与标签口径的单因子评价（Pearson/Rank IC、coverage/missing、Top 分位更换与分数自相关）、综合轮动评分、批量参数扫描、T 后下一行情日开盘模拟、绩效/成本汇总、实验 JSON。
- Axum 原有结果 GET 与新增本地 Universe CRUD/筛选路由；React 因子目录/详情、策略目录/详情、API 驱动的 Universe 管理与已保存结果筛选页面。`apps/web` 可构建。
- Universe 手工成员与 `/api/v1/instruments` 已可查询发布快照中的证券；股票候选保留未核验分类，不能当作历史 PIT 名录。发布只冻结本次版本，草稿继续可编辑并可发布后续版本。
- Universe 页面将发布操作放在草稿视图顶部的吸附操作栏，并在按钮旁显示发布进度/结果；PIT、覆盖与运行能力说明可展开，日期成员明细默认收起。
- Universe 发布新增服务端 preview/hash 再校验：确认前展示与上一版本的差异，有变化才允许发布；草稿内容相同则不新增版本，过期确认会被拒绝。成员表按“代码 · 名称”展示。
- 策略详情新增持仓资金占用率及各标的持股数量曲线，两者与净值/回撤共用所选日期区间；新增逐标的已实现、浮动与总盈亏表。盈亏使用移动加权平均成本并披露成本/估值规则；旧报告无字段时明确提示重跑。口径与兼容规则见 `docs/decisions/0006-instrument-pnl-attribution.md`。
- 发布差异接口与重复发布保护有单测；Batch 2 集成全工作区为仓库 14 项、研究 56 项通过，真实五 ETF opt-in 1 项默认 ignored。格式、Clippy 和前端构建均通过；真实状态覆盖仍未验。

## In Progress

Batch 2 的增量同步/行情页已有代码、隔离小样本和合成浏览器验收；仍待可信五 ETF 状态源的历史覆盖与可用时刻、带真实状态快照复跑、生产股票证券身份与日线验证、生产调度启用、历史 PIT 成员和公司行动。既有 `research-output/` 历史样本不自动升级为新规则结果。

## Missing

- 历史 ETF 名录与上市/退市日期、可信历史交易状态源及可知时刻、涨跌停规则执行、公司行动/分红总回报。
- 通用 Factor Registry/版本/持久缓存、因子值分布统计和相关矩阵；独立订单流水/可查询持仓时序 API；统一策略 ID/版本。回测报告内已有逐日持仓快照，但尚无独立持仓查询接口。
- 后端 Sortino、Win Rate、Benchmark Return、Excess Return、Tracking Error、Information Ratio。
- 历史指数成分 provider 和完整覆盖证明；当前指数 Universe API 对历史成分明确返回空、unknown/gaps。
- React 的基准/超额净值、月度收益热图、年度收益、逐日持仓权重、组合换手、成交与交易成本明细；因子值分布与相关热图。策略详情现有逐日资金占用率、持股数量和期末逐标的盈亏摘要。
- 实验/Universe API 的完整 cursor 分页、日期范围与大型序列聚合仍缺；证券搜索和日线 API 已有范围与 cursor，但十年生产性能未验。Universe 历史成分 provider、完整覆盖的独立跨交易所日历尚未连接；ETF 快照已有 SZSE 完整月份旁车及共同缺日校验；异步运行状态/队列与网页发起新运行尚无。独立 CI、迁移回填/版本管理工具、Python 研究包。
- 初级产品目标已有显式证券列表的股票/ETF 通用研究快照、增量同步/恢复、证券 K 线 API/页面代码及合成或隔离小样本测试；仍缺生产股票快照验收、股票回测、正式 Python 因子研究互通、已启用的日更调度与远程认证/部署配置。现有 Rust 本地 CLI/loopback API 不等于这些生产能力。

## Known Bugs

- 旧 `factor_dashboard.html` 仍有重叠收益累计图，但**不被当前服务使用**。轮动策略被误标成单动量的页面问题已在本批修复，见集成验收；若 API 摘要无新 `score_mode`，页面标为评分口径未知。

## Test Coverage and Gaps

- 新增的固定样例覆盖真实 `rotation_scores`→Top-1→回测路径、D 收盘信号/下一行情日开盘成交、买卖与换仓、100 股手数、比例佣金/最低佣金、双向滑点、末日信号未执行、每日现金/整数持仓/最近收盘估值与 NAV 恒等式，以及缺单 ETF bar 时跳过卖出和沿用旧 close。固定场景手算成本为佣金 700、税 0、滑点 630、总成本 1,330；这些是合成数据回归值，不是实际数据验收。
- 回测 JSON 现记录逐日证券持仓、市值及资金占用率曲线和逐标的盈亏归因。3 ETF×10 日固定账本校验每日现金+持仓市值=权益、占用率公式、加权平均成本归因，以及标的盈亏合计=组合期末权益变化；旧 JSON 缺字段时保持可读并显示无持仓快照提示。策略页展示占用率、标的持股数量曲线和盈亏表。盈亏契约见 `docs/decisions/0006-instrument-pnl-attribution.md`。
- 仍缺：真实交易所状态源历史覆盖/可知时点、涨跌停 side-specific 成交限制、旧快照重跑与前端未执行订单展示；逐日权重与组合敞口报告/通用不变量接口；历史 universe、分红/拆分情境；真实交易所执行日验证。合成状态回测现覆盖 `UNKNOWN` 买单和 `HALTED` 卖单、状态缺失拒绝、延期恢复、逐腿原因和逐日现金/持仓/NAV；快照测试覆盖状态列读取与旧格式兼容。基准当前仅保存原始 close 点，测试只验证输入点保留，不证明日期对齐后的基准收益或超额收益。
- 新增固定答案覆盖手算 Pearson/Rank IC、截面不足/null、分母 coverage、开盘标签的市场日历对齐与缺 ETF bar 留空、波动率首窗口完整收益；React 因子详情改为展示逐期前瞻标签，不复利。实际无日历文件时使用观察 bar 日期并集，无法识别所有 ETF 同日缺行情；报告契约见 `docs/decisions/0004-factor-evaluation-contract.md`。
- 新增的 3 ETF 缺价目标切换手算用例先在旧实现上失败（D3 现金被买单用尽、实际持仓变成 2），修复后 D3 整次调仓延期、保留 A×100 和现金 4,000；最新信号 C 替换旧 B 目标，D4 卖 A 买 C×200、现金 2,000，逐日 NAV 10,000 且正仓数不超过 Top-1。回测报告记录延期目标/阻塞证券/日期/原因；策略详情展示末日实际正仓与延期记录。
- Universe HTTP handler 的 test-only 3 证券×10 交易日合成 fixture 覆盖 D4 三成员、D10 空池、完整覆盖与能力阻断；该 fixture 的 `verified_pit` 仅代表合成真值。前端构建通过。真实历史成分和生产数据验证未完成。
- 既有测试另覆盖仓库入库/版本/锁、基础 rolling、排名统计、手数及单笔成本、参数展开。

## Technical Debt

- Rust 与 Python 重复行情入库原型；Rust 动量/轮动分数有多条路径，统一口径/版本未完成。保留现状，权威边界见 `architecture.md`。
- `backtest.rs::metrics` 是权威；未使用旧 `dashboard.html` 自算绩效。React 计算最多 60 个报告日的简单滚动 Rank IC 均值；它是展示派生值，未持久化在 Rust 报告。
- `server.rs` 完整加载/返回大 JSON，无分页/降采样；API 前缀 `/api` 和 `/api/v1` 不统一；前端 JS 构建约 1.27 MB（本轮 build 警告）。
- 本轮对本机 402 MB DuckDB 只读查询 `research.etf_daily_bar` 分源计数时发生 DuckDB `OutOfMemoryException`，提示临时磁盘使用达到 20.6 GiB 上限。它是该视图大范围查询的已观察可扩展性问题；未分析执行计划前不归因到特定 JOIN。快照 manifest 曾记录约 143.6 万 ETF 行，不能据此推断全市场覆盖。

## Risks

- 缺旧仓卖出 open 或状态不明确/不可交易会延期整笔调仓；目标买腿缺 open/状态不明确/不可交易会跳过并写原因，仍可能造成 Top-N underfill；缺估值 bar 沿用旧收盘价。旧格式快照仍按 bar-only 行为成交；新版状态快照也未执行 side-specific 涨跌停限制。
- 当前 ETF 分类与证券名称来自当前观察，不支持历史退市产品；原始价格复权/分红缺失；`observed_at` 不等于公开时刻。
- 基准仅原始价序列，日期可能与权益点不齐；不能由它声称已实现超额/跟踪误差。
- `signal-research` 当前默认 horizon 按日历开盘标签推进；显式 `close_to_close` 诊断 horizon 按单证券有效 bar 计，不一定等于 n 个市场交易日。旧报告未写入独立因子口径版本。
- 本地数据目录被忽略，新 Agent 无法只凭 Git 仓库复现已有真实数据结果；需注明数据获取/快照来源与 hash。

## Open Questions

1. 研究基准是 ETF 价格收益还是含分红总回报？需要具体 ETF 分红/拆分样本和数据源证据。
2. 历史 ETF universe 的权威名录、上市/终止日期及可知时刻从何获得？未解决前结果只能限定为当前观察池实验。
3. 缺 bar/状态不可交易时旧持仓如何处理？现选择缺旧仓卖出 open 或状态非 TRADABLE 时整笔延期、沿用最近 close 估值并用新周期信号替换延迟目标；目标买腿缺价/状态时跳过并记录逐腿原因。仍需决定旧快照是否强制重跑、报告 UI 展示方式，以及状态/涨跌停来源的验收标准。
4. 基准及其他独立序列的对齐日历、缺值处理与日期范围口径是什么？现代码未定义。
5. 非空 universe 但没有有效评分日是否应暂停调仓计数/清仓？现代码暂停计数、保留持仓。

## A-Share Daily History Backfill — In Progress (2026-09-23)

- Rust CLI 新增 `backfill-market-history`，可回填本机已观察的 SH/SZ `EQUITY_CANDIDATE`、`EQUITY`、`ETF`，2016 起按 800 个自然日窗口调用腾讯不复权日线；原文/hash/URL及逐窗口成功、空、失败状态落库，成功和确认空可断点跳过。运行中每25个窗口输出进度与ETA。
- 腾讯 2016 年日线直连探测有效。单证券全区间验证：`sh510010` 的6个窗口成功，导入2,588行，失败0。通达信官方包直连 TLS 握手失败，因此本批次采用可用的腾讯源。
- 全量回填的目标本机已观察名录为6,882只、约34,410个窗口；最近一次用户提供的终端日志为16,775/34,410（48.8%），此样本核验未读取全局实时进度。5只沪深股票/ETF样本各自5/5个目标窗口均完成，回填源行数与库内不同交易日数相同、失败0，日期范围2016-01-04至2026-09-24。复查命令和具体数字见 [`A股本地数据库建设方案.md`](A股本地数据库建设方案.md)。
- 新增 `audit-trading-calendar` 逐日对照已保存的深交所官方月历与样本腾讯日线。2016-01-01 至 2026-09-24 共3920个自然日，数据库中官方日历覆盖3858日、已确认开市2570日；2017-01月接口只返回30/31天，2019-05多次超时，合计62个日历日尚未覆盖。已覆盖日期上 `sh600000`/`sz000528`/`sz159919` 分别缺18/21/1个开市日行情，`sh600519`和`sh510050`未发现缺日；每个样本证券在两个未知月份各有38个行情日未获日历认证。2017-01不完整原始响应与失败记录已保存在本地 `data-core/raw/szse-incomplete/`。这不是全市场完整性认证，缺行情不能仅据此归因为停牌或源漏数。
- **覆盖限制:** 不含本机未观察到的已退市证券；腾讯接口不支持北交所，且此接口无成交额。`EQUITY_CANDIDATE` 尚未由官方历史名录核验。完成这批也不能称作经过完整性验收的全量 A 股或 PIT 数据。


## Universe Delivery — API/UI Integration (2026-09-23)

- **Implemented in code:** 冻结契约与 ADR 0003；schema v5/v6 历史成员事实、加入/退出公告时间字段、覆盖底座与来源校验；Rust 不可变版本模型、公告/生效时间解析、严格覆盖拒绝、成员映射与日哈希；实验可选版本身份并保留旧 JSON 读取；Universe CRUD/发布/成员/覆盖/能力及结果过滤 API；loopback-only 本地配置单写者存储；ETF manual Universe 的 runner 入口和显式空池下一开盘清仓语义。新运行入口拒绝没有定义对象的 Universe 配置，避免静默忽略。
- **Integrated:** React Universe 管理页面调用实际列表、创建草稿、草稿 PATCH、发布、成员快照、覆盖/能力、归档 API；策略目录及因子研究目录的已保存结果筛选调用实验/因子报告 API。新运行与筛选独立，因 `/api/v1/runs` 和作业状态存储未实现而禁用。前端不再内置 mock Universe 或假报告。旧 experiment 摘要显式标记 `legacy_universe_unknown`；旧因子报告缺少身份时显示 Universe 未知。
- **Not integrated:** CLI 仍不能从 Universe 版本发起服务端新任务；异步运行队列/API 与历史成分 provider 未接入。runner 入口不是服务端可发起的新运行能力。
- **Data blocked:** 已临时导入第三方 `index-constitution` CSI 300 CSV：1221 个可定界成员区间、覆盖声明 2016-01-01 至 2026-06-12；opt-in/opt-out 前 14 个自然日仅是模拟发布时间。成员均为 `unknown`，覆盖 `unverified`，4 条缺失 opt-in 的源行登记为缺口。原文与 SHA-256 已归档。本数据不满足官方公告/PIT 完整性要求，Universe 历史 provider 仍未接线；`sh000300` 仍只有指数价格。股票 ETF Rotation 运行仍被拒绝。
- **Evidence:** `universe.rs` 和 API handler 的 test-only 合成 3 证券×10 日期 PIT fixture 覆盖加入/剔除、晚公告、D4 三成员、D10 空池、完整覆盖和股票回测能力阻断；runner/backtest 测试覆盖按版本应用 manual ETF、版本/hash 冻结、真实空池与无评分区分，以及原有逐日 cash/positions/NAV 恒等式。合成 fixture 的 `verified_pit` 只表示其已知测试真值，不是实际沪深300历史数据验证。
- **Local HTTP check:** 经用户允许 loopback 端口后，以隔离输出目录在 `127.0.0.1:18787` 启动 Axum 并完成浏览器/API 联动：页面创建草稿、编辑简介、写入 3 个合成 ETF 成员、发布不可变版本、查询指定日期成员与覆盖/能力；页面显示 `retrospective_static`、`unverified`、覆盖缺口及策略/因子运行阻断。策略与因子页均加载实际 Universe；因子目录按 Universe 后再按不可变 version 精确筛选，显示临时合成报告身份。初次因子页检查发现坏形状的旧/损坏报告会使目录空白，React 现对缺少 `report.reports[0]` 的报告做防御并提供渲染错误边界，刷新后页面与筛选正常。临时服务和测试数据已停止/清理；这不验证真实市场数据或真实因子表现。
- **Current test status:** 本轮全工作区 Cargo 测试 9 仓库 + 25 研究通过；formatter 和 Clippy 通过。前端本轮未改动；此前 build 有既存 ECharts 单 chunk >500 KB 警告。Python 单测未能运行通过：当前 `.venv` 缺 `duckdb`。
- **当前模拟验证阶段（用户指定）:** 暂缓沪深300真实历史成分的来源、公告时间与完整覆盖核验。API handler 在隔离临时输出目录中读取 test-only 3 证券×10 交易日合成 Universe；Web 页面使用实际 API，不内置模拟运行或生产默认数据。测试中的 `verified_pit` 只表示该**合成 fixture 的已知真值**，不得外推到真实沪深300；第三方 CSV 的模拟公告时间仍为 `unknown/unverified`，股票 ETF Rotation 运行仍阻断。
- **本次模拟测试复核:** `cargo test -p quant-research --lib --release --locked --offline universe` 5/5 通过；同一构建生成的研究库测试可执行文件 23/23 通过；`cargo fmt --all -- --check` 与 `apps/web` 的 `npm run build` 通过。随后浏览器联动确认因子目录/详情报告和 Universe/version 精确筛选可用。测试及页面报告均使用合成数据，不验证真实历史成分或真实因子表现；ECharts bundle 仍有超过 500 KB 的 Vite 提示。

## Web Route / Chart Loading and Strategy Curves (2026-09-24)

- React 页面拆为独立动态路由 chunk；ECharts 使用 `echarts/core` 并只注册当前使用的 line/bar chart、tooltip、legend、dataZoom、grid 和 canvas renderer。构建后入口 JS 232.59 KB（gzip 73.20 KB），详情页共用的图表 chunk 535.01 KB（gzip 178.19 KB）；策略/因子/Universe 页面各自 chunk 约 1.9–14.4 KB。访问目录/Universe 不需要图表 chunk；详情页才加载。图表 chunk 仍超过 Vite 500 KB 提示线。
- 策略详情净值与回撤图已上下排列，共用开始/结束日期选择器。图表只显示区间内有效权益日期；净值以区间首个有效权益值重设为 1.0，回撤以该区间内高水位重新计算；顶部绩效卡仍代表完整回测期。
- 验证：`cd apps/web && npm run build` 通过；在 loopback 浏览器打开保存策略详情，检查两个图表 DOM 顺序/渲染，并将日期区间设为 2026-01-05 至 2026-06-30，确认选择器值更新。Rust 未变更，本轮未重跑 Rust 测试。

## Five-ETF Universe MVP Check (2026-09-23)

- **User-selected scope:** 暂缓沪深300作为验证对象，使用 `513300`、`518880`、`159612`、`510320`、`159952` 五只 ETF。实验区间为 2025-09-23 至快照最后交易日 2026-09-21，名单按手工固定集合处理，报告标记 `retrospective_static`。
- **Existing data found:** immutable ETF snapshot `research-output/snapshots/cb4697c7-6727-4e0b-83e9-3bf31db05371/etf_daily.parquet`，manifest SHA-256 `21ee38bbc955bce9130298ab228b18fe09c0f22cd9395f67e6e8ba5898a959d4`，范围 2016-01-04—2026-09-21。五只 ETF 在所选一年内各有 241 个交易日、无重复日期；检查的 OHLC 约束异常为 0。510320 的快照历史始于 2025-04-25，但所选一年覆盖齐全，因此无需再次抓取行情。快照在库当前 writer 持锁时读取，不触碰该在运行的回填进程。
- **MVP run:** 可用 `cargo run --offline -p quant-research --example etf_universe_mvp` 重现；结果在 `research-output/experiments/1dfe2ade-072b-4ea5-94c8-c61e55ca5ef5/experiment.json`。输入包含 80 个交易日左右的历史预热行情；报告和成交只覆盖选定的 241 日。该次运行不传入沪深300 benchmark，报告 benchmark 为空。5 ETF 回测 32 笔成交、初始权益 1,000,000、最终权益 1,179,713.32、总成本 4,973.08；独立重建逐日现金与持仓市值，最大 NAV 差为 0。
- **Runner correction:** 本次实际验证发现 Universe runner 曾把因子预热日期也作为交易日期。现保留完整输入行情作预热，但把成员快照、因子横截面和回测日期限制到配置区间；新增起始日期边界回归断言。
- **Limits:** 这是技术/接线 smoke run，不作为投资业绩结论。数据是快照中未复权 OHLC；分红/拆分与总回报尚未核对。固定名单没有历史 PIT 含义。该一次性示例生成了带 version/hash 的实验结果，但没有通过 API 把 Universe 配置保存到 `UniverseStore`，现有网页也没有新运行队列。
- **Verification:** `cargo fmt --all -- --check`、`cargo test --workspace --locked --offline`（9 仓库 + 25 研究）及 `cargo clippy --workspace --all-targets --locked --offline -- -D warnings` 均通过。详细数据与限制已记在 `HANDOFF.md`。

## Universe Management UX (2026-09-24)

- 默认管理视图只显示名称、简介、来源、资产范围、状态、普通序号版本及成员/PIT/覆盖/能力；Universe ID、version ID、content hash、instrument ID 与 membership hash 已移出主要视图，稳定标识放到折叠的“技术信息”。
- 页面解释归档：已发布版本和历史结果关联保留、停止草稿编辑与新版本发布；无版本的空草稿才物理删除。合同流程同步至 `docs/universe-contract.md`。
- `cd apps/web && npm run build` 通过；仍有既有的大 bundle (>500 kB) Vite 警告。没有新增 API 行为。

## Universe Unfinished Work Priority (2026-09-24)

1. **P0 — 成员 PIT 与数据覆盖可信度：部分完成。** ETF 覆盖 API 已将 hash 校验的日线快照与完整自然月的深交所官方交易日历旁车一同冻结；缺少或不完整的自然日历记录不推断为休市，覆盖状态 fail closed。对 2025-09-23—2026-09-21 五只 MVP ETF 逐日审计，241 个官方开市日每只均有一根有效 bar，重复、OHLC 异常和非开市日 bar 均为 0。日历来源尚未与上交所独立对拍；历史指数/ETF 成分与上市状态 provider 仍未接入；手工名单仍是 `retrospective_static`，不可标为历史 PIT。此证据只证明该窗口行情日期覆盖，不证明历史可投资性或分红总回报。
2. **P1 — 运行能力门槛：未完成。** 新运行 API/异步队列及完整资产级执行验证缺失。当前 API 能返回阻断项，不足以证明策略/因子可运行；在第 1 项及资产执行条件通过前不得开放运行。

页面已按此顺序标记 P0/P1。细化证据、放行条件见 [`priorities.md`](priorities.md)。

## ETF MVP Default-Investable / Zero-Distribution Scenario (2026-09-24)

按用户指定的 MVP 测试假设运行五 ETF 轮动：每只 ETF 仅在真实行情审计范围内默认可投资，审计前的行情只用于因子预热，不进入候选池；分红/派息现金流假设为 0，结果明确是原始价格收益，不是总回报。测试示例运行时还会重新执行官方日历覆盖审计并 fail closed：范围 `2025-09-23`—`2026-09-21`，五只各 241 根 bar，日历 241 个开市日，无缺失/非开市日/重复/OHLC 异常。成员有效区间按各自审计区间的首末行情日期动态生成；本次五只都为 `2025-09-23`—`2026-09-21`。成员状态仍为 `retrospective_static`，这是假设，不是已验证上市 PIT。

此段早期场景记录由 2026-09-24 的 Batch 1 最终复跑取代；其准确命令与结果见 [`batch1-integration-acceptance.md`](handoffs/batch1-integration-acceptance.md)。早期结果文件：`research-output/experiments/42785f2f-06e8-42d6-a077-fe9db29ecc01/experiment.json`。原始价格测试净收益 17.97%，32 笔成交，总成本 4,973.08；241 个净值点的逐日现金＋持仓市值与 NAV 最大误差为 0。该数字只用于验证 MVP 数据/计算接线，不是含分红总回报，也不是历史无偏业绩。股票能力、ETF PIT 上市状态和公司行动处理仍未完成。

最终验证：`cargo test --workspace --locked --offline`（9 warehouse + 36 research = 45 tests passed）、`cargo clippy --workspace --all-targets --locked --offline -- -D warnings`、`cargo fmt --all -- --check`、`git diff --check` 均通过；指定 MVP 示例运行及其内置日历/成员行情审计通过。
