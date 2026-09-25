# Batch 2 A — 五 ETF 历史执行状态证据交接

日期：2026-09-24。范围仅为 `513300`、`518880`、`159612`、`510320`、`159952`，固定回测窗口 `2025-09-23`—`2026-09-21`。以第一批已发布 Universe 版本及价格快照为准。本交付未改公共 schema、`data.rs`、`runner.rs`、`backtest.rs`、共享状态文档或契约。

## 结论

本次**没有取得足以证明五只 ETF 在 241 个预期开市日全日状态与历史可用时刻的完整原始数据**。因此冻结输入的 1205 个证券×交易日格均为 `UNKNOWN`；事件列表中的静默不推作 `TRADABLE`。输入只用于验证现有 reader/status gate 的 fail-closed 行为，不代表真实状态复跑，也不足以关闭 P0-3。

找到一条窗口内的盘中限制报告：`513300` 在 `2026-09-18` 开市后至 `10:30` 暂停交易。二级来源显示该消息在 `2026-09-17 19:13 +08:00` 已公开，早于次日开盘，但晚于前一日 `15:00` 信号决策时刻。原始上交所/基金公司公告及其字节未能取得；`published_at`/`available_at` 仅记录该二级来源网页的公开时刻，验证状态为 `unverified_primary_not_captured`。日频开盘成交无法表示 09:30—10:30 的限制，所以该日仍为 `UNKNOWN`，另保留时分事件。此事实足以证明真实停复牌细节会影响日频“次日 open 可执行”的假设，但它不是全量状态覆盖证据。

## 来源与覆盖矩阵

| 来源 | 范围/粒度 | 对五 ETF 的窗口覆盖 | 可用时刻与原文/hash | 审计结论 |
|---|---|---|---|---|
| [上交所基金停复牌查询](https://www.sse.com.cn/disclosure/dealinstruc/suspension/fund/) | 沪市基金事件列表；页面字段含基金代码、停牌起止日、时间、原因，可按代码和日期查询 | 适用于 `513300`、`518880`、`510320` 的事件查找；不是逐证券逐日全状态表。公开页面未给出“未列即全天正常”完整性语义 | 页面抓取时间 2026-09-24；底层列表响应未导出，原文路径/hash 为空 | 区分为事件列表；空列表不能证明正常交易 |
| [上交所 LDDS 主参考数据说明](https://bsp.sseinfo.com/admin/static/public/2024-06-07/163dba54e4334131b2d953ede8a8368c/%E4%B8%8A%E6%B5%B7%E8%AF%81%E5%88%B8%E4%BA%A4%E6%98%93%E6%89%80LDDS%E7%B3%BB%E7%BB%9F%E4%B8%BB%E5%8F%82%E8%80%83%E6%95%B0%E6%8D%AE%E6%8E%A5%E5%8F%A3%E8%AF%B4%E6%98%8E%E4%B9%A6%E6%8A%80%E6%9C%AF%E5%BC%80%E5%8F%91%E7%A8%BF.pdf) | 规范描述截至数据日 T 的沪市历史停牌文件，含全天、一小时、连续停牌及复牌边界状态码 | 是最有希望的逐日来源；没有取得窗口内的实际逐日文件/证券记录，也没有分发覆盖清单 | 规范页面/索引可查；原始 PDF 和逐日文件未保存，SHA-256 未知。逐事实历史发布时间/可用时间未知 | 仅列为候选全量状态源，不能据规范宣称数据已覆盖 |
| [深交所停复牌公告查询](https://www.szse.cn/disclosure/notice/temp/index_1.html) | 深市公告/事件列表，按公告日期和关键词查询 | 可用于 `159612`、`159952` 的事件搜索；目前未取得五 ETF 固定窗口完整逐日结果或静默完整性保证 | 网站抓取超时；原文响应、发布时间、hash 未取得 | 事件列表/失败接口，缺记录不能转为正常交易 |
| [深交所 ETF 技术数据交换规范 TS03](https://www.szse.cn/marketServices/technicalservice/column/history/P020180328468135161873.pdf) | 每日发送 ETF 跟踪指数**成份股**停牌信息，含 `T/N/F` | 不是 ETF 自身的二级市场执行状态；不用于 `159612`/`159952` | 官方规范可查；当日文件未公开取得。 | 排除，避免把成份股停牌误当 ETF 停牌 |
| [BaoStock 历史 K 线](https://www.baostock.com/) `tradestatus` | 候选全日状态字段：`1` 正常、`0` 停牌；skill 描述可查询 A 股/ETF 日线 | 可能支持 ETF，但本次未能验证五个 ETF 号段及全窗口端点行为 | 本机无 BaoStock SDK；外网 shell DNS 不可用；未获得响应、原文/hash，历史公开/可用时刻语义未证 | 候选接口失败/未验证；不能把无响应或空值当真实状态 |
| `513300` 2026-09-18 盘中停牌公告转述 | 单事件：09:30—10:30 临停，次级网页发表于 2026-09-17 19:13 +08:00 | 窗口内单日证据；不是全量状态覆盖 | [二级来源页面](https://finance.eastmoney.com/a/202609173877453845.html)，另有[雪球记录](https://xueqiu.com/S/SH513300)；原始 SSE/发行人公告未抓取，原始 hash 未知 | `UNKNOWN` + 盘中限制事件；不可证明当天全日停牌或全天可交易 |

技术说明依据的搜索索引还显示：上交所历史停牌文件意在覆盖沪市上市证券至数据日，并区分全天/一小时/连续停牌与复牌；但文件获取权限、保留期以及这五只基金逐日记录完整性均未验证。深交所 TS03 描述的是 ETF 组合证券停牌而非 ETF 自身状态。BaoStock 的状态字段虽适合作为候选，但本轮没有 SDK/网络响应；不能将其声明为已经实测可用。

完整来源材料目录：[`evidence/batch2-a/source-evidence.json`](evidence/batch2-a/source-evidence.json)、[`evidence/batch2-a/source-facts.json`](evidence/batch2-a/source-facts.json)。原始交易所响应/PDF/日文件未落盘，相关 `raw_path` 与 `raw_sha256` 保持 null；不得用本地采集时刻填历史发布时间。

## 冻结覆盖报告与身份

- 日期集合来自 Batch 1 固定行情 snapshot 同目录的哈希交易日历（manifest 的日历源标记为 `szse_official_month_list`），不是从 bar 的有无生成：241 个预期开市日 × 5 ETF = **1205** 个格。
- 状态适配器版本：`prajna-etf-status-evidence-v1`。所有格为 `UNKNOWN`（每只 241 unknown，0 TRADABLE、0 全天 HALTED、0 conflict）；来源完整性 `unverified`，覆盖状态 `gaps`。
- 审计 JSON 在被忽略的本机目录 `research-output/batch2-a-status-evidence/`：状态事实快照 SHA-256 `02a5fd68e60123b0fa80950b8dcc95e682de8a157242d8710b283a8d1bfe9c99`；覆盖报告 SHA-256 `83c14ff40905351e8d3cfe1287d0849fbd2c1de4af6f199f1ee49bb5781b7802`。生成时间 `2026-09-24T15:32:06Z`。原文响应哈希未知，与上述派生审计文件 hash 不同。
- 状态数据内的 `observed_at` 只记录本轮抓取/构建时间；不代表历史公告时间。513300 事件保存为 `intraday_restrictions=[09:30,10:30)`，其原始来源仍未核验。
- 价格面板锁定为 Batch 1 ETF snapshot `fe1dd8a3-d227-4e06-bb98-f0a6b3be0f34`，Parquet SHA-256 `8c8debb7e1fdd7b3b592cc0991e807b26f1e94a59fb7bbe7e66e00f3030af872`。Universe `d8811237-6c37-4189-86b8-9b05fbccc405` / version `3fcfed9c-b8c1-4cce-a933-bc2b79ebf0da`，content hash `91aa64f7872ced85b4513f224fa617a14fa12ae99c86f5936085a86466a4ba18`。

回测专用 unknown 占位 Parquet 快照：ID `92073b21-feea-4e26-885c-dd22d9b31374`，SHA-256 `29f00b61a6e9cec8bbe2f119a15a7de1ee5bfff3df8cf045cbd056d7d42b7fa5`。它只对固定五证券和窗口明确写入 UNKNOWN，其他日期/标的的状态列为空并 fail closed。此 Parquet 是**审计阻断输入**，不是从市场源获取的真实状态。新实验 `ea8f6e18-1684-41ac-a389-0f744cf450ae` 的比较文件记录其价格 snapshot 与 Universe 身份，并引用上述状态审计 manifest。

## 同一价格面板复跑差异

基线实验是 Batch 1 已发布 `legacy_bar_only` 实验 `4e076758-8069-4e0e-9269-1d3f9d574159`。新实验 `ea8f6e18-1684-41ac-a389-0f744cf450ae` 为 `status_gated`，本地路径 `research-output/experiments/ea8f6e18-1684-41ac-a389-0f744cf450ae/experiment.json`；比较 JSON 同目录 `batch2-a-comparison.json`。两次行情文件、策略参数、Universe 版本一致。

| 指标 | Legacy bar-only | UNKNOWN fail-closed | 差异解释 |
|---|---:|---:|---|
| 成交 | 32 | 0 | 新输入 1205 状态格全 UNKNOWN，状态门槛拒绝所有 24 个开仓腿 |
| 未执行买单 | 0 | 24（`unknown_status`） | 不是市场真实拒单统计，是完整状态缺失导致的保守拒绝 |
| 整笔延期 | 0 | 0 | 起始为空仓，没有旧仓待卖腿触发整体延期 |
| 期末持仓 | 518880: 65,000；159612: 285,600 | 空仓 | 未知状态下无成交 |
| 佣金 / 印花税 / 滑点 / 总成本 | 2,991.47 / 0 / 1,981.61 / 4,973.08 | 0 / 0 / 0 / 0 | 成本只随成功成交产生；新跑无成交 |
| 期末 NAV | 1,179,713.32 | 1,000,000.00 | 完全由 fail-closed 无交易解释，不能称为真实状态收益差 |

两跑都独立逐日重建现金、成交数量及收盘持仓市值，`cash + Σ(quantity × close) − NAV` 最大绝对误差 **4.66e-10**（status-gated 为 0）；逐日现金、持仓均非负且逐日恒等式核对通过。费用组件之和与总成本一致。未用新快照的价格数据，因而本组差异没有行情面变化混杂。

实际成交窗口内的 513300 临停时段不应简单映射成全天 `HALTED`：执行引擎只用日线 open，无法在同一日回到 10:30 后成交；也没有方向特定限价、盘中部分成交、排队或成交量约束。即便取得交易所全量停复牌表，仍需决定并测试临时停牌的日频保守映射。

## 修改文件与集成需求

- 新增 [`scripts/audit_etf_status.py`](../../../scripts/audit_etf_status.py)：固定五标的/窗口的证据事实标准化、1205 格覆盖、来源重复/冲突、半开有效区间、可用时刻（开盘 09:30 Asia/Shanghai）验证与版本化快照/hash。缺状态、时刻缺失或迟于执行开盘均 UNKNOWN。只接受显式 full-day 事实生成 TRADABLE/HALTED；intraday 事件单独保存。
- 新增 [`tests/test_etf_status_audit.py`](../../../tests/test_etf_status_audit.py)：覆盖缺失/unknown、停复牌半开边界、盘中限制、迟到与未知 available_at、来源冲突、重复记录、当前状态查询误用、完整覆盖需要来源认证，以及不合法事件 scope。
- 新增 [`crates/quant-research/examples/etf_status_gated_replay.rs`](../../../crates/quant-research/examples/etf_status_gated_replay.rs)：通过通用 ETF snapshot reader 锁定同一行情输入和已发布版本；构建明确标注的 UNKNOWN Parquet，复用 runner/status loader/backtest；输出差异摘要并做逐日账本独立复核。只读既有 snapshot，不写生产 DuckDB。
- 新增 `docs/handoffs/evidence/batch2-a/source-evidence.json`、`source-facts.json`：来源目录和这条未核验盘中事件的机器可读事实。

请由集成人员协调 B 的单写者接线，不能由 A 直接向生产库导入。本批未新增公共 schema。接入前需保证 B 的导入器接收/保留冻结契约所列 `source_ref`、原文路径/hash、`published_at`、`available_at`、`observed_at`、`verification_status`、`coverage_ref` 及 UNKNOWN/冲突原因；现有 `core.security_status_revision` 与状态 snapshot reader 只保留状态、来源、`observed_at` 和覆盖布尔值，缺少上述证据字段。共享 `data.rs`/snapshot manifest 和 reader 扩展由集成负责人统一接线。

## 验证结果

- `python3 -m unittest tests/test_etf_status_audit.py -v`：**10 tests passed**。
- `python3 scripts/audit_etf_status.py --calendar research-output/batch2-a-status-evidence/expected-trading-dates.json --facts docs/handoffs/evidence/batch2-a/source-facts.json --output research-output/batch2-a-status-evidence`：完成；241 日期、1205 格，coverage `gaps`；五只证券各 241 UNKNOWN；状态快照与覆盖报告 hash 如上。
- `cargo run -p quant-research --example etf_status_gated_replay --locked --offline -- --output research-output`：完成，锁定价格/Universe 身份、status mode、1205 格 UNKNOWN 门槛、0 成交、24 个 unknown_status 拒单、费用组件和两跑独立逐日账户核算断言通过。最大 NAV 误差 `4.66e-10`。
- `cargo test --workspace --locked --offline`：**10 warehouse + 55 quant-research tests passed**；真实快照 opt-in 用例仍按仓库默认 ignored（1）。
- `cargo fmt --all -- --check`：失败，只有新增的并行文件 `crates/quant-research/src/market_api.rs` 有 formatter diff；A 的 Rust example 已单文件 rustfmt。未格式化并行文件。
- `cargo clippy --workspace --all-targets --locked --offline -- -D warnings`：失败于并行新增 `market_api.rs` 的三个 `Result<_, Response>` 函数 `bounded_limit`、`resolve_snapshot`、`read_snapshot`（`clippy::result_large_err`）；A 的 example 已通过 `cargo check` 与真实回放编译执行。未修改并行模块。
- shell 抓取上交所/深交所失败（DNS 解析失败或 web 页抓取超时），本机 Python 无 `baostock` 包；这就是原文缺失的具体原因，不据此认定来源“无数据”。
- workspace Rust 测试已通过；formatter 与 Clippy 需由 C/集成方修复 `market_api.rs` 后重跑全局检查。本交接明确保留这两个并行文件问题，不将其归因到 A 的新增文件。

## STATUS / HANDOFF / ADR 建议

- STATUS：P0-3 继续开放。真实来源证据为不完整，唯一窗口内事件仍只有二级转载；unknown 占位跑仅证明 gate 关闭缺证输入。
- HANDOFF：记录 A 的候选源调查、原始证据获取失败原因、状态审计/实验 UUID 和单写者集成字段要求；状态模式 `status_gated` 不等同已验证状态来源。
- ADR：暂不修改已接受的日频状态门槛决策。建议另起待决提案讨论盘中临时停牌映射、按买卖方向的价格限制，以及开盘交易可用时间的状态门槛语义；在决定并有事实 fixture 前不要声称覆盖。

**P0-3 关闭条件未满足。** 尚缺五只 ETF 对 241 日的来源覆盖证明、交易所原文及逐文件 hash、历史公开/可用时间、深交所 ETF 自身全日状态源、冲突裁决证据，并且现有公共状态快照没有保存这些审计字段。唯一建议下一步：让 B/集成方取得或建立两市官方逐日状态文件的单写者归档与覆盖证明，再把可验证窗口状态灌入通用快照，沿本交接的固定价格面板与 Universe 版本复跑。
