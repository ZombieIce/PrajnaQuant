# Agent Handoff — 新平台目标基线与 POC-0

## 当前交接（2026-09-28，B2 票据 08 归因）

固定 S2 64×252 首差已按 [ADR 0014](docs/decisions/0014-b2-correctness-failure-attribution.md) 归因为 Nautilus Adapter 逐证券开盘 Quote 的先买后卖问题，而非 Fast Event 错误。[诊断票据](.scratch/poc-0-b2-matched-remeasure/issues/08-s2-64x252-parity-diagnosis.md)给出裁剪自固定输入的 10 ETF / 68 session 手算用例：2025-04-07 close 信号、04-08 08:50 可用的 TRADABLE 状态、09:30 先卖五只再买五只，现金 48350、NAV 99560；Rust 与修正后 Adapter 测试通过。04-07/08/09 ETF038 均有 bar，无阻断状态。原 S2 351/348（Rust-only 133、Nautilus-only 130）在修复后变为两边 351 订单/Fill 且所有共同子集字段通过。旧报告保持归档，不改写。

[完整重测](poc/poc0-benchmark/results/b2-robustness-64x252-parity-diagnosis.json)按原协议先做两次正确性对拍，再进行 20 次串行和五组 2-worker × 6 Run；S2 中位 Rust/Nautilus 19,105,375 / 260,615,500.5 ns、并行 61.89 / 6.70 Runs/s、RSS 90,685,440 / 302,743,552 bytes 上界，S3 也达到数值门槛。**2026-09-28 B2 票据 #2 reset parity 已完成并选择 fallback：**Nautilus 2.0.0rc5 下，S2/S3 判定 fixture 与两套 64×252 fixture 均完成至少三次同 worker reset Run；projection checksum、现金、订单/持仓和 pending 计数、迭代/回放边界、strategy/instrument/quote cache 状态均与新建引擎一致，判定负载 checksum 也与此前通过独立 Rust golden 字段对拍的投影 checksum 一致。但每次 reset Run 都打印 Nautilus 原生错误 `Invalid state trigger READY -> INITIALIZE`，因此报告把 `engine_log.error` 作为首个差异并统一选择 `cached_conversion_new_engine`，不忽略该错误。版本、revision、输入 hash、checksum 列表和 state snapshots 见 [reset parity evidence](poc/poc0-benchmark/results/b2-nautilus-reset-parity-2026-09-28.json)。将模式证据传给 coordinator 后，S2/S3 decision loads 都完成 registered `adopt`，报告与原始样本见 [registered decision-load report](poc/poc0-benchmark/results/b2-matched-decision-loads-reset-selected-2026-09-28.json)。B2 overall 仍 `unresolved`，因为 64×252 robustness 尚未用所选模式证据 registered 复核。ADR 0012 原生停牌订单生命周期仍排除。2026-09-28 起票据在 GitHub Issues 跟踪，`.scratch/` 为只读归档。

**验证与复核：**票据 #2 的 reset 单元测试和 `2.0.0rc5` 四负载实际复跑见上方 evidence；reset 模式因 native error log 未选用，统一 fallback 已选定。decision-load coordinator 已消费该证据，S2/S3 均是 registered `adopt`。当前唯一建议下一步：由票据 #3 将同一证据接入 64×252 robustness 复核并形成 B2 正式结论；在此之前整体 B2 仍 `unresolved`。

## 票据 14 交接（2026-09-28）

固定 Nautilus 2.0.0rc5 / Python 3.12.2 的[独立 HALT 隔离实验](poc/poc0-benchmark/results/nautilus-halt-native-probe-2026-09-28.md)表明：单 instrument QuoteTick 可观测后 HALT 生效，两组在相同后续时刻直接提交市价单，原生 `on_order_rejected` 报 `Market PROBE.SIM is CLOSED`，引擎缓存状态 `REJECTED`；去掉 HALT 的对照在同一 QuoteTick 上成交，状态 `FILLED`。[ADR 0012](docs/decisions/0012-poc0-nautilus-status-gate.md)已将原生能力 Unknown 更新为已证实，B2 已登记比较范围不变。MVP-4 可考虑让 Nautilus 原生处理停牌；实际集成时仍须验证状态源可用时刻与订单生命周期，再决定是否移除项目层门槛。目前 Adapter 项目门槛未改，旧 A 股/ETF 时序不变；B2 64×252 S2 对拍差异仍按下方当前交接处理。

票据 14 验证：隔离脚本复跑通过（无 HALT `FILLED` / HALT `REJECTED`），模拟版本不匹配返回 `Unknown`；POC Python 31/31、`cargo fmt --all -- --check` 通过。`cargo test --workspace --locked --offline` 中 16 + 65 项单元测试及 14/15 个 B2 CLI 用例通过，已有 B1 sweep CLI 因缺文件失败，1 项 ignored；`cargo clippy --workspace --all-targets --locked --offline -- -D warnings` 退出 0，既有 vendor `polars-io` 告警仍存在。根 Python 21 项通过、`test_warehouse` 因环境缺 `duckdb` 导入失败（本票不新增依赖）。独立 Standards/Spec review 的同时间对照、异常结果归类与重复交接建议均已修正；复核确认 Spec 无剩余发现，Standards 提出的票据 05 历史交接措辞属于既有内容，未改。开放问题：MVP-4 实际 Adapter 的状态源时间语义与原生生命周期尚未集成验证。

## 历史交接（2026-09-28，B2 票据 04 修正前稳健性）

POC-0 B2 64×252 robustness 报告：[报告](poc/poc0-benchmark/results/b2-robustness-64x252-2026-09-28.json)、[release build record](poc/poc0-benchmark/results/b2-robustness-release-build-final-2026-09-28.json)。Rust S2/S3 Dataset Content SHA 与 B3 注册值一致，Rust account checksum 也分别通过。S2 两次独立 Nautilus common-subset parity 得到相同投影 checksum 和相同六个非排除失败字段（Rust 351 个订单/Fill，Nautilus 348）；紧凑首差样例显示 ETF038 下单/成交从 Rust 2025-04-08 错到 Nautilus 2025-04-09，并记录日账本差异。HALT 日期/证券在双方均无项目订单或 Fill；ADR 0012 的 native lifecycle 仍单独排除。S2 性能采集按正确性门跳过。S3 parity 通过并完成测量，数值门槛通过；中位串行延迟 Rust/Nautilus 2,455,667 / 53,476,979.5 ns、五组 2-worker Runs/s 535.84 / 34.21、RSS 90,996,736 / 234,848,256 bytes（两 worker 峰值和上界）。当前 B2 robustness 和 combined mapping 在归档报告中为 `reject`；该负载没有独立金标准，差异无法归因到哪一方，按 [ADR 0014](docs/decisions/0014-b2-correctness-failure-attribution.md) 改为 `unresolved`（`attribution_required`），POC-0 票据 09 同步恢复为 `unresolved`。

**验证：**`cargo fmt --all -- --check`、`cargo clippy --workspace --all-targets --locked --offline -- -D warnings`、两项 64×252 B2 Rust CLI checksum/投影用例、POC B2 Python tests（24/24）通过。`cargo test --workspace --locked --offline` 中新增用例通过；一个既存 B1 sweep CLI 用例因缺少文件失败。根 Python suite 有 21 项通过，`test_warehouse` 因缺少 `duckdb` 导入失败。最终 release 在 10 GiB 闸门保护下 warm build 12.172 秒、target +4,776 bytes。Nautilus 2.0.0rc5。独立复核确认 repeatability 与差异诊断 P2 已修复，新增诊断/HALT 边界测试后复核中。**唯一建议下一步：**执行 B2 remeasure [票据 08](.scratch/poc-0-b2-matched-remeasure/issues/08-s2-64x252-parity-diagnosis.md)：用手算最小用例归因 64×252 S2 的日期错位，保持 B3 固定输入与 ADR 0012 边界不变；07 与 06 均被 08 阻塞。

POC-0 B2 票据 03 已完成两项判定负载的统一编排和 release 测量：[实现票据](.scratch/poc-0-b2-matched-remeasure/issues/03-full-decision-workload.md)、[综合原始报告](poc/poc0-benchmark/results/b2-matched-decision-loads-2026-09-28.json)。S2 3×10、S3 3×130 独立 correctness 均通过。fallback 模式下两负载均通过数值速度/RSS 门槛且先验预测成立；由于 `cached_conversion_new_engine` 尚无 reset parity 证据，03 子结果仍为 exploratory。票据 04 的 64×252 S2 parity failure 曾把 B2 overall 更新为 `reject`，现按 ADR 0014 为 `unresolved`，故本段的旧“唯一建议下一步”已由当前交接覆盖。报告、环境与构建细节仍见上方 03 证据链接。

B2 matched remeasure 票据 05 已验收为 `resolved`：[验收记录](.scratch/poc-0-b2-matched-remeasure/issues/05-maintenance-proxy-metrics.md)。维护代理已加入比较报告生成入口；03 归档报告记录 Fast Event POC / Nautilus Adapter 3,857 / 1,221 行、4 / 24 个测试、直接依赖 0 个 Cargo crate / 1 个 Python 包、ADR 0012 的 1 项语义差异。当前 `.venv` 有 Nautilus 2.0.0rc5；按启用的环境 marker 解析其安装依赖闭包为 0 个传递发行包，四个 `visualization` extra 未启用。报告保留解析方法并注明它不是完整 lock；缺失或冲突按 `Unknown` 处理。代理指标仅作记录，不进入判定。验收复核无票据 05 剩余缺项，`.venv/bin/python -m unittest discover -s poc/poc0-benchmark -p test_b2_matched.py -v` 为 17/17 通过；仍以票据 04、07、06 完成稳健性、模式选定和正式结论，下一步保持票据 04。

03 本轮验证：POC Python 测试 25 项通过；`cargo fmt --all -- --check`、票据 03 定向 Rust CLI 测试和 workspace Clippy 通过。workspace 全量 Rust 测试 16 + 65 + 12 通过、1 项 ignored，另有一个范围外 B1 sweep CLI 用例因文件缺失失败；根 `tests/` Python suite 21 项通过，`test_warehouse` 因环境缺少 `duckdb` 无法导入。Spec review 提醒报告要记录具体 CPU 与内存、依赖快照需按 active marker 处理、构建命令耗时与记录时间间隔要分开；这些均已修正。Standards review 提醒的过时交接信息已更正。Nautilus 依赖解析是已安装发行包快照，不是完整 lock。唯一建议下一步仍为上文票据 04；reset parity 由票据 07 在 03/04 后补证。

票据 12 综合报告已完成：[POC-0-SYNTHESIS.md](poc/poc0-benchmark/POC-0-SYNTHESIS.md)。结论为 B1 Custom SoA 在登记 64×252 负载 `defer`，Parquet 受限读取功能通过但布局性能 unresolved；B2 Fast Event/Nautilus 的共同正确性子集通过，速度/RSS 边界不匹配所以选型 unresolved；B3 Python per-bar 和 batch 对登记负载 `reject`，不外推通用 Python/GIL 边界。构建工程成本与运行时吞吐分开，cold build 仍 Unknown。报告保留固定数据、hash、参数、门槛、版本、原始样本/checksum 和复跑入口；不证明生产 Engine、真实 ETF PIT/总回报或实盘能力。

票据 12 文档链接检查通过。独立 review 的 Parquet/B2 RSS 信息缺口已补齐；`/code-review` Spec 轴指出的 Parquet 实测条件与性能门槛区分、B3 Python 转换/维护成本边界也已明确并通过定向复核，Standards 轴无发现。**唯一建议下一步：**如需裁决 B2，引擎双方按一致的数据准备、初始化、并行与 RSS 边界重测后再应用已登记门槛。

日期：2026-09-26。用户确认 Rust-first 多市场平台路线取代此前 A 股日频 + Web 初级产品交付顺序。目标决策见 [`ARCHITECTURE.md`](ARCHITECTURE.md)，新路线见 [`docs/product-roadmap.md`](docs/product-roadmap.md)，领域词汇见 [`CONTEXT.md`](CONTEXT.md)，POC-0 工作范围与验收见 [本地 spec](.scratch/poc-0-benchmark/spec.md)，执行规范见 [`docs/poc-0-benchmark-spec.md`](docs/poc-0-benchmark-spec.md)，目标 ADR 为 0009–0011。旧 A 股路线保存在 [`docs/legacy-ashare-roadmap.md`](docs/legacy-ashare-roadmap.md)。这些是目标/试验文档，**不是三级引擎、Nautilus 或 Python 入口的已实现证据**；现有功能与量化 P0 风险仍以 `docs/STATUS.md` 及下方 Batch 2 交接为准。

## 前次交接（2026-09-28，POC-0/10–11 实现）

票据 10 B3 基础回调对照已实现并通过独立 review：可选 `b3-pyo3` feature 增加 `benchmark-poc0-b3` CLI，PyO3 0.29.0 嵌入 Python 3.12.2。固定 3 ETF × 10 日 fixture 的 29 个 present-bar events 按交易日和 symbol 排序，Rust Native/Python empty 与 S1 回调收到同一事件流；S1 决策按日期、symbol 和目标逐事件相等，同日错误 symbol 会被拒绝。Python 输出的 S1 target 被传入同一 Rust Fast Event 账户 runner，所有订单/Fill、现金/持仓、成本、NAV 和 PortfolioResult 与 Rust reference 与独立 fixture 一致，checksum `f2ffcc44a2e94c778ad33e0731632bed69da98d05696f8934222f7e94f557928`。原始 release 五次样本、版本/数据 hash/provenance 在 [B3 callback report](poc/poc0-benchmark/results/b3-pyo3-callbacks-review-fix-2026-09-28.json)；复跑和限制见 [POC README](poc/poc0-benchmark/README.md#b3-pyo3-per-bar-callback-comparison-ticket-10)。单机微型样本不支持规模拐点、GIL 并行结论或生产选型；ticket 10 性能结论保持 `unresolved`。

初始 warm PyO3 build dev/release 分别 83.14/284.00 s，target 逻辑字节增量 2,427,198,271/434,471,616；Python 环境 162,369,399 bytes。最终源码 release warm rebuild 17.86 s、+1,022 target bytes。workspace target 共用且全程超过 10 GiB 保留线，未清理缓存。构建/测试/Clippy 资源原始记录列于票据 10 与 `poc/poc0-benchmark/results/b3-pyo3-*.json`。

票据 11 已由用户验收：S2 Momentum Rotation 与 S3 MA20/60 的 Rust Native、Python per-bar 与 Python batch 对比已完成。固定独立 golden 对拍通过，64 instruments × 252 sessions 合成目标负载的决策与 Rust 账户 checksum 三候选一致。预登记门槛为两策略均需 callback 并行中位数 ≤ Rust 2× 且端到端 2-worker Runs/s ≥ Rust 80%；最终 release 两策略两种 Python 模式均未达到，结论是 `reject` 该目标负载。Runs/s（Rust / Python per-bar / Python batch）：S2 126.04 / 6.67 / 14.48；S3 472.64 / 11.58 / 144.38。该结论限定在这台机器与合成 64×252 workload，不外推通用 Python 边界。进程共享峰值 RSS 91,078,656 bytes，不能归因单候选；Python 3.12.2 环境 162,369,399 bytes；最终 warm release feature build 19.51 s、target 逻辑字节变化 -960。原始报告与资源记录见 [ticket 11](.scratch/poc-0-benchmark/issues/11-pyo3-strategies-and-parallel-boundary.md)。

验证记录：`cargo fmt --all -- --check`、`python3 -m py_compile poc/poc0-benchmark/capture-build-resource.py`、`git diff --check` 通过；B3 S1 集成测试 1/1、B3 S2/S3 golden parity 测试 1/1 通过；`cargo test --workspace --locked --offline` 为 16 + 65 + 11 passed、1 ignored；workspace Clippy 和 B3 feature Clippy 均通过；S2/S3 release B3 正确性通过。票据 10 的独立 review P2 已修复并复核通过；本轮票据 11 按用户要求未做 independent review。Clippy 保留 vendor `polars-io` 的既有 29 项 unused 警告。调试/首次链接失败均有原始记录。未提交；保留 `.vscode/` 和 `poc/vendor/polars-io/` 下用户工作区既存未跟踪文件。

**唯一建议下一步：**执行票据 12 POC-0 综合结论，逐项保留 09 的跨引擎吞吐不可比状态与 11 的目标负载 reject 结论，不据此宣称生产技术选型。

## 前次交接（2026-09-27）

票据 09 实现交接：固定 3 ETF × 10 日 S2 Momentum Rotation 与 3 instrument × 130 日 S3 MA20/60 已由 Rust Fast Event 和 pinned Nautilus 2.0.0rc5 运行。S2 复用固定因子/排名/TopK 语义，`UNKNOWN` 入场被拒、`HALTED` 出场延期；S3 有独立版本化输入/预期，MA60 首次有效时不交易，cross-up/down 后次日 open 分别买卖 900 股。两个策略的信号、Adapter 项目订单、Fill、账户现金/持仓、成本与逐日 NAV 在共同子集通过。S2 原生停牌订单生命周期按 [ADR 0012](docs/decisions/0012-poc0-nautilus-status-gate.md) 排除在判定外，不冒充 Nautilus 原生拒单。本结论基于上述共同子集成立；停牌场景下的原生订单生命周期语义仍 `unresolved`，不构成本次结论的一部分。

[票据 09](.scratch/poc-0-benchmark/issues/09-b2-strategy-parity-and-throughput.md) 和 [POC README](poc/poc0-benchmark/README.md#b2-s2s3-registered-decision-protocol-ticket-09) 含复跑命令、预登记双策略中位延迟/并行吞吐至少 2 倍及 RSS 不增加的门槛。release 原始报告在 [combined](poc/poc0-benchmark/results/b2-09-comparison-2026-09-27.json)、[Rust S2](poc/poc0-benchmark/results/b2-09-s2-throughput.json) / [S3](poc/poc0-benchmark/results/b2-09-s3-throughput.json)、[构建](poc/poc0-benchmark/results/b2-09-release-build-2026-09-27.json)。Rust 两线程共享 prepared bars，Nautilus 两个 warm 进程每 Run 转换 QuoteTicks / 初始化引擎；Rust 全进程 RSS 含预检，Nautilus RSS 逐 worker。因此无法以这些不等口径验证预登记收益门槛，选型结论 `unresolved`，无 Sharpe 胜负断言。冷构建 Unknown、Python 传递依赖未完整锁定；synthetic golden 不证明真实 ETF PIT。

验证：`cargo fmt --all -- --check`、`cargo test --workspace --locked --offline`（bench CLI 11 passed，其他 Rust 测试通过，1 ignored）、`cargo clippy --workspace --all-targets --locked --offline -- -D warnings` 和 Nautilus/preflight Python 8 项均通过；`.venv/bin/python -m unittest discover -s tests -v` 21 passed、`test_warehouse` 因当前环境缺少兼容 `duckdb` 不能导入。独立双轴 review 提示 S3 golden 需导出，现已补齐且窄测试通过；声称没有 Nautilus S2/S3 样本或没有预登记阈值的发现经原始报告/README 复核不成立。后续 review 的未解决问题是双方吞吐/RSS 的不同口径。**唯一建议下一步：**为两个 B2 候选建立相同输入转换、计算、账户初始化和并行 worker/RSS 计时边界后，重新跑 release 两策略测量再做选型；不要为排除的停牌原生生命周期重复做阻断试验。

票据 05 已由用户于 2026-09-27 确认验收并标记 `resolved`：10,000,029 行、404,509,661 字节的 Parquet 在本机 Colima Linux ARM64 VM 的 cgroup v2 `memory.max=268435456` 下 `--reuse` 退出 0，`oom_kill=0`，只读取 2,448 组中的 1 组、7 列中的 3 列，三候选结果一致。子进程 RSS 58,933,248 字节，容器峰值 268,435,456 字节（含文件缓存及 Python 测量进程），`memory.events.max=646` 表示出现回收压力；不能将子进程 RSS 当作整个容器峰值。原始报告和 cgroup/Docker 配置证据见 [票据 05](.scratch/poc-0-benchmark/issues/05-parquet-and-out-of-core.md) 与 [POC README](poc/poc0-benchmark/README.md#parquet-b1-path-ticket-05)。

票据 06 已实现并标记 `resolved`。64 instruments × 252 synthetic weekday sessions、六个 S2 参数 Run、三布局、六次轮换重复全部通过独立 fixture、跨布局 `1e-8` 对拍和冷热逐 Run checksum。最终结论是 `defer`：SoA 相对最佳替代的两线程吞吐在 cache miss 为 +8.99%，cache hit 为 +0.32%，未达预登记 20% 门槛。顺序与并行计时均排除 checksum 序列化。release sweep 全进程峰值 RSS 107,905,024 字节；暖缓存 shared-harness dev/release 构建分别 5.582/9.675 秒，target 增量 -142,598,341/+53,203 字节（负值为共享缓存变化，非负成本）。报告明确为合成负载，不代表真实市场/PIT；报告的 Git diff 身份因未跟踪文件不完整，但构建记录含编译源码 hash 与未跟踪文件内容身份；每布局单独构建成本及冷构建 Unknown。复跑命令、结果表和限制见 [B1 sweep README](poc/poc0-benchmark/README.md#b1-parameter-sweep-and-factor-cache-ticket-06) 与 [票据 06](.scratch/poc-0-benchmark/issues/06-b1-sweep-cache-and-conclusion.md)。

本轮票据 06 验证：窄 CLI 测试、`cargo fmt --all -- --check`、`cargo test --workspace --locked --offline`（16 + 65 + 10 passed、1 ignored）、`cargo clippy --workspace --all-targets --locked --offline -- -D warnings`、脚本 `py_compile`、受资源闸门保护的 dev/release 构建、release sweep/RSS 与 `git diff --check` 均通过。`.venv/bin/python -m unittest discover -s tests -v` 有 21 项通过、1 个模块导入失败：环境缺少 `duckdb`，与本票无关。vendor `polars-io` 的既有未使用项编译警告保留。开放问题：合成负载不等于真实市场，冷构建和每布局构建成本仍 Unknown；不得据此作平台级布局裁决。

2026-09-27 票据 07 审核验收：独立复算金标准 fixture（`poc/poc0-benchmark/fixtures/dataset-v1.json`）的逐日现金/持仓/NAV，手算结果与 `b2_fast_event_buy_hold` 报告完全一致（Jan 13 现金 39740/NAV 100640 … Jan 16 现金 9610/NAV 101710，`total_cost` 390，`final_equity` 101710）；B 在 Jan 13 因 `HALTED` 状态被拒单、Jan 14 重试成交，B 在 Jan 16 缺 bar 时沿用 Jan 15 收盘 99.0 估值。非零买入税变体（移除 A 全部后续 bars、`buy_tax_rate=0.02`）手算 `tax` 1201.2、`final_equity` 99738.8、末日未执行目标 reason `no_successful_open_fill_before_dataset_end`、逐日现金非负，均与代码路径吻合。复跑 `cargo test -p quant-research --no-default-features --locked --offline --test poc0_benchmark_cli`（10 passed）、`cargo fmt --all -- --check`、`cargo clippy --workspace --all-targets --locked --offline -- -D warnings` 均通过。票据四条验收项与正确性范围说明均满足，标记 `resolved`；`.scratch/poc-0-benchmark/issues/07-fast-event-buy-and-hold.md` 已同步。

2026-09-27 票据 08 复审后状态为 `unresolved`。先前把 Adapter 自行生成的 B 停牌拒单计入 Nautilus 原生订单一致性，将 6/6 与 `resolved` 写入报告/交接，是错误的跨引擎结论；原两份报告作为历史证据保留。当前 [复核报告](poc/poc0-benchmark/results/nautilus-adapter-review-2026-09-27.json) 分列 Adapter 项目事件和 Nautilus 实际订单：Jan 12 close 形成目标，Jan 13 08:50 已知 B `HALTED`，09:30 Adapter 记零数量拒单、A/C 各成交 300 股；Jan 14 B 恢复后新订单成交 300 股。Adapter 项目事件 4 条与 Rust 4 条一致，但 Nautilus 仅收到 3 笔订单，未执行原生 HALTED 拒单。七项比较六项通过（项目事件、Fill、现金/NAV、持仓、直接 Portfolio 账户、成本），原生订单生命周期一项未通过；跨引擎吞吐结论保持 `unresolved`。该时间模型和可手算现金示例见 [ADR 0012](docs/decisions/0012-poc0-nautilus-status-gate.md)。

受控入口 [nautilus_preflight.py](poc/poc0-benchmark/nautilus_preflight.py) 在 pip 安装和共享 target release 构建前实测可用空间、保留预计完成后 10 GiB，运行中继续监控；不足时保存 `unresolved` 原始记录并拒绝安装/构建。成功 [预检](poc/poc0-benchmark/results/nautilus-preflight-review-2026-09-27.json)、[构建](poc/poc0-benchmark/results/nautilus-build-review-2026-09-27.json) 与强制过大估算的 [拒绝记录](poc/poc0-benchmark/results/nautilus-preflight-space-blocked-2026-09-27.json) 均已保存。Nautilus 内部 1 次预热后 5 次重复的转换、初始化、事件处理、端到端原始样本及统计量已记录，投影一致；这些不是跨引擎吞吐结论。报告含 Cargo.lock/requirements hash、pip freeze 身份、版本和编译配置；Python 传递依赖只记录已解析版本，未形成完整锁；cold build 与 cold start 均 Unknown。

票据 08 复核验证：`cargo fmt --all -- --check`、`cargo test --workspace --locked --offline`（16 + 65 + 10 passed、1 ignored）、`cargo clippy --workspace --all-targets --locked --offline -- -D warnings`（无新增告警，既有 vendor `polars-io` 未使用项告警保留）均通过；`test_nautilus_adapter.py` 与 `test_nautilus_preflight.py`（5 passed，含 HALTED 拒单、账户与生命周期对拍分离、空间不足阻断三项）通过；`.venv/bin/python -m unittest discover -s tests -v` 22 项中 21 passed，`test_warehouse` 因当前环境缺少 `duckdb` 无法导入，与本票无关。开放问题：Nautilus 原生停牌拒单路径、完整 Python 依赖锁与可比的跨引擎吞吐仍未验证。

**唯一建议下一步：**先确认 Nautilus 是否能在同一固定 fixture 上产生可证的原生 HALTED 拒单及重试；若不能，保持订单生命周期差异和票据 08 为 `unresolved`，再界定票据 09 可比较的共同子集。

前次交接：本地 `polars-io` vendor 解开离线 Parquet 依赖；新增固定 fixture 的版本化 Parquet/manifest、`--reuse`、列与 row group 裁剪、三候选对拍和分阶段报告。复核时修正了 `ParallelStrategy::None` 对零重叠组仍进入列读取路径的问题。10,000,029 行、404,509,661 字节源文件改用解码前跳过无关组的策略后，2,448 组中选 1 组，独立进程 RSS 57,573,376 字节；原始结果见 [票据 05](.scratch/poc-0-benchmark/issues/05-parquet-and-out-of-core.md)。当时 256 MiB 地址空间限制在 macOS 启动前失败；此缺口现由上方 Linux cgroup 测量补齐。

前次代码验证通过 `cargo fmt --all -- --check`、`cargo check -p quant-research --no-default-features --locked --offline`、`cargo test -p quant-research --no-default-features --locked --offline --test poc0_benchmark_cli parquet_cli_round_trips_fixture_and_prunes_groups_and_columns`、`cargo test --workspace --locked --offline`（16 + 65 + 9 passed，1 ignored）、`cargo clippy --workspace --all-targets --locked --offline -- -D warnings` 和 `git diff --check`。本轮 Linux `cargo build -p quant-research --no-default-features --locked`、大样本生成和两次受限 `--reuse` 均通过；本轮未重跑全工作区测试。依赖 vendor 编译有未使用项警告；macOS 原生限制试验仍失败，不应冒充 Linux cgroup 的成功证据。

2026-09-27 构建资源决策（票据 13 完成前的交接记录）：用户确认先优化 POC-0 spec/票据，再由新 [票据 13](.scratch/poc-0-benchmark/issues/13-build-resource-boundary.md) 实施轻量构建边界。现场只读清点：根 `target/` 约 58 GB、独立 B1 target 约 1 GB、卷可用约 14 GiB，重复的 bundled DuckDB 构建产物是主要占用。新增构建须开工及预计结束均保留至少 10 GiB；记录增量/release 构建耗时与产物增量，冷构建仅在预算允许时测。票据 03/04 均已由用户确认验收。不得自动清空整个 target。本轮未清理文件。后续票据 13 已完成，当前票据 05 状态见上方交接。

2026-09-27 票据 04 已由用户确认验收并标记 `resolved`：`benchmark-poc0 --candidate polars` 使用 Polars lazy expressions 从压缩的 observed-close 序列计算短/长动量与样本波动率、复合分数、确定性排序、TopK/等权目标和简化组合收益；权重分母按实际入选数量计算。独立 CLI 用例覆盖 4 观察期首个有效日及手算分数、符号零平局、固定 fixture 手算收益、缺少次日 bar 返回 null、少于 top_n 时实际入选数量均分，以及错误/变体输入跳过计时。release 原始报告保存在 [`Polars results`](poc/poc0-benchmark/results/b1-polars-2026-09-27.json)。此前复验：`cargo test --workspace --locked --offline`（89 passed、1 ignored）、`cargo clippy --workspace --all-targets --locked --offline -- -D warnings`、`cargo fmt --all -- --check` 均通过。第一次 release 构建因磁盘空间不足中断；只清理了当时创建的临时 Cargo target 后，release 构建和运行成功。当前工作树还含 Arrow 未提交改动；两票代码尚未提交。固定 3×10 样本不支持布局/性能决策。票据 03 此后亦由用户确认验收；票据 13 之后完成，当前待办更新见上方。

提交已按领域拆分：旧 A 股状态取证 `e8764e1`、日更与身份导入 `8e8bff9`、诊断作业与页面 `c7906ab`；Agent 工具配置 `d8fab3a`；新平台架构与旧路线归档 `15cc6d5`；POC-0 计划 `20eb209`、B1 标量 smoke `e8c3bd8`、01 已验收 harness `6401651`、07 最小事件原型 `a907212`、02 SoA 候选 `d78df42`。各提交仅含所属批次文件，未推送远端。

当前 POC 状态：01–06 已验收/关闭；03 Arrow 的候选实现验收与 06 的同条件 release sweep 已完成。06 对 64 × 252 合成负载给出 `defer`，没有采纳 Custom SoA，也不构成真实市场/PIT 结论。07 是最小事件原型，不能支持 Fast Event 选型；08 仍依赖 07 的验收。Nautilus、PyO3 和真实 ETF 历史可信度门槛均未完成。

2026-09-27 票据 03 Arrow：用户已确认实现验收并标记 `resolved`。`benchmark-poc0 --candidate arrow` 使用 Arrow `RecordBatch` 完成 S2 因子、排序、TopK、权重和下一会话 close-to-close 收益；独立用例覆盖首个有效日、排名分数、符号升序平局、末日缺 bar 排除、所选目标下一日缺 bar 返回 `None` 和手算收益。报告含五次 raw 样本、分阶段耗时、checksum 和单独转换耗时。`cargo check -p quant-research --locked`、全工作区测试（89 passed、1 ignored）、Arrow 定向测试、`cargo clippy --workspace --all-targets --locked --offline -- -D warnings`、`cargo fmt --all -- --check` 与 `git diff --check` 通过。没有生成 Arrow release 性能样本，架构性能比较仍 Unresolved；验收不表示采用 Arrow 布局。

## 历史进展记录

2026-09-27 更新：POC-0 票据 01 已按用户要求完成验收并标记 `resolved`。复跑命令为 `cargo run -p quant-research --release --locked --offline -- benchmark-poc0`，输出在 `target/poc-0/benchmark-report.json`。它对拍独立固定输入/金标准、逐日账本与费用，并在正确性通过后保存 5 个 Rust 参考路径耗时样本；RSS 暂为 Unknown，单 fixture timings 不能用于布局决策。本次复验通过：`cargo fmt --all -- --check`、`cargo test --workspace --locked --offline`（82 passed，1 ignored）、`cargo clippy --workspace --all-targets --locked --offline -- -D warnings`，以及 release 基准命令；错误金标准的 CLI 用例退出失败并跳过计时。入口、输入身份、时间规则、报告限制见 [`POC-0 harness`](poc/poc0-benchmark/README.md) 和票据 01。**唯一建议下一步：**完成票据 02 的 SoA 候选选择及分阶段计时。

2026-09-27 更新：POC-0 票据 07 增加 `b2_fast_event_buy_hold` 最小 L1 事件原型，按 Jan 12 收盘目标、次日开盘下单，记录订单/Fill、成本、逐日现金/持仓/NAV；B 在 Jan 13 停牌时拒绝、次日重试，B 在末日缺 bar 时沿用上次估值。固定输入和变体输入独立检查重复账本 checksum、次日执行、NAV 恒等式、末日未执行目标和含非零买入税时现金非负；正确后分别记录初始化、事件处理、端到端五次样本。该结果只证明原型 fixture 行为，不是完整 Fast Event，也不构成性能选型。当前工作树预先含多项其他未提交改动，本轮不应将它们一并提交。详见 [票据 07](.scratch/poc-0-benchmark/issues/07-fast-event-buy-and-hold.md) 和 [benchmark harness](poc/poc0-benchmark/README.md)。

本轮另建立独立的 [`poc/b1-layout`](poc/b1-layout/README.md) 标量读取切片，锁定 Polars 0.55.2、Arrow 60.0.0，自定义 SoA 与两者的动量输出通过独立手算小例和 checksum 对拍。`cargo check --manifest-path poc/b1-layout/Cargo.toml --locked --offline`、`cargo fmt --manifest-path poc/b1-layout/Cargo.toml -- --check`、`cargo clippy --manifest-path poc/b1-layout/Cargo.toml --all-targets --locked --offline -- -D warnings`、release 运行与 `git diff --check` 通过。一次 128 证券 × 4096 日、lookback 20、10 次重复的原始 smoke 结果与输入 hash 保存在 [`poc/b1-layout/results/2026-09-26-local-smoke.json`](poc/b1-layout/results/2026-09-26-local-smoke.json)；SoA / Arrow / Polars 标量访问中位数分别为 931 / 1303 / 3533 微秒，计数和 checksum 相同。CPU 型号无法读取，工作树含现有未提交改动；固定候选顺序且只测标量访问，**不能据此选择布局**。未运行主仓库全量测试，未变更其量化代码。

（历史建议，已由上方“当前交接”中的票据 05 复跑步骤取代。）完成 POC-0 B1 的同语义完整工作负载：独立 S2 期望输出、Polars 表达式、Parquet 扫描/转换、排名/TopK/组合收益、内存与 out-of-core 测量；然后接入 B2/B3。真实 ETF 历史结果继续受 PIT、总回报和状态证据门槛约束。

2026-09-26 票据交接：用户确认了 12 张 POC-0 纵向票据，已按依赖顺序发布在 [本地 issues](.scratch/poc-0-benchmark/issues/01-fixed-dataset-and-benchmark-entry.md)。01 是当前唯一无阻塞票据；其余票据仍 `ready-for-agent`，尚未执行。本轮只编辑 Markdown，没有运行代码、格式器或测试；现有工作区未提交改动已保留。**唯一建议下一步：**从 01 的固定数据集、独立金标准和统一基准入口开始，完成后再解锁 02 与 07。

## 历史交接：Batch 2 集成后

日期：2026-09-25。正式矩阵见 [Batch 2 集成验收](docs/handoffs/batch2-integration-acceptance.md)，最小字段与边界见 [公共契约](docs/contracts/batch2-contract.md)。Batch 1 的 [`legacy_bar_only` 结论](docs/handoffs/batch1-integration-acceptance.md)没有被新占位复跑替代。

## 当前结论

- **A：部分通过。** 五 ETF 固定窗口 241 日×5=1205 个执行状态格均为 UNKNOWN；一条 513300 盘中临停仅有二级消息，缺交易所原文及 hash。A 的 `etf_status_gated_replay` 锁定同一价格快照 `fe1dd8a3-d227-4e06-bb98-f0a6b3be0f34` 和 Universe 版本 `3fcfed9c-b8c1-4cce-a933-bc2b79ebf0da`，在 UNKNOWN 占位输入下由旧 32 笔变为 0 成交/24 个拒单，逐日 NAV 独立误差至多 4.66e-10；这只证明 fail closed，P0-3 未关闭。最新集成方复跑实验 `709a4f67-6aca-427c-8064-8aac413d1764`；旧真实实验 `4e076758-8069-4e0e-9269-1d3f9d574159` 仍 `legacy_bar_only`。
- **B：部分通过。** 显式证券 `sync-daily`/`sync-daily-latest`、失败恢复、修订回看、质量审计、`publish-sync` 和不可变目录/current 指针已实现。隔离 Tencent 股票 `sh600519`+ETF `sh510300` 的重复同步 8 行、0 新修订；当前发布 snapshot `56a22581-b3e1-49c8-9e32-89c754b154e8`、manifest SHA-256 `a436483185e0248a28d5f7b91a118534632b500754573a9363aa67c7a5042484`、Parquet SHA-256 `63ab4d0b67def0b2ec02e7c5f0d1921138e277dda1e400c0fef962663e60624a`，截止 2026-09-24。只证明两只显式证券的行情日期覆盖；目录分类和状态仍 UNKNOWN，生产回填状态 Unknown，调度模板未启用。
- **C：合成通过、生产未验。** `/api/v1/instruments`、`/api/v1/daily-bars` 与 `/market` 股票/ETF 日 K 和成交量已实现；B 的正式发布格式与 C 的只读 resolver 已集成，新增固定旧版本测试。合成浏览器验证了两类证券各 1000 根、OHLCV、单位、原始价、截止日和快照 hash。B 真实隔离样本没有稳定证券身份，不能据此声称生产股票搜索/页面已验。股票回测仍阻断。

五 ETF 固定成员仍为 `retrospective_static`，收益为零分红/原始价格口径；P0-1 历史成员/上市终止、P0-2 公司行动/总回报、P0-3 真实状态覆盖与可知时刻均开放。沪深300真实历史成分核验继续暂缓。`observed_at` 是本地采集时间，不是历史发布时间；bar 存在不证明可交易。

## 使用与恢复

```bash
# 隔离或经授权的单写者数据目录；生产 writer 状态不明时不要并发运行
cargo run -p ashare-warehouse --release --locked -- --data-dir data-core sync-daily-latest --symbol sh600519 --symbol sh510300

# 先构建前端，再启动只读服务；行情从已发布快照目录读取，不打开写库
cd apps/web && npm run build && cd ../..
cargo run -p quant-research --release --locked -- serve --output research-output --market-data-dir data-core --web-dist apps/web/dist --address 127.0.0.1:7878
```

`--market-data-dir` 缺省为 `data-core`；无 `snapshots/current.json` 时新行情 API 返回不可用，不退回未发布文件。A 占位审计/复跑命令、B 隔离样本命令与调度模板、C 合成快照/browser 命令分别见三个独立交接。生产 `data-core/.writer.lock` 存在且实时持有者无法证明，本批没有写入生产库；真实数据和实验目录被 Git 忽略。项目只需本地集成，不推送、不发布远程服务、不安装 LaunchAgent。

## 验证与开放问题

`cargo fmt --all -- --check`、`cargo test --workspace --locked --offline`（14 仓库+56 研究、1 真实 opt-in ignored）、`cargo clippy --workspace --all-targets --locked --offline -- -D warnings`、`cd apps/web && npm run build`、`git diff --check` 通过。Python 状态审计 19/19、调度脚本 `sh -n`、A 来源证据 JSON 语法校验通过。全量 `.venv/bin/python -m unittest discover -s tests -v` 因环境未安装 `duckdb`，旧 `test_warehouse.py` 导入失败，未记为通过。浏览器在合成发布目录验证股票/ETF K 线；第二个本地端口的 B 真实样本 HTTP 验收受浏览器/CLI 网络限制，未记为通过。B 样本四个发布文件 hash 由本地检查匹配。

开放风险：官方两市五 ETF 逐日状态原文/覆盖/历史可用时刻缺失；股票身份/分类和生产快照未验；交易所状态中的盘中限制、涨跌停方向、排队/部分成交未入日频引擎；生产同步及调度未启用；远程认证部署、Python 因子研究层仍缺。B 的行情 `complete` 是显式范围结论，不代表状态或全市场完整。

**唯一最优先下一步：**取得并归档覆盖固定五 ETF 窗口的两市官方逐日执行状态文件与完整性说明，核验每证券/日期的原文 hash 和历史可用时刻，经 B 单写者冻结真实状态，在同一发布 Universe 版本上复跑；证据不足时保持 UNKNOWN/P0-3 开放。

2026-09-27 更新：POC-0 票据 02 已由用户验收并标记 `resolved`。统一 `benchmark-poc0 --candidate soa` 报告覆盖 SoA 动量/样本波动率、截面排名、TopK、等权目标和次日 close-to-close 权重收益；对照独立事件账本金标准校验排名与目标，错误时跳过性能测量。报告提供因子、排序/权重、收益三阶段耗时样本，并按 `rebalance_every` 生成目标。独立手算用例覆盖内部缺 bar 和不足窗口。该权重收益模型不含现金、费用、订单、成交或事件 NAV；固定 3×10 fixture 的性能数据不支持布局决策，完整 Polars/Arrow/Parquet 比较仍未完成。最终验证：83 项通过、1 项按条件忽略；格式、Clippy、两个 release 候选 CLI 均通过。

2026-09-27 更新：POC-0 票据 13 已实现 `app` 可选 feature 构建边界。正常应用默认功能不变；POC 命令以 `--no-default-features` 构建，依赖树不含 `ashare-warehouse`、DuckDB 或 `libduckdb-sys`，B1/B2/B3 共用根 `target/`。构建资源原始记录与命令见 `poc/poc0-benchmark/README.md` 和 `poc/poc0-benchmark/results/`。初始 warm-cache dev 39.645 秒、target 逻辑字节增量 511,089,592；release 264.239 秒、增量 461,279,366 字节。带 dirty/source identity 与运行时空间监控的复跑 dev 21.175 秒、release 3.017 秒；均未触及 10 GiB 停止线。预算拒绝样例记录了 3 GiB 估算时的 10,964,553,728 bytes 可用空间。冷构建没有测量，未清理任何 target。旧 `du -sh` 是分配块估算，资源 JSON 记录文件逻辑大小，不能直接对比。最终验证：`cargo fmt --all -- --check`、`cargo test --workspace --locked --offline`（89 passed、1 ignored）、`cargo clippy --workspace --all-targets --locked --offline -- -D warnings`，以及轻量 release Polars CLI correctness 均通过。后续重依赖/大数据任务仍应在各自构建前估算并保留 10 GiB。
