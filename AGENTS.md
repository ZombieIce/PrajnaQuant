# Agent 工作约定

## Project Scope and Authority

项目主路线是 Rust-first 多市场研究、并行回测与未来实盘平台。目标见 [ARCHITECTURE.md](ARCHITECTURE.md)，阶段见 [product-roadmap](docs/product-roadmap.md)，词汇见 [CONTEXT.md](CONTEXT.md)，当前能力、验证范围和限制以 [STATUS](docs/STATUS.md) 为准。POC-0～MVP-6 已分阶段交付；MVP-6 的[有保留签收](https://github.com/ZombieIce/PrajnaQuant/issues/182#issuecomment-6406038887)、六项工程 pass、证据处理流程 fail 与第一轮 fail 保留。

Web 非验收项仅指 MVP-0～6；MVP-7 按 [#286](https://github.com/ZombieIce/PrajnaQuant/issues/286) 的产品闭环及[研究优先裁决](https://github.com/ZombieIce/PrajnaQuant/issues/286#issuecomment-6435307888)推进。阶段目标不表示现有能力已验收。

Rust 是数据、因子、执行、账户、成本和绩效权威，避免新增第二套量化核心。新平台 `crates/prajna-{domain,data,research,account,adapter,experiment,py}` 分担契约、存储、研究、账本、Backend、实验和绑定；旧 `src/` 是 DuckDB 仓库，`crates/quant-research/src/` 保留 ETF 研究/回测及 Axum API，两条路径的身份、报告和验证范围分别说明。已确认边界写入 `docs/decisions/`，未决方案只写提案。

`python/prajna` 通过 PyO3 声明实验、读取 Arrow、生成受限 As-of Decision；回调不读取账户/成交，实际执行和绩效由 Rust 负责。逐 bar 回调推迟，Notebook/依赖闭包限制及构建见 [prajna-py README](crates/prajna-py/README.md)。`warehouse.py` 是独立早期入库原型。`apps/web/` 由旧服务托管，展示、排序和交互应与后端语义一致，浏览器不得成为因子/绩效权威；旧 HTML 未被当前 `server.rs` 引用。

证据优先级：代码与可运行测试 > 当前配置/schema > 最新 ADR > STATUS > HANDOFF > README > 旧文档/注释。不要为了文档设想改写正确代码。区分配置要求、代码实现、已有测试与实际数据验证；无证据标 Unknown/Unresolved。

## Quant Correctness and Evidence

- 因子说明输入字段、窗口、可观测/可用时刻、方向、缺失规则、版本/来源；未来收益仅作评价标签，`forward_return` 不进入评分。
- 当前 A 股/ETF 策略用 T 日收盘含当日 close 形成信号，下一可用日 open 执行。检查 rolling 边界、shift、缺失填充、全样本归一化、基准日期与历史 Universe 可知性；基本面按公开时间而非报告期末可用。
- 新平台时间语义按当前 spec/ADR；修改或复用 A 股/ETF 执行时序前读 [time-model](docs/time-model.md)，先给事件序列和可手算小例。`observed_at` 是本地采集时间，不是历史发布时间。
- 现金、数量、成交价、佣金、税、滑点可追溯，成本配置随结果保存。逐日核对 `NAV ≈ cash + Σ(quantity × 当日估值价) + receivables`；旧无应收路径为零，应收计权益但不可花费。长仓数量/权重非负，解释余量现金和总权重；停牌/缺行情不视为零价或可成交。
- 历史 ETF 成分、上市/退市、价格分红调整缺 PIT 证明时，结果与文档披露幸存者及收益口径风险。合成覆盖、真实已触发事件、真实未触发路径分别报告；编译通过不代替量化验证。
- 真实行情、原文、请求日志、因子/标签、账本/NAV/绩效、详细覆盖、hash 和 Run/Execution 身份仅留本机，不进入公开仓库、Issue/PR 或聊天/工具输出。公开只用代码、合成 fixture 与允许的文字结论；历史有限保留不扩大、不再次传播，旧验收档案不改写。具体任务的证据范围按当前 Issue/spec 裁决。

## Startup and Reading

新会话先读本文件，运行 `git status`、`git log -5 --oneline`，保留用户未提交文件。按任务范围读当前 GitHub Issue/评论、spec、相关代码和测试；读取方式与当前状态刷新见 [issue-tracker](docs/agents/issue-tracker.md)。已固定 head/spec 的资料按需复用，输入或 head 变化时刷新相关部分，不每轮全量重复输出。

- **新平台：**读 `ARCHITECTURE.md`、相关 ADR；按需读路线图、CONTEXT、STATUS。交接读 Issue/PR 与 `HANDOFF.md` 入口，逐票历史按需查 [archive](docs/handoffs/poc-0-mvp-0-handoff-archive.md)。构建资源任务另读 [benchmark spec](docs/poc-0-benchmark-spec.md) 与票据 13。
- **旧 A 股实现或复用其契约：**按修改范围读 README 与 `docs/` 下 `architecture.md`、`time-model.md`、`data-model.md`、`factor-system.md`、`strategy-system.md`、`backtest-engine.md`、`STATUS.md`、`priorities.md` 及相关 ADR；不通读无关历史。

## Ticket Workflow

一票一 Issue、一条从最新 `main` 拉出的 `<issue>-<slug>` 分支、一个含 `Closes #<issue>` 的 PR。只认领无 blocker 且 `ready-for-agent` 的票；口径未定先 `needs-triage`，等负责人定稿。只提交/推送自己的分支，不直推 main、不 force push 共享分支、不自行合并。一个工作区同一时间只承载一个分支，并行任务使用独立 worktree。

拆票、Prepare 与范围复核按 [票据范围规则](docs/agents/ticket-scope.md)：先判独立契约，约 800 changed lines 触发复核；近期票先做范围检查，Prepare 只做六项增量复核，超规模单票须在开工前明确授权预算。同一行为的薄页面/API/持久化切片可一票交付。不得改写验收迎合实现或修改其他票状态/结论，调整在对应 Issue 提议。

负责人可直接启动 [implement-ticket](.agents/skills/implement-ticket/SKILL.md) / [review-pr](.agents/skills/review-pr/SKILL.md)，或授权 [ticket-loop](.agents/skills/ticket-loop/SKILL.md) 协调启动。实现者完成验收映射、定向验证和 `Self-check (implementer)`，不充当独立 reviewer、不自行启动 reviewer；每次独立 review/re-review 使用未参与实现且不继承实现上下文的新会话。普通票可由一名 reviewer 检查 Spec 与 Standards；高风险展开双轴并行，风险判定见 review-pr。显式 [code-review](.agents/skills/code-review/SKILL.md) 保留，不是普通票强制自查。票据不使用 implement-spec 的一 spec 一 PR 流程。

Review 结论以 PR 评论为准；每个发现由实现者回应，任何新 push（含文档）均需独立 review 覆盖最终 head。必需验证未完成不称可合并；负责人合并。授权、worktree 保留/清理与 tracker 操作的共同契约见 [issue-tracker](docs/agents/issue-tracker.md)。

## Verification and Completion

按改动范围执行 [定向验证规则](docs/agents/verification.md)：纯文档做 diff/链接/一致性检查；代码验证受影响入口，按需选 feature，不因例行交接编译全 workspace。该文档承接 Python 3.12/PyO3 环境、CI 来源及合成金标准要求；具体 CI 命令以 `.github/workflows/` 为准。

交接在 PR 描述记录验收映射、命令结果、失败/环境限制、风险判断、验证范围、开放问题和唯一建议下一步；review 结论留 PR 评论。`HANDOFF.md` 仅作入口，不随 PR 修改。`docs/STATUS.md` 仅在能力、风险、量化口径或验证状态实际改变时在对应主题改一条，不追加逐票流水，纯文档/流程 PR 不改。接口、数据 schema 或量化口径变更时同步文档与必要 ADR；重要决策/契约变化同步相关 API/README。尽量在 review 前完成，避免后续 push 使审查过期。

项目 tracker 为 GitHub `ZombieIce/PrajnaQuant`，`.scratch/` 只读；五种 triage label 见 [triage-labels](docs/agents/triage-labels.md)。领域文档采用根 `CONTEXT.md` 与 `docs/decisions/`，见 [domain](docs/agents/domain.md)。
