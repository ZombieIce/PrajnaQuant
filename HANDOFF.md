# Agent Handoff

审计日期：2026-09-23。项目的 `main` 分支关联 [ZombieIce/PrajnaQuant](https://github.com/ZombieIce/PrajnaQuant)，保留远端初始提交与 LICENSE；本地 `data-core/` 和 `research-output/` 被忽略。正式状态见 `docs/STATUS.md`，有序问题清单与验收判据见 `docs/priorities.md`，本文件只记录下一 Agent 接手时最需要的信息。

## Current Objective

优先推进 ETF Rotation MVP 的**可验证正确性**，在现有架构上建立可信的小型回测验算。先解决 `docs/STATUS.md` 中有证据的 P0 问题的边界与验收，不把当前样本结果当无偏业绩。

## Current Project Phase

本地日频 Rust 研究/回测和 React 只读展示原型。A 股 Rust 仓库可持久化行情修订；Python 仅早期入库原型。

## Work Completed This Session

完整扫描源码、SQL、配置、旧文档、React、测试、Git 状态、明显 TODO/FIXME/XXX/deprecated/temporary/hack；建立 README、AGENTS 与架构/时间/数据/因子/策略/回测/API/STATUS 文档及两项现状 ADR。修正了旧文档中对当前前端的过时表述。未变更回测逻辑或数据 schema。

随后把本地项目接入现有 GitHub `main` 历史，并补充前端依赖、构建产物的 Git 忽略规则。仓库中的原始数据和既有实验结果未纳入版本控制。

## Current Implementation State

Rust `src/` 是仓库，`crates/quant-research/` 是因子研究/ETF 回测/网格/API，`apps/web` 是实际托管界面。因子与回测数值以 Rust 为权威。无 Python 研究包、PyO3、正式 Factor Registry 或通用策略配置层。

## Factor State

`signal.rs` 固定 10 个定义，可单因子 Rank IC/分组多 horizon 评价；轮动综合评分在 `strategy.rs`，未注册。`factor.rs` 统计 Mean Rank IC、Std、ICIR、正率和分组/多空未来收益。无 Pearson IC、分布统计、因子自相关/换手/相关矩阵。React 的重叠收益累计图不能作策略 NAV。

## Strategy State

`configs/etf_rotation_grid.json` 可跑短/长动量减波动率、趋势过滤、Top-N、等权、周期调仓的参数扫描。`etf_momentum*.json` 走单动量分支。universe 由仓库当前 ETF 分类决定；无历史在市名单或上市/退市日期。

## Backtest State

`backtest.rs` 按 T close 决策、下一组行情日 open 模拟成交，收盘算权益。支持双向佣金/最低佣金、税和滑点，spread 未单独模拟。结果有成交、成本、权益、回撤、部分绩效；每日仓位价值/权重及基准派生指标缺失。

## API State

`server.rs` 六个只读 GET 路由，详见 `docs/api.md`；无触发回测、参数过滤、分页/降采样。详情返回整个实验 JSON。

## Frontend State

React 策略目录/详情、因子目录/详情及 ECharts 已存在；图可 tooltip 和缩放。当前策略详情只有净值和回撤，缺基准、超额、交易、成本、仓位等。`dashboard.html`/`factor_dashboard.html` 未被服务端引用。旧文档把前者功能误写为当前可用功能。

## Tests

仓库/Rust 因子与策略有基础单元测试，Python 原型有五项测试；缺小型端到端组合核算、历史池、分红与展示口径验算。本次验证结果与命令见下方，不能以已有大 JSON 的存在代替验收。

## Known Issues

`docs/STATUS.md` 已列分级：当前分类筛选全历史的幸存者偏差；ETF 原始价缺总回报；React 重叠未来收益复利图；`signal.rs` 波动率首窗口虚构零收益。策略目录将轮动实验也标为单动量规则。未找到明确未来标签进入选股的证据。

此外，本机五个已保存 Top-5 实验的最大正仓数为 6–7：缺执行 bar 的旧仓未卖、新目标仍可买，实际持仓可超过 Top-N。该观察值来自旧结果 JSON，后续需固定答案测试复现。

## Correctness Risks

交易状态视图未用于回测；缺当日 bar 时沿用旧 close 估值并跳过成交；基准与权益未对齐；没有逐日会计不变量测试。`observed_at` 不能替代历史可知时刻。

## Important Decisions

`docs/decisions/0001-computation-ownership.md` 和 `0002-current-event-time.md` 记录现行计算权威与时序。新接口/因子/账户模型尚无获准设计，不在本轮决策。

## Do Not Change Without Review

T 收盘→下一开盘事件顺序、原始价/交易价与收益口径、仓库版本修订与快照来源优先级、现金与费用恒等式、API/实验 JSON 契约。更改时需要可验算场景和文档/ADR 同步。

## Recommended Next Step

**唯一最优先任务：建立 3 ETF / 10 交易日的端到端 Rust 金标准回测测试**，用手工计算覆盖 T close→下一 open、排名/Top-N、至少一次卖买、手数/费用/滑点、每日 cash/position value/NAV、缺 bar、末日 pending 和基准日期；让正确性风险先有稳定的验收尺度。随后再处理历史池、分红和前端口径问题。

这项是执行前置任务。按结果影响排序，历史 ETF universe 和分红/总回报居前；两者都需要外部数据证据。详见 `docs/priorities.md`，不要把测试先做误读为历史池问题严重度较低。

## Verification Commands

```bash
git status --short
cargo fmt --all -- --check
cargo test --workspace --locked --offline
cargo clippy --workspace --all-targets --locked --offline -- -D warnings
.venv/bin/python -m unittest discover -s tests -v
cd apps/web && npm run build
```

本轮已确认 `cargo fmt --all -- --check`、`cargo test --workspace --locked --offline`（Rust 仓库 6 项、研究 9 项）、`cargo clippy --workspace --all-targets --locked --offline -- -D warnings`（冷编译约 14 分钟）均通过；`.venv/bin/python -m unittest discover -s tests -v` 五项通过，`cd apps/web && npm run build` 通过（1.27 MB JS chunk 警告）。以本地服务对 6 个实际 GET 路由及 `/` 做 HTTP 烟测，均返回 200；`GET /api/experiments/{id}` 的一次压缩前 HTTP 响应为 1.91 MB。对本机五个旧实验 JSON 的诊断检查确认末日权益/总收益、逐笔成本汇总与最终持仓非负一致，但无法验证每日会计恒等式，因为报告没有每日证券级持仓价值。首次 Python DuckDB 检查因 `.venv` 缺 `pytz` 而失败；时间戳转字符串后简单主表统计可读，对 `research.etf_daily_bar` 全历史分源计数触发 20.6 GiB 临时空间上限，未完成实时覆盖核验。已有 manifest 记载约 143.6 万 ETF 行/1674 个当前证券/2605 个基准行，仅是本机旧产物记录，不能证明当前全量数据正确。旧文档的历史验证不能替代本轮实测。
