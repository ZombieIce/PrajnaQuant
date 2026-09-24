# Agent Handoff

审计日期：2026-09-23。项目的 `main` 分支关联 [ZombieIce/PrajnaQuant](https://github.com/ZombieIce/PrajnaQuant)，保留远端初始提交与 LICENSE；本地 `data-core/` 和 `research-output/` 被忽略。正式状态见 `docs/STATUS.md`，有序问题清单与验收判据见 `docs/priorities.md`，本文件只记录下一 Agent 接手时最需要的信息。

## Current Objective

产品初级目标已由用户明确为可本地运行、可部署到服务器远程使用的 A 股股票/ETF 日频研究回测平台，含 Rust 后端、Python 因子研究、每日自动同步，以及网页因子/组合/Universe/K 线展示。分阶段验收见 [`docs/product-roadmap.md`](docs/product-roadmap.md)；这些是目标，不应读成现有能力。当前 ETF Rotation 仍是首个验证切片。

优先推进 ETF Rotation MVP 的**可验证正确性**，在现有架构上建立可信的小型回测验算。先解决 `docs/STATUS.md` 中有证据的 P0 问题的边界与验收，不把当前样本结果当无偏业绩。

## Current Project Phase

本地日频 Rust 研究/回测与 React 报告原型。A 股 Rust 仓库可持久化行情修订；本地 Universe CRUD API、管理页面及已保存结果筛选已接通；Python 仅早期入库原型。

当前未形成通用股票回测、Python 因子研究层、每日自动日更、股票/ETF K 线 API/页面或服务器远程访问闭环。后续 Agent 必须将这些写作待交付目标，并在每项完成后用运行证据更新 `docs/STATUS.md`。

## Work Completed This Session

新增 `backtest.rs` 端到端手算回归场景：合成 3 ETF × 10 日行情通过 `run_momentum` 的真实轮动评分、Top-1 选择和回测事件循环；固定预期排名 D3–D10 为 C、B、B、C、A、A、A、C，成交发生在下一行情日开盘。测试核对买卖、100 股手数、滑点、比例/最低佣金、现金、正持仓、收盘估值和每日 NAV，并确认 D10 信号不成交。

另加 C 在 D5 缺 bar 的场景，确认旧仓当日不能卖、估值沿用 D4 close，且报告没有跳过原因/陈旧估值字段。没有改引擎、结果结构、事件顺序、账户口径或 benchmark 契约；无 ADR 变化。`docs/STATUS.md` 同步记录覆盖与缺口。现有工作区开始时干净。

## Current Implementation State

Rust `src/` 是仓库，`crates/quant-research/` 是因子研究/ETF 回测/网格/API，`apps/web` 是实际托管界面。因子与回测数值以 Rust 为权威。无 Python 研究包、PyO3、正式 Factor Registry 或通用策略配置层。

## Factor State

`signal.rs` 固定 10 个定义，可单因子 Pearson/Rank IC、分组多 horizon 评价；轮动综合评分在 `strategy.rs`，未注册。报告记录标签方法、日历来源、有效截面/缺失和 coverage，并提供分数自相关与 Top 分位成员更换率。缺截面或无有效相关时统计为 null。React 因子详情展示逐期前瞻标签，不复利成 NAV；组合换手、分布统计和因子相关矩阵仍缺。详情见 `docs/factor-system.md` 与 ADR 0004。

## Strategy State

`configs/etf_rotation_grid.json` 可跑短/长动量减波动率、趋势过滤、Top-N、等权、周期调仓的参数扫描。`etf_momentum*.json` 走单动量分支。universe 由仓库当前 ETF 分类决定；无历史在市名单或上市/退市日期。

## Backtest State

`backtest.rs` 按 T close 决策、下一组行情日 open 模拟成交，收盘算权益。支持双向佣金/最低佣金、税和滑点，spread 未单独模拟。结果有成交、成本、权益、回撤、部分绩效；每日仓位价值/权重及基准派生指标缺失。

## API State

`server.rs` 保留原结果 GET，并提供 Universe CRUD/成员/覆盖/能力与实验/因子结果筛选路由，详见 `docs/api.md`。策略摘要带 Universe 身份、PIT/覆盖和旧结果 unknown 状态。尚无运行提交/队列、完整分页/降采样；详情返回整个实验 JSON。覆盖查询按被冻结版本的 coverage segments 判断，手工 Universe 为 `retrospective_static/unverified`，无 provider 的 index history 为 `unknown/gaps`。

## Frontend State

React 策略目录/详情、因子目录/详情、Universe 管理与 ECharts 已接实际 API；已保存结果按 universe/version 精确筛选，旧报告显示 Universe 未知；草稿、成员、发布、日期快照、覆盖能力和归档均读写服务端。新运行继续禁用并解释 `/api/v1/runs` 不存在。当前策略详情缺基准、超额、交易、成本、仓位等。`dashboard.html`/`factor_dashboard.html` 未被服务端引用。

## Tests

新金标准固定手算：起始现金 100,000；D4 买 C 900，D5 卖 C/买 B，D7 卖 B/买 C，D8 卖 C/买 A；最终现金 8,670、A 持有 900、D10 收盘权益 101,370。每日权益序列为 100,000、100,000、100,000、99,810、101,230、102,130、101,750、99,570、100,470、101,370。4 买/3 卖共 7 笔，佣金 700、税 0、滑点 630、报告总成本 1,330。测试辅助账本由成交重建现金/整数数量，以每只证券最近可用收盘价逐日验证 `NAV = cash + Σ(quantity × mark)`，并断言现金非负及数量非负。

缺 bar 测试 D5 断言 C 的卖出被跳过、无 D5 成交、现金维持 9,810、C 按 D4 的 100 估值，NAV 为 99,810；之后市场恢复时策略才可以在后续调仓日卖出。测试覆盖的是实际缺价行为，不代表目标偏离问题已修复。benchmark 仅断言传入点被原样保留，没有基准派生收益测试。

## Known Issues

`docs/STATUS.md` 已列分级：当前分类筛选全历史的幸存者偏差；ETF 原始价缺总回报；React 重叠未来收益复利图；`signal.rs` 波动率首窗口虚构零收益。策略目录将轮动实验也标为单动量规则。未找到明确未来标签进入选股的证据。

P0 Top-N 目标与实际持仓偏离仍未修复：本机五个旧 Top-5 实验的最大正仓数为 6–7；新测试确认了缺 bar 跳过卖出/沿用旧价，但这个低现金固定场景没有复现超过 Top-N。交易失败/未执行原因和目标重试语义仍未输出。

## Correctness Risks

交易状态视图未用于回测；缺当日 bar 时沿用旧 close 估值并跳过成交，没有陈旧估值标记；基准与权益未对齐。新逐日恒等式只证明此合成场景的账本行为，不证明真实市场数据、停牌、历史 universe、生存偏差或分红处理正确。`observed_at` 不能替代历史可知时刻。

## Important Decisions

`docs/decisions/0001-computation-ownership.md` 和 `0002-current-event-time.md` 记录现行计算权威与时序。新接口/因子/账户模型尚无获准设计，不在本轮决策。

## Do Not Change Without Review

T 收盘→下一开盘事件顺序、原始价/交易价与收益口径、仓库版本修订与快照来源优先级、现金与费用恒等式、API/实验 JSON 契约。更改时需要可验算场景和文档/ADR 同步。

## Recommended Next Step

**当前唯一建议下一步：先为缺执行 bar 后实际持仓超过 Top-N 的情景补独立手算失败用例，冻结未成交目标/待执行交易语义，再修复该 P0。**

既有金标准覆盖普通换仓和缺单 bar 行为，但没有复现超过 Top-N。历史 ETF universe 与分红/总回报仍是开放的 P0，五 ETF 名单暂按用户要求作为回溯静态集合；沪深300历史成分只用合成数据验证接口，不把模拟事实当真实 PIT 证据。详见 `docs/priorities.md`。

## Verification Commands

```bash
git status --short
cargo fmt --all -- --check
cargo test --workspace --locked --offline
cargo clippy --workspace --all-targets --locked --offline -- -D warnings
.venv/bin/python -m unittest discover -s tests -v
cd apps/web && npm run build
```

本轮实际通过：

- `cargo fmt --all -- --check`
- `cargo test --workspace --locked --offline`（仓库 6 项、研究 11 项，共 17 项通过）
- `cargo clippy --workspace --all-targets --locked --offline -- -D warnings`

本轮未改 Python；前端有改动并已完成生产构建。Rust 命令有现存 `~/.cargo/config` deprecated 警告，不影响结果。


## User-Selected Five-ETF Universe MVP (2026-09-23)

User temporarily deferred CSI 300 validation and specified `513300`, `518880`, `159612`, `510320`, and `159952`. Existing immutable snapshot `research-output/snapshots/cb4697c7-6727-4e0b-83e9-3bf31db05371/etf_daily.parquet` already contains every selected instrument throughout 2025-09-23—2026-09-21 (241 unique dates each); no fetch was needed. OHLC sanity checks found zero invalid rows. The full snapshot is 2016-01-04—2026-09-21; 510320 begins 2025-04-25.

Repro command: `cargo run --offline -p quant-research --example etf_universe_mvp`. Result: `research-output/experiments/1dfe2ade-072b-4ea5-94c8-c61e55ca5ef5/experiment.json`; one-year manual static ETF set, no HS300 benchmark input, 32 trades, 17.97% total return on this raw-price smoke run, total recorded costs 4,973.08, zero independently reconstructed NAV mismatch. Do not present these numbers as investable performance: the bars are unadjusted and dividends/splits/total-return treatment remain unverified. The result freezes universe/version/content and snapshot hashes, but the temporary example does not register the definition with `UniverseStore`.

The real-data run exposed and fixed a Universe runner boundary bug: pre-start bars needed for warmup had been sent to the backtest as trade dates. The runner now computes with warmup bars but reports membership, factors and executions only inside configured start/end; regression assertions verify the first equity date and no earlier trades.

Verified this round: `cargo fmt --all -- --check`; `cargo test --workspace --locked --offline` (9 warehouse + 25 research); `cargo clippy --workspace --all-targets --locked --offline -- -D warnings`. The DuckDB writer was already held by `target/release/ashare-warehouse`; no write or interrupt was attempted. The test used the last published immutable snapshot and did not wait for or alter the ongoing market-history backfill.

## Universe Management UX Review (2026-09-24)

The management page now defaults to business-facing information and ordinal versions; IDs, hashes, instrument IDs, and membership hashes are hidden from the normal surface. Stable identifiers remain under “技术信息” for support/debugging. The page explains that archiving preserves config, versions, and historical experiment references while blocking edits/new releases; only an unpublished empty draft is physically deleted. The full create → edit → inspect → publish → select-for-run → filter-saved-results workflow and current run-API gap are documented in `docs/universe-contract.md` §8. `cd apps/web && npm run build` passed; Vite retains the existing >500 kB chunk warning.

## Web Route / Chart Loading and Strategy Curve UX (2026-09-24)

React pages now load from route-specific dynamic imports; ECharts is modularized through `echarts/core` with only the line/bar charts and components currently used. Build output: entry 232.59 KB / gzip 73.20 KB, shared chart chunk 535.01 KB / gzip 178.19 KB, page chunks 1.9–14.4 KB. The chart chunk loads only for factor/strategy details; Vite still warns because that individual chunk exceeds 500 KB.

Strategy detail stacks NAV and drawdown vertically under one shared start/end date selector. Both plots use the same filtered dates. NAV resets to 1.0 at the first valid equity point in the chosen range; drawdown high-water mark is recalculated within that range. The metric cards continue to show the full backtest period. Browser verification on a saved local experiment checked both canvases and changed the interval to 2026-01-05—2026-06-30. `cd apps/web && npm run build` passed; no Rust code changed in this UI round.


## Universe Feature Delivery — UI/API Integration

本轮由整合 Agent 冻结 `docs/universe-contract.md`，并新增 `docs/decisions/0003-universe-identity-and-pit.md`。保留了开始前已有的 backtest 金标准和 STATUS/HANDOFF 修改。首轮实施包括：

- 数据侧新增 schema v5/v6 可回放迁移和通用指数成员导入；事实保存加入/退出公告时点、生效区间、原始附件/来源 hash，通用导入不会把成员或完整覆盖自行升级为 PIT。
- Rust 新增版本化 Universe 模型与事件式 PIT resolver；manual 成员明确 `retrospective_static`；定义 hash、逐日 membership hash、版本隔离键与股票运行门槛。旧 experiment JSON 缺 universe 字段仍可读并标为 `legacy_universe_unknown`。
- `runner::run_experiment_with_universe` 对不可变 ETF Universe version 逐交易日解析，在完整行情面板上只掩码日评分与因子横截面；结果冻结 identity、每日 membership hash、来源 hash、coverage/PIT 和 snapshot hash。传统入口遇到 Universe 选择但未传 Definition 时 fail closed。
- Backtest 新入口接收明确的已验证空成员日；仅真实解析为空且有证据时产生空目标，缺评分/未验证空集合不推断清仓。清仓仍在下一有行情开盘尝试，现有缺 bar 保留仓位/旧价行为不变。
- API 提供本地 Universe CRUD/发布/成员/覆盖/能力与实验/因子结果筛选；配置由输出目录 JSON 单写者原子替换。写服务拒绝非 loopback。无异步队列，所以 `/api/v1/runs` 未开放。
- Web 页面连接实际 CRUD、发布、成员/覆盖与能力查询 API，以及 experiment/factor saved report 筛选 API。没有默认 mock Universe 或假报告；旧结果身份缺失显示未知。运行按钮独立禁用并解释没有 `/api/v1/runs`/作业状态存储。

### Verification Performed

- `cargo fmt --all -- --check`：通过。
- `cargo test --workspace --locked --offline`：通过，仓库 9 项、研究库 25 项。
- `cargo clippy --workspace --all-targets --locked --offline -- -D warnings`：通过。
- `cd apps/web && npm run build`：通过；ECharts bundle 约 1.29 MB，Vite 提示单 chunk 超 500 KB。
- `.venv/bin/python -m unittest discover -s tests -v`：未通过，Python 环境缺 `duckdb` 模块；本轮未改 Python。
- 经用户允许 loopback 端口后，以 `/private/tmp/prajna-universe-http-smoke` 为隔离输出目录启动 `127.0.0.1:18787`，在浏览器完成 Universe 创建、草稿更新、合成成员写入、不可变版本发布、指定日期成员及覆盖/能力查询；界面正确显示 `retrospective_static`、`unverified`、覆盖缺口和运行能力阻断。策略与因子页面均能加载，因子目录通过真实 API 按 Universe + version 精确筛出临时合成报告。临时服务与数据已清理。
- 联动中发现缺少 `report.reports[0]` 会令因子目录抛错白屏；`apps/web/src/main.tsx` 已增加报告结构保护和错误边界，生产构建通过且刷新后的目录正常。ECharts 产物仍约 1.29 MB，Vite 报单 chunk 超 500 KB。

### Correctness / Open Limits

已在本机数据库暂存第三方 `index-constitution` CSI 300 历史区间 CSV（1221 条定界区间）；opt-in/out 发布时间人为设为生效日前 14 个自然日并明确标记为模拟。4 条缺失 opt-in 的源行登记为覆盖缺口，覆盖仍 `unverified`、成员仍 `unknown`；原文及 SHA-256 在 `data-core/raw/index-constitution/`。这不是官方公告 PIT 数据，API provider 尚未连接，不能把价格基准变成成分。股票行情、交易单位、费用/税、可交易状态、公司行动仍未验，股票策略被阻断。服务端无异步运行；Web 的 Universe 页面和结果筛选已接 API。测试是合成 fixture，不构成真实历史回测业绩证明。成员查询截止为上海时间 15:00（UTC 07:00）；当前时序和缺行情旧价估值规则见 `docs/time-model.md`。

### Universe Handoff — Current State

前端 CRUD 和已保存结果筛选已对接；`server.rs` 的 test-only synthetic 3 securities × 10 trading days handler 场景在 `tempdir` 中验证 D4 三成员、D10 空池、完整覆盖及股票运行阻断。另经用户允许 loopback 端口，在临时输出目录完成浏览器到 API 的 Universe 草稿/发布/成员/覆盖/能力流程，以及因子目录按精确 Universe/version 筛选合成报告；临时数据已经清理。合成 `verified_pit` 只证明固定 fixture 的代码行为。服务端真实 index-history config 创建仍没有 provider 写入成员事件；真实沪深300成分及股票 ETF Rotation 继续阻断。

## A-Share Daily History Backfill (2026-09-23)

Added `backfill-market-history` to fetch Tencent raw daily bars for the locally observed SH/SZ equity candidates, equities, and ETFs from 2016 onward. It stores each raw response and hash, records resumable per-window states, and reports completion/ETA every 25 windows. A direct Tencent probe for 2016 returned valid bars; a full 2016–2026 test for `sh510010` imported 2,588 rows across 6 successful windows. The official TDX direct TLS connection failed during this run.

The full local-pool import is still running: 6,882 symbols, about 34,410 800-calendar-day windows; the latest handoff snapshot was 1,175 windows (850 success, 325 confirmed empty, 0 failed), with roughly 7h54m estimated remaining. The observed pool can omit delisted securities never seen locally; Tencent does not cover BJ here and does not return turnover amount. Do not mark this scope as PIT or verified complete. Resume with the command in `docs/A股本地数据库建设方案.md`; completed windows are skipped. A first run with 700-day boundaries completed about 150 requests before interruption; those ranges may be fetched again under the 800-day boundaries, but row hashes avoid duplicate revisions.

Added `audit-market-history`, a reproducible length check for five default samples. On 2026-09-24, five samples across SH/SZ stocks and ETFs each had all five expected 800-day windows recorded, no failed windows, and source row-count sums equal to stored distinct Tencent dates: sh600000 2590, sh600519 2608, sh510050 2608, sz000528 2587, sz159919 2607. Each ranged from 2016-01-04 through 2026-09-24. This validates sample source-to-storage length consistency, not an independent official-calendar gap audit or full-market completeness. The audit was corrected to match exact target-window boundaries so earlier end-date variants are not double-counted.

Code compiled via `cargo run --release --offline`; the one-symbol end-to-end data import and sample audit passed. `git diff --check` and file-level `rustfmt --check` for `src/main.rs`, `src/lib.rs`, and `src/sources.rs` passed. Workspace `cargo fmt --all -- --check` is blocked by the pre-existing uncommitted non-ASCII byte-string syntax error at `crates/quant-research/src/server.rs:953`. A full debug workspace test run was stopped during the first build of DuckDB's C++ dependency; workspace tests and Clippy remain unverified for this change. Next step: cross-check sample date sets against the official exchange calendar, then audit all symbols after backfill completes.

## Official Trading Calendar Acquisition and Sample Audit (2026-09-24)

Fetched the SZSE monthly calendar directly for 2016-01 through 2026-09 and persisted accepted days to `core.trading_calendar_revision`, with each accepted raw response, URL, and SHA-256 under `data-core/raw/szse/`. Historical API timeouts were retried. Two months remain unresolved: 2017-01 returned only 30/31 natural dates (its raw response and validation manifest are saved under `data-core/raw/szse-incomplete/2017-01/`); 2019-05 timed out on repeated direct attempts. The exact unresolved-month list is saved in `data-core/raw/szse-incomplete/fetch-failures.json`.

Added `audit-trading-calendar` and a fixture test. The 2016-01-01—2026-09-24 range has 3,920 natural dates; official calendar coverage is 3,858 dates with 2,570 confirmed open days, leaving 62 unknown dates in those two months. Within covered dates, Tencent bars miss 18 open days for `sh600000`, 21 for `sz000528`, and 1 for `sz159919`; `sh600519` and `sh510050` have no covered-date gaps. Each sample has 38 observed bars inside the two unresolved months, which cannot be certified against this calendar. No bars fell on known closed dates. Missing rows are not classified as suspensions versus source omissions; this is a five-symbol sample, not full-market validation.

Verification: `cargo fmt --all -- --check` and `cargo test -p ashare-warehouse --locked --offline` passed (10 warehouse tests, including the new audit fixture). The live database audit ran with `cargo run --release --offline -- --data-dir data-core audit-trading-calendar`. Remaining issue: obtain the 2019-05 calendar from an official alternative or source notice, and fill the one omitted 2017-01-01 date with separately attributed official evidence before claiming the range is calendar-complete; then rerun the sample audit and extend it to the market pool after history backfill completes.

## Universe Instrument Lookup and Publish Semantics (2026-09-24)

Universe member entry now queries `GET /api/v1/instruments` for local ETF snapshot code-prefix/name matches. Selecting a suggestion fills name, exchange and asset type. The repository's latest available instrument snapshot is ETF-only, so stock lookups return no matches with an explicit note; users may still type stock names manually. No stock name is guessed from a code pattern. Added a DuckDB-backed Parquet lookup test for code-prefix and name matching.

Publishing freezes that version's definition and membership. It does not permanently lock the Universe: the draft remains editable and a subsequent publish creates another immutable version. This is documented in `docs/universe-contract.md` and `docs/api.md`.

Verification this turn: `cd apps/web && npm run build` passed (Vite reports the existing ~1.29 MB JS chunk warning); `git diff --check` and final `cargo fmt --all -- --check` passed. `cargo test -p quant-research --locked --offline` passed (26 tests); `cargo clippy -p quant-research --all-targets --locked --offline -- -D warnings` passed. The initial formatter check identified formatting in the new lookup code, so `cargo fmt --all` was applied before final verification. Workspace-wide Python tests were not run because this change does not touch Python.

## Universe Management UX Follow-up (2026-09-24)

Moved “保存并发布新版本” from the bottom member editor into a sticky toolbar near the top of the draft view. Publishing now saves current draft name/description first, then publishes; the toolbar shows progress, success with the version number, or failure beside the action. PIT/data coverage and run capability are compact expandable explanations, and the date-specific member snapshot/table is collapsed until requested. `docs/universe-contract.md` and `docs/STATUS.md` record this behavior.

Verification: `cd apps/web && npm run build` passed; `git diff --check` passed. Quantitative API/engine semantics were not changed in this UX pass.

## Universe Publish Diff Guard (2026-09-24)

Member tables present securities as `code · name` without exchange prefixes or internal IDs. Publishing first saves metadata, requests a server-side version preview, and shows changed fields for confirmation. An unchanged draft reports that no new version was created. The server compares canonical content hashes under its write lock, no-ops duplicate publishes, and rejects a publish if the draft hash changed after confirmation. Preview hash, no-op publish, and stale-confirmation behavior are covered in the Universe store test.

Publish preview verification for this update: `cargo test -p quant-research --locked --offline` passed all 26 tests, including API preview/change response and no-op `created=false`; the store test also verifies stale preview hashes are rejected and unchanged content reuses the prior version. `cargo clippy -p quant-research --all-targets --locked --offline -- -D warnings`, `cargo fmt --all -- --check`, `git diff --check`, and `cd apps/web && npm run build` passed. Vite retains the existing >500 KB chart chunk warning.

## Universe Remaining Work Priority (2026-09-24)

Explicit order added to `docs/priorities.md` and the Universe page: **P0** establish trustworthy member point-in-time facts and date-level market-data coverage (provider sources, publication/effective times, gaps, missing bars); **P1** complete asset-specific factor/strategy capability checks and the run API/queue, gated on P0 and execution validation. Both remain incomplete; current capability output reports blockers and is not a claim that runs are available. The page marks both tiers and explains their purpose. `apps/web` production build and `git diff --check` passed after this presentation update.

## ETF Universe Market Coverage Audit (2026-09-24)

接通了现有 ETF Parquet 快照，不再把行情覆盖固定返回 `unverified`：coverage API 会选取创建时间最新且文件 SHA-256 与 `manifest.json` 一致的不可变快照，按版本成员和指定日期范围返回每证券行数、首末日期、缺失日期、重复日期和 OHLC 异常，并返回快照 ID/hash。响应区分成员覆盖与行情覆盖；手工成员仍为 `retrospective_static`，不会因行情完整而升级成历史 PIT。

目前覆盖对照日期使用所选成员观察日期的并集，可定位成员间缺口；没有独立交易日历，无法识别所有成员同日都缺行情的市场级缺口。无任何观察行返回 `unverified`，不推断完整。页面在 P0 展开区展示逐成员审计表、快照日期范围、缺口与校验异常。

仓库已有五 ETF 快照审计记录显示：`513300/518880/159612/510320/159952` 在 `2025-09-23` 至 `2026-09-21` 各有 241 个交易日、OHLC 约束异常为 0；本轮新增 handler 测试在合成 Parquet 上验证单证券缺日、空快照 `unverified`、manifest hash 校验及 API 响应。五 ETF 名单仍是手工回溯静态集合；原始价不证明分红/拆分总回报。

本轮验证：`cargo fmt --all -- --check`、`cargo test -p quant-research --locked --offline`（29 项通过）、`cargo clippy -p quant-research --all-targets --locked --offline -- -D warnings`、`cd apps/web && npm run build` 和 `git diff --check` 通过。前端构建保留既有 ECharts 535 KB chunk 提示。没有改写当前其他任务在 `backtest.rs`、数据仓库 `src/` 或实验产物上的工作。

本轮把深交所官方日历旁车接入研究快照及 Universe 覆盖审计。最新快照 `fe1dd8a3-d227-4e06-bb98-f0a6b3be0f34` 的 ETF 文件 SHA-256 为 `8c8debb7e1fdd7b3b592cc0991e807b26f1e94a59fb7bbe7e66e00f3030af872`，共 1,441,506 行、1,674 个证券，范围 2016-01-04—2026-09-24；日历旁车 SHA-256 为 `0701455f5d65e317974bf61b223a0cbdb36250079d1dca7ca4e3ba01b17bcfee`，395 行、2025-09-01—2026-09-30，来源为 SZSE 官方月历。只导出完整自然月，不用工作日推断补日。

真实数据审计 `sh513300/sh518880/sz159612/sh510320/sz159952`，2025-09-23—2026-09-21：241 个官方开市日；每只 ETF 均有 241 行，缺失日、非开市日、重复日、无效 OHLC 均为 0；总体 `complete`，日历覆盖 `complete`。快照内日历可识别全体成员共同缺行情；旧快照缺旁车时仍为 `unverified`。已加入合成测试验证共同缺日可检出及快照仅收录完整自然月。覆盖 API/合同/API 文档同步更新。

实际运行命令：`cargo run --offline -p quant-research -- snapshot --warehouse data-core/market.duckdb --output research-output` 成功生成上述快照；`cargo fmt --all -- --check` 与 `git diff --check` 通过。`cargo test --workspace --locked --offline` 通过（ashare-warehouse 9 项、quant-research 35 项；合计 44 项）；`cargo clippy --workspace --all-targets --locked --offline -- -D warnings` 通过；`cargo fmt --all -- --check`、`git diff --check` 通过；`cd apps/web && npm run build` 通过，Vite 提示 Chart bundle 约 535 KB。为解决本工作区既有未完成 Universe/因子代码与 API 合编冲突，曾修复 `experiment.rs` 的可选 IC 字段对齐、`runner.rs` 的测试标签/PIT 可见性、`signal.rs` 的 observation wrapper、`main.rs` CLI 新字段；这些与既有工作区改动同属未提交状态，不能从基线区分。

边界：日历来自深交所，虽用于五只沪深 ETF 同步开市日核对，但本轮没有与上交所官方日历独立交叉校验。行情覆盖完成不等于 ETF 上市/终止 PIT、历史 Universe PIT、公司行动总回报、股票执行能力或可发布历史业绩完成。

## Factor Evaluation Corrections (2026-09-24)

按 Qlib 因子评价参考建议调整本地因子研究口径：评价默认改为 T 收盘打分、下一市场日开盘入场、H 个市场日区间后开盘退出；显式保留 close-to-close 诊断选项。快照指定交易日历优先，缺日历时退化为观察 ETF bar 日期并集并记录 `calendar_basis`。缺证券 bar 时标签缺失，不插值。评价结果新增 Pearson IC、评价状态、期望截面/因子/有效标签及缺失数、coverage、同证券分数自相关和 Top 分位成员更换率。有效截面少于 `max(quantiles,3)` 时 IC 与分组收益留空，不返回伪零值。React 因子详情改为逐期标签图，移除对重叠未来标签的复利展示。波动率首窗改为只使用完整的已观测日收益窗口。

新增固定答案测试：手算 x=1..5、y=x² 的 Rank IC=1、Pearson IC=`60/sqrt(3740)`；固定 expected=8、有效标签=5 的 coverage=5/8；样本不足时指标为 null；开盘标签日历对齐和缺 bar 留空；两期 ±10% 样本收益的首个两日波动率为 `sqrt(0.02)`。既有 ETF Rotation 3×10 手算账本回归仍通过。标签和 JSON null 语义见 `docs/decisions/0004-factor-evaluation-contract.md`、`docs/factor-system.md`、`docs/time-model.md` 与 `docs/api.md`。

验证通过：`cargo test -p quant-research --locked --offline`（35 项）；`cargo fmt --all -- --check`；`cargo test --workspace --locked --offline`（9 仓库 + 35 研究）；`cargo clippy --workspace --all-targets --locked --offline -- -D warnings`；`cd apps/web && npm run build`；`git diff --check`。前端产物保留既有 ECharts 535.01 KB chunk 警告。

仍未证明或解决：全部三个 P0（历史 ETF PIT 名单、公司行动/总回报、缺 bar 后目标与实际持仓偏离）继续开放；旧快照或无日历输入时，bar 日期并集不能发现全体证券共同缺行情；`close_to_close` 是证券有效 bar 诊断 horizon；报告的 Top 分位更换率不等于成交换手；无基准对齐收益、超额收益或 IR 指标；未在真实市场数据上验证新标签统计和实际因子表现。旧因子报告无独立因子口径版本，历史结果的算法区分仍需后续处理。

**唯一建议下一步：**先为缺 bar 后仍超过 Top-N 的情景补独立手算失败用例，并据此冻结目标持仓/待执行交易语义，再修复 P0-3；在该规则通过回归前不把 Top-N 解释为持仓上限。

## Product Scope Clarification (2026-09-24)

用户明确了初级产品目标：本地运行、可部署到服务器远程使用的 A 股股票/ETF 日频研究回测平台；Rust 管数据/回测/API，Python 做因子研究，网页展示因子评价、组合绩效、选股池和证券 K 线，交易日后自动同步日频数据。已将目标与实际实现分开记录在 `docs/product-roadmap.md`，并同步 `AGENTS.md`、`README.md`、`docs/STATUS.md` 和本交接首页。当前代码仍仅支持 ETF 研究回测切片与手动数据回填；没有因这次目标澄清而修改量化代码、启动新数据回填或声称已实现自动日更/远程服务。

本轮仅文档修改；`git diff --check` 通过，未运行编译或测试。后续按当时的唯一建议任务推进，完成时按 `AGENTS.md` 更新状态与交接。

## P0-3 Deferred Rebalance Guard (2026-09-24)

按最新交接建议补了一个 3 ETF 四个交易日的独立手算场景。A 以 60 买入 100 股、余现金 4,000；次日 A 缺开盘价，旧实现继续买 B 并把现金用到 0，手算断言先红。修复后缺少任一非目标旧仓卖出价会延期整次调仓，D3 不成交，保留 A×100 和现金 4,000；同日收盘新 C 信号替换旧 B 目标；D4 卖 A×100 得 6,000，再按 100 股手数买 C×200 花 8,000，余现金 2,000。每日 NAV 均为 10,000，正持仓始终不超过 Top-1，佣金/税/滑点均为 0。延期结果记录决策日、尝试日、目标、阻塞标的及原因。

策略详情现展示期末正持仓数量/Top-N 和延期记录；旧结果缺此字段时明确显示“无法确认没有延期”。`BacktestReport.rebalance_deferrals` 以 serde 默认值兼容旧文件，不回写历史 JSON。待执行目标在之后有可执行行情时重试；期间新的**已到调仓周期**信号替换旧目标。ADR 0005 记录该规则；时间模型、回测/API/状态/优先级文档已同步。

验证通过：`cargo test -p quant-research --locked --offline`（36 项）；`cargo fmt --all -- --check`；`cargo test --workspace --locked --offline`（ashare-warehouse 9 项、quant-research 36 项）；`cargo clippy --workspace --all-targets --locked --offline -- -D warnings`；`cd apps/web && npm run build`；`git diff --check`。Vite 保留 Chart bundle 535.01 KB 的既存体积警告。仍未完整关闭 P0-3：交易所停牌/执行状态没有接入；目标买入 bar 单独缺失时该买腿仍被跳过且不单独记因；旧结果不重算。

**唯一建议下一步：**接入并验证 `research.daily_bar_execution` 到回测执行门槛，同时给缺目标买入 bar 增加逐腿跳过原因，再评估是否可关闭 P0-3。历史 ETF PIT 与公司行动总回报仍是独立 P0。

## ETF MVP 默认可投资、零分红场景测试（2026-09-24）

依用户指定，默认将五 ETF 视为仅在实际审计数据区间内可投资，所有分红/派息设为 0。`crates/quant-research/examples/etf_universe_mvp.rs` 已改用带日历旁车的快照 `fe1dd8a3-d227-4e06-bb98-f0a6b3be0f34`；运行时重新审计深交所官方交易日历与每只 ETF，要求 241 个开市日、每只 241 根有效 bar、无缺失/非开市日/重复/无效 OHLC 才继续。成员有效区间从每只实际审计的首根行情到末根行情（有效区间按右开结束日编码），此处五只均为 2025-09-23 至 2026-09-21。窗口前的 60 日行情仍用于因子预热，但不进入成员集合。Universe source_ref/description 和 experiment assumptions 均明确标注默认可投资、非 PIT、零分红、原始价格收益。

执行 `cargo run --offline -p quant-research --example etf_universe_mvp` 成功，生成 `research-output/experiments/42785f2f-06e8-42d6-a077-fe9db29ecc01/experiment.json`。市场覆盖与日历状态均 `complete`；241 净值点、32 笔成交、总成本 4,973.08、价格收益 17.97%；现金＋持仓市值对 NAV 的最大逐日误差为 0。不要将该价格收益称为 ETF 总回报或历史可投资回测；`pit_status=retrospective_static`。使用无分红假设可能忽略分红造成的价格除息影响。

本轮最终验证：`cargo test --workspace --locked --offline`（9 warehouse、36 research，合计 45 项通过）；`cargo clippy --workspace --all-targets --locked --offline -- -D warnings`、`cargo fmt --all -- --check`、`git diff --check` 通过；上述 MVP 示例自身的独立行情/日历审计断言通过。

唯一建议下一步：将此明确标注假设的五 ETF Universe 正式保存在 `UniverseStore` 并用已发布版本 ID 复跑，使报告能从管理页筛选追溯；保留 `retrospective_static` 与“零分红价格收益”标签。

## Strategy Position Utilization and Instrument P&L (2026-09-24)

策略回测报告现新增逐日 `position_curve`，保存现金、持仓证券数量/收盘价/市值和资金占用率；页面在净值与回撤下方展示资金占用率和各标的持股数量曲线，统一使用绩效区间选择器。`instrument_performance` 按证券汇总买入/卖出数量、成交数、期末数量/市值及已实现/浮动/总盈亏。标的盈亏按移动加权平均成本归因：买入费用计入成本、卖出费用抵减收入，滑点已体现在 fill，期末浮盈按最近收盘价标记。JSON 契约/口径记录于 `docs/api.md`、`docs/backtest-engine.md` 和 ADR 0006。旧实验仍可读取，缺少新字段时前端显示需重跑，不推断旧持仓。

独立 3 证券×10 交易日账本新增校验：逐日现金+持仓市值等于权益、资金占用率符合定义、各证券的已实现/浮动损益与成本及最终数量一致，损益合计等于组合期末权益变化。验证通过：`cargo fmt --all -- --check`；`cargo test --workspace --locked --offline`（9 warehouse、36 research）；`cargo clippy --workspace --all-targets --locked --offline -- -D warnings`；`git diff --check`；`cd apps/web && npm run build`。Build 有既存 ECharts 535 KB chunk warning。没有对真实行情宣称完成验证；本机 `127.0.0.1:7878` 当前没有服务监听，故本轮未做浏览器联动验收。

唯一建议下一步：在同一独立手算账本上补充报告 JSON 的 API round-trip 回归，验证新增字段经实验落盘和详情 API 后仍与报告结构一致。

## Synthetic Execution Status View Check (2026-09-24)

为验证交易状态数据如何进入研究层，在仓库单元测试中构造同一证券多日行情及 `TRADABLE`、`HALTED`、`UNKNOWN` 三种状态 CSV，并留一天行情没有对应状态。固定预期为 `TRADABLE → is_tradable=true`，其余三种情形（停牌、未知、状态缺失）均为 `false`。它验证了状态 CSV 导入及 `research.daily_bar_execution` 的日期连接/保守缺失行为；未验证回测执行会拦截订单，因为 ETF 快照仍只导出普通行情字段，`Bar`/回测入口没有状态字段。

本轮验证：`cargo test -p ashare-warehouse --locked --offline calendar_factors_and_status_are_versioned_for_research_queries`、`cargo fmt --all -- --check`、`git diff --check`。测试结果和格式检查通过；未运行全 workspace Clippy，因为本轮未改变 Rust 生产代码。

已知问题及风险：`HALTED`/`UNKNOWN` 目前仅在仓库研究视图可见，策略仍可能因有行情 bar 就执行；未知状态拒绝语义尚未覆盖回测分支；状态来源的历史完整性及可知时点也未由合成数据证明。P0-1 历史 ETF PIT、P0-2 公司行动/总回报未解决，P0-3 目标与实际持仓差异审计仍未关闭。

**唯一建议下一步：**把状态随 ETF 快照传入回测，增加人工可算的停牌/未知状态买卖用例，并逐腿记录缺 bar/不可交易原因；再据这些结果决定 P0-3 是否可关闭。

## ETF Execution Status Gate (2026-09-24)

已把每日交易状态与来源列表嵌入新 ETF 快照。多状态源必须全部为 `TRADABLE` 才执行；状态缺失、`UNKNOWN`、`HALTED` 或冲突均 fail-closed。需要卖出的旧仓缺 open/不可交易时整次调仓延期；目标买腿不可执行时跳过。`BacktestReport.unexecuted_orders` 保存尝试日、决策日、方向、目标数量、状态、来源和原因。runner 与 batch 从快照加载一次状态映射后传给回测。旧 Parquet 缺状态列时保持 bar-only 兼容，并在 ADR 0007 记录此结果边界。

独立手算 3 ETF×10 日用例：D4 C 的 UNKNOWN 买单被拒；D5 买 B×900，现金 9,810；D7 B 的 HALTED 卖单被拒，整次 C 调仓延期，最新信号改为 A；D8 卖 B×900、买 A×900，现金 9,430。每日收盘权益固定为 100,000、100,000、100,000、100,000、101,610、102,510、100,710、100,330、101,230、102,130。3 笔成交佣金 300、税 0、滑点 270、总成本 570。另有状态快照 SQL 测试覆盖 `TRADABLE`、多源冲突、缺状态 fail-closed 和来源保存；runner 测试验证 Parquet→真实评分→回测的停牌拒单和恢复后成交；旧快照/旧报告兼容也有固定测试。

验证通过：`cargo test --workspace --locked --offline`（ashare-warehouse 10 项、quant-research 40 项，合计 50 项）；`cargo fmt --all -- --check`；`cargo clippy --workspace --all-targets --locked --offline -- -D warnings`；`git diff --check`。前端未改，因此未运行前端构建。

仍未证明/未解决：真实状态源的历史完整性、历史可知时点、停牌业务规则真值和 side-specific 涨跌停成交；旧快照继续按 bar-only 成交，需要决定重跑/升级要求。UI 还未展示未执行订单。P0-1 历史 ETF PIT、P0-2 公司行动/总回报仍未解决；P0-3 虽补了交易状态门槛和订单原因，目标 underfill、旧快照风险和真实源证据仍未通过验收，继续标为未解决。无真实行情或证券规则结论可从这些合成值外推。

**唯一建议下一步：**挑选有官方出处的 ETF 状态历史样本，核对状态覆盖、来源优先级和有效时点，并据此决定旧快照应 fail-closed 还是强制重跑；在有证据前不关闭 P0-3。
