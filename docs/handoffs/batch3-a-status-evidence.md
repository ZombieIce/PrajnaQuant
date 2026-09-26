# Batch 3 A：五 ETF 固定窗口交易状态证据交接

核验日期：2026-09-26（Asia/Shanghai）。窗口：2025-09-23—2026-09-21（含），锁定五只 ETF：513300、518880、159612、510320、159952。结论是**未取得足够的真实历史逐日状态证据**；冻结数据仅保留证据缺口和 `UNKNOWN`，未导入生产或隔离仓库，P0-3 继续开放。

## 范围身份与原始输入

- 已发布 Universe：`d8811237-6c37-4189-86b8-9b05fbccc405`，version `3fcfed9c-b8c1-4cce-a933-bc2b79ebf0da`，content hash `91aa64f7872ced85b4513f224fa617a14fa12ae99c86f5936085a86466a4ba18`。该五只 ETF 仍为 retrospective-static membership。
- 对照价格快照：`fe1dd8a3-d227-4e06-bb98-f0a6b3be0f34`，Parquet SHA-256 `8c8debb7e1fdd7b3b592cc0991e807b26f1e94a59fb7bbe7e66e00f3030af872`；本回测窗口交易日历来自该快照内哈希的 `szse_official_month_list` 日历，共 241 日。回测价格面板、配置与 Universe 版本均未改变。
- 本批审计脚本版本 `prajna-etf-status-evidence-v1`。冻结目录：[`evidence/batch3-a/frozen/`](evidence/batch3-a/frozen/)，清单 SHA-256 `ded6fc9f942e8f282a9ff43d24c19d7d0d159325c79fce439ff7a5bc195e25ec`；状态行快照 SHA `422f9fec89b56af93c7639915bd104c2bc23cc4875641059496ceb6b25a59df4`；覆盖报告 SHA `d002da4a7a6a4e6265f24f931b7a34d761c777b769a2df7b91b910175ff3611e`；预期开市日 SHA `e4c39876272086c375c37ff9be1137c40a08b130e8b42d4249fa753e0945c875`。
- 来源清单/证据类型为 [`evidence/batch3-a/source-audit.json`](evidence/batch3-a/source-audit.json)，SHA-256 `e6d56634d4f8da0f6a68dcc52074bc56cffef7c4ea39df7b874de1b7121cb5e8`。浏览器检索摘录 hash 是**摘录 hash**，不是原文 hash；所有未保存的交易所文件 `original_path`、`original_sha256` 均明确为空。Batch 2 的既有机器事实 [`evidence/batch2-a/source-facts.json`](evidence/batch2-a/source-facts.json) 只包含一条次级转载的盘中事件，本批没有提升其验证等级。

## 来源审计与范围矩阵

| 来源 | 证券/日期范围和语义 | 取得结果及原文/hash | 历史可用时刻 | 判定 |
|---|---|---|---|---|
| [上交所对外公示数据目录](https://www.sse.com.cn/market/publicdata/) | 沪市基金 513300、518880、510320 的相关入口；目录把“停复牌信息”与“盘中停牌信息”分列。属于事件/提示目录，不是五只 ETF × 每开市日完整表 | 2026-09-26 用网页搜索和页面打开核验；网页原始字节未保存，原文 SHA 未知。保存了带来源说明的短摘录及其摘录 SHA | 目录没有给出目标窗口的逐条公开/可用时刻；未知 | 事件列表静默不证明 `TRADABLE`。 [官方目录](https://www.sse.com.cn/market/publicdata/) |
| [上证所信息网络有限公司业务文档目录](https://www.sseinfo.com/services/assortment/document/) 的 LDDS 历史数据接口文档 | 沪市候选历史数据接口，目录列出正式版历史数据接口说明书 1.1.3（2024-06-21）及 ETF 统计开发稿 1.1.4（2024-06-24）；适用方向可能覆盖三只沪市 ETF，但没有目标日文件/行 | 官方接口文档目录可读；PDF/逐日状态数据文件的原始 bytes/hash 未保存。实际日状态文件、访问权限/分发清单和日期覆盖未取得 | 对窗口内每个文件/状态无 `published_at` 或 `available_at` 证明 | 候选全日来源而非已覆盖证据。不能凭规范反推实际数据存在或完整。 [官方接口文档目录](https://www.sseinfo.com/services/assortment/document/) |
| [深交所 TS03 ETF 技术规范](https://www.szse.cn/marketServices/technicalservice/column/history/P020180328468135161873.pdf) | 讨论 ETF 跟踪指数成份证券停牌数据，不是 ETF 自身二级市场交易状态；不适用于把 159612、159952 的沉默推为正常 | 官方规范的网页搜索索引可核其对象；PDF 直开超时，本次未保存原文 bytes/hash | 非目标状态数据；无逐日可用时刻 | 排除：标的是组合证券而非 ETF 本身。 |
| 深交所停复牌公告/临停事件查询 | 159612、159952 的事件搜索候选；不具备完整逐日状态表语义 | Batch 2 有界请求超时；未得到 HTTP body、请求结果或原文 hash | 未知 | 查询失败不是“无停牌”，事件列表无覆盖声明不能证明全日正常。既有失败记录见 B2 A 交接。 |
| BaoStock `tradestatus` 历史日线字段（第三方） | 候选逐日值，skill 描述为 `1` 正常、`0` 停牌；五 ETF 全窗口实际响应未验证 | 当前 Python 环境未安装 SDK，未取得 API payload；无原始响应/hash，无五证券 × 窗口实测 | 无该端点的历史发布/可用时刻语义证据 | 不采纳为真实数据源。没有响应不能视为无状态。 |
| 513300 二级来源对 2026-09-18 09:30—10:30 盘中临停的转述 | 仅 513300 单事件；Batch 2 记录二级页面时间为 2026-09-17 19:13 +08:00 | 原交易所/基金公司公告字节和 hash 未取得；二级页面摘录无 hash；状态事实为 `unverified_primary_not_captured` | 二级来源声称的 `published_at=available_at=2026-09-17T19:13:00+08:00`，主来源时间仍未知；该时刻晚于 09-17 的 15:00 信号时点、早于 09-18 开盘 | 保留盘中事件，日频开盘模型无法表达，故 09-18 状态仍 `UNKNOWN`。不能把该事件扩展成全日 `HALTED` 或 `TRADABLE`。 |

**抓取时刻**：网页目录检查 `2026-09-26T17:35:44+08:00`；审计 JSON 快照生成 `2026-09-26T09:36:21Z`（即 17:36:21 +08）。这些都是本地观察/采集时刻，不是历史发布时间。公开目录列有不同的停牌数据类别，但没有宣称可用来从“没有事件”导出完整证券日历；上交所历史接口目录只证明接口文档存在，没有证明目标窗口状态文件已获得。已按有限范围停止扩展搜索，没有以采集时间补填历史可用时间。

| 证券 | 交易所 | 预期开市日 | 有证据全日状态 | 冲突 | 缺证 | 不可按日频表达/可知性 | 覆盖判定 |
|---|---|---:|---:|---:|---:|---:|---|
| 513300 | SSE | 241 | 0 | 0 | 240 | 1 盘中事件无法映射日频；原文未核 | `gaps` |
| 518880 | SSE | 241 | 0 | 0 | 241 | 0 | `gaps` |
| 510320 | SSE | 241 | 0 | 0 | 241 | 0 | `gaps` |
| 159612 | SZSE | 241 | 0 | 0 | 241 | 0 | `gaps` |
| 159952 | SZSE | 241 | 0 | 0 | 241 | 0 | `gaps` |
| **合计** | 两市 | **1,205** | **0** | **0** | **1,204** | **1** | **`gaps`** |

“缺证”指没有针对该证券/日期的合格来源事实，不表示该 ETF 实际停牌；盘中事件格有事件但不能转成日线全日状态。五只证券 1205 格最终 `trade_status=UNKNOWN`，`is_tradable=false`，完整性 `unverified`。状态矩阵另有 `evidence_state`：缺证 `missing`、冲突 `conflict`、时刻/历史范围不可证 `unverifiable`、来源语义不足 `insufficient_source_semantics`、日频无法表达 `not_representable`、已证明 `evidenced`；任何非已证明状态都不会被当作可交易。当前本批计数只有 1,204 `missing` 和 1 `not_representable`，没有冲突，也没有被证明的 TRADABLE/HALTED。

## 导入、冻结与快照

- Batch 3 契约现已由统筹落盘：`docs/contracts/batch3-contract.md`。B 提供 `import-status-evidence` 单写者接口；本批仅拿到接口说明和事件候选，没有可导入的真实逐日状态/覆盖文件。没有打开或写生产 DuckDB，也没有用 A/B 并发写生产库；没有调用导入入口或发布生产状态快照。因没有资格数据，未用全 UNKNOWN 人造填充去冒充状态来源。
- 冻结的 `status-snapshot-v1.json` 将五 ETF × 241 日完整表达为 fail-closed `UNKNOWN`，并携带来源事实列表、覆盖引用、生成身份；它是**阻断输入**，不是真实状态事实发布。文件 hash 与覆盖 hash 均见冻结 manifest。Snapshot 保留 513300 盘中转载事实及 `available_at` 的次级来源说法，原始文件 hash 仍为空。
- 没有合成/推断值写入 `TRADABLE` 或 `HALTED`。没有状态事实进入生产或隔离仓库，因此本批无 importer job、仓库快照 ID；只有研究侧审计快照身份与回测 overlay 身份。

## 复跑与独立账本核对

复用已发布 Universe 和价格快照，以及 baseline 保存的配置、241 日交易日历。三种身份应分开理解：

| 输入/运行 | 实验/快照 | 运行结果 | 说明 |
|---|---|---|---|
| Batch 1 旧真实价格实验 | experiment `4e076758-8069-4e0e-9269-1d3f9d574159`；price snapshot `fe1dd8a3-d227-4e06-bb98-f0a6b3be0f34` | 32 笔；佣金 2,991.470829，税 0，滑点 1,981.612800，总成本 4,973.083629；最终 NAV 1,179,713.316371；末持仓 518880: 65,000、159612: 285,600；0 拒单、0 延期 | `legacy_bar_only`，不是真实执行状态验证。 |
| Batch 2 UNKNOWN 占位复跑 | accepted experiment `709a4f67-6aca-427c-8064-8aac413d1764`；状态 overlay snapshot `80ff7b13-5a05-473f-8d02-df01c751bffb` | 0 笔；24 个 `unknown_status` 买入拒单；无延期/持仓；成本 0；最终 NAV 1,000,000 | 全 UNKNOWN 仅验证 gate fail closed，不是市场执行结果。 |
| Batch 3 有界取证后复跑 | experiment `55a6bd11-0c38-4bba-a91a-f16165022970`；overlay snapshot `b61fd7dd-332a-4cec-befb-9a1dad9d3bc9`，Parquet SHA `14e1cf13f0eadca7262d91f08b205d6959e3f8e243e761a01169b2c6c12750d8`；状态审计清单仍为 `gaps` | 0 笔；24 个 `unknown_status` 买入拒单；无延期/持仓；成本 0；最终 NAV 1,000,000 | 取证没有使 UNKNOWN 提升为真实状态，因此这仍是阻断/回归验证；不称为“有证据交易状态复跑”。 |

价格面板及 universe/config 固定，三跑价格输入无变化；可解释的唯一差异是 legacy 没有执行状态门槛，而后两跑以 UNKNOWN fail-closed。Legacy 到 UNKNOWN overlay 的 NAV 差为 -179,713.316371、总成本差 -4,973.083629，来自被阻止的执行轨迹，**不是停牌真实影响估计**。B2 与本批 UNKNOWN 运行结果一致；本批变化仅审计来源资料、原因分类和冻结身份。

独立核算新跑全部 241 个日点：起始现金按配置、无成交后现金保持 1,000,000、数量均为 0、持仓市值为 0、NAV 每日为 1,000,000，最大 `cash + Σ(quantity × close) - NAV` 误差为 0；24 个未执行项均为 BUY / `unknown_status`，无延期；佣金、税、滑点、总成本均为 0。Legacy 同面板独立核算最大 NAV 恒等式误差 `4.656612873077393e-10`，现金与非负持仓/费用组件核对通过。差异不能归因于价格变更。

## 修改与测试

A 本批修改：

- `scripts/audit_etf_status.py`：在原有 fail-closed 日状态基础上为每格增加 `evidence_state` 与覆盖分类计数，区分缺证、冲突、未知可用时刻/当前查询、来源语义不足及盘中状态无法日频表达。
- `tests/test_etf_status_audit.py`：增加并扩展缺失、来源声明不足、事件列表语义、当前状态查询、重复/冲突、迟到/未知发布时间、半开停牌/恢复及盘中模型缺口测试。
- `crates/quant-research/examples/etf_status_gated_replay.rs`：新增证据目录参数和审计 manifest 1205 格 gaps 前置校验；复跑标签改为 Batch 3 来源缺证，比较 JSON 输出使用 `batch3-a-comparison.json`。该工具不写仓库 DuckDB。
- 新增本交接及 `docs/handoffs/evidence/batch3-a/` 的来源审计 JSON、冻结状态快照、覆盖报告、预期开市日与哈希清单。

验证结果：

- `python3 -m unittest tests/test_etf_status_audit.py tests/test_execution_status_audit.py -v`：**21/21 passed**。
- `cargo run -p quant-research --example etf_status_gated_replay --locked --offline -- --output research-output --evidence-dir research-output/batch3-a-status-evidence`：**退出 0**；experiment `55a6bd11-0c38-4bba-a91a-f16165022970`，1205 UNKNOWN 行、24 `unknown_status` 拒单、0 成交，独立账本 NAV 最大误差 baseline `4.656612873077393e-10` / 新跑 `0`。
- `cargo test --workspace --locked --offline`：**通过**，warehouse 15、quant-research 56；已有真实快照 opt-in 测试 1 项 ignored（需本地被审计快照）。
- `cargo fmt --all -- --check`：**通过**，未执行全仓格式化。
- `cargo clippy --workspace --all-targets --locked --offline -- -D warnings`：**未通过，属并行 C 改动**。首次检查还报 `run_jobs.rs:153` 的 `clippy::possible_missing_else`；复查时该项已消失，当前剩 `crates/quant-research/src/server.rs:74` 将 `Arc<PathBuf>` 传给要求 `PathBuf` 的 `RunJobApi::start`。A 未修改这些并行文件。
- 冻结目录 hash 与矩阵检查：**通过**，manifest 引用的 source audit/calendar/status snapshot/coverage hash 全匹配；每证券 241 `UNKNOWN`，合计 1205。
- `git diff --check`：**通过**。

`docs/contracts/batch3-contract.md` 是统筹并行新增文件，不由 A 修改；`src/lib.rs` 与 `src/main.rs` 有 B 的并行修改，本批没有编辑这些文件。

## 集成需求与未解决问题

1. 保持 B3 合同的 B 单写者导入原则。将来来源合格后，导入行应使用真实 `raw_path/raw_sha256`、`published_at`、`available_at`、`verification_status`、来源覆盖/独立核验与事件半开边界；unknown发布时间留 null。A 不绕过 B 写库。
2. 将状态事实、逐证券×预期开市日覆盖声明、原文/文件 hash 和历史可用时刻共同绑定到冻结状态快照；现有 ETF 状态 reader 接线不能只存 `status/is_tradable/source/observed_at` 而丢弃审计身份。公共 schema/reader/runner/backtest 接线由统筹协调。
3. 上交所 LDDS 日文件取得方式/权限、目标 ETF 与逐日记录的完整性声明、每条历史文件实际发布或可用时间仍未知；深交所 ETF 自身逐日全日状态来源未建立。现有深市 TS03 是成份股停牌数据，不能替代。
4. 有全日停牌证据后仍须决定并验证日频次日 open 交易对盘中临停/恢复、仅限制买或卖、价格限制/排队/部分成交的映射；当前 `ExecutionStateInput` 仅日/证券维度，不能完整表达。
5. `batch3-contract.md` 的“可信作业资格侧车”要求原文/coverage/time 全可回溯；本批 gaps 清单显然不满足侧车门槛，不应生成可放行资格声明。

## STATUS / HANDOFF / ADR 建议

- STATUS 与 HANDOFF：记录状态来源证据仍为不完整、1205 格 UNKNOWN、Batch 3 同价 UNKNOWN 重跑只是阻断回归；P0-3 保持开放。
- 时间模型/ADR：无需改变现有事件顺序或 fail-closed ADR。后续如果将盘中临停映射为日频执行能力，须先形成明确事件序列和可手算 fixture；方向性限制不是现状态字段能力。
- P0-3 关闭条件不满足：缺两市覆盖五 ETF 窗口的官方逐日原文与 hash、来源完整性、各日历史可用时刻、可复核冲突/缺日结论、状态与证据共同冻结及真实状态同面板复跑。当前只有接口目录、事件分类/规范说明及一条未核验二级盘中转载，不能证明逐日执行状态。

**唯一建议下一步：** 由统筹协调 B/数据授权方取得 SSE 历史状态文件与 SZSE ETF 自身逐日状态原始资料及发布/可用时间证明，再按既定单写者接口导入冻结，并在当前 Universe 与价格面板复跑。来源取不到前，不继续扩展接口搜索或关闭 P0-3。
