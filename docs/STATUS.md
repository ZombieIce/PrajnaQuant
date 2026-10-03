# Project Status

2026-09-30 GitHub Issue #15 在分支 `15-manifest-dataset-version` 实现 `prajna-data` 的 D8 Manifest 与 Dataset Version：`Manifest::new` 对 inputs/table 排序并按受限 JCS(core) 计算 `dsv:sha256:<hex>`；provenance 不参与身份。manifest 写至 `manifests/<dsv_hex>.json`，先同步 staging 文件再原子 rename；现存相同 core 幂等，core 不同时报错；读取会重算并验证 DSV。固定手写 JCS/DSV 向量及 provenance、core 字段变化、顺序、篡改、幂等与冲突测试均通过。轻量 CI 对应的本地验证通过。PR [#40](https://github.com/ZombieIce/PrajnaQuant/pull/40)；Independent review: see PR #40。

2026-09-30 GitHub Issue #13 实现 `prajna-data` 的受限 JCS、schema fingerprint 与 Arrow logical hash。hash 按主键排序，解码字典列，拒绝重复键和 Appendix A 外类型；schema fingerprint 排除 `prajna.dsv`。两行 `sessions` 测试向量（含 null、负 Decimal128 和多字节 UTF-8）已提交为 hex fixture，并发布到 [spec Issue #7](https://github.com/ZombieIce/PrajnaQuant/issues/7#issuecomment-5902283970)；独立 `xxd -r -p | shasum -a 256` 输出 `2c627a1813303394238717869776a089234b08da095a9c1bae0198ca26c2b58c`。定向 test/clippy、格式与新 crate 依赖守卫通过。轻量 `quant-research` test 中既有 Parquet round-trip 用例因本机约 2.98 GB 可用空间低于 10 GiB reserve gate 而返回 `unresolved`；其余过滤后用例通过。实现者双轴自查无剩余发现。PR [#37](https://github.com/ZombieIce/PrajnaQuant/pull/37)；Independent review: see PR #37。

2026-09-29 GitHub Issue #10 在分支 `10-time-bar-types` 实现 MVP-0 时间与 Bar 契约：`TimestampNs` 只接收带显式 offset 的 RFC3339；`BarSpec` 支持规范化 round-trip 与每日 `@session`/固定 offset 区间；`Session` 检查严格递增边界；`BarData` 经 OHLCV、日期、时间和区间校验后构成不可变 `Bar`，未知 `available_at` 保持为空。`cargo fmt --all -- --check`、`cargo test -p prajna-domain --locked --offline`（18 passed）、workspace Clippy、`quant-research --no-default-features --features b3-pyo3` 检查及 Python 3.12 POC 测试（42 passed，8 个预期 Nautilus skip）通过。全 workspace 与 research 轻量测试均遇到既有 Parquet CLI 10 GiB 空间闸门：可用空间约 5.77 GB，相关用例报告 unresolved；未绕过闸门。PR [#36](https://github.com/ZombieIce/PrajnaQuant/pull/36)；Independent review: see PR #36。

2026-09-29 GitHub Issue #12 在 `prajna-data` 实现 D6 Raw store：对象使用 `sha256:<小写 hex>` 身份及 `raw/sha256/<前两位>/<hex>` 路径；唯一临时文件写入并 fsync 后原子 rename；已有对象重复写入及 `get` 均校验字节 hash。来源 JSONL 记录字段为 `raw_sha256`、`byte_len`、`content_type`、`source_kind`、`source_id`、`request`、`observed_at`、`ingested_by`；URL query/userinfo、header、JSON 与表单 body 的敏感 key（大小写不敏感）会被替换为 `[REDACTED]`。合并 #13 后 `prajna-data` 锁定离线测试 11 项通过（含 5 项 Raw store 测试），Clippy、格式与新 crate 依赖守卫通过。轻量 CI 的 quant-research 测试有 1 个 Parquet CLI 用例因本机空间 6.2 GiB 低于其 10 GiB 预检门槛而停止；其他 38 项通过。Python 3.12 POC 测试 42 项通过、8 项按 Nautilus 缺失条件跳过。实现者自查无剩余发现。PR [#35](https://github.com/ZombieIce/PrajnaQuant/pull/35)；Independent review: see PR #35。此票只实现 Raw 层，不表示 normalized 表或 Dataset Version 已实现。

2026-09-29 GitHub Issue #9 在分支 `9-instrument-identity` 实现了 `VenueId`、`Currency`、规范 `InstrumentId`、独立声明的 `InstrumentKind`、经校验的 `InstrumentSpec` 与同 ID 重用检测；并提供 SH/SZ/BJ 旧格式到 XSHG/XSHE/BJSE 的单向映射。实现位于 `prajna-domain`，没有修改 `quant-research`。Domain crate 测试/clippy、workspace 轻量 Rust 路径（`quant-research` clippy/test、`b3-pyo3` check、DuckDB 依赖守卫）及格式检查通过。Python 轻量测试因本机缺少 `.venv` 且系统 Python 为 3.8，无法按 CI Python 3.12 环境验证。实现者双轴自检无剩余发现。PR [#33](https://github.com/ZombieIce/PrajnaQuant/pull/33)；Independent review: see PR #33。#7 的其余 Domain Core 与 Data/Arrow 范围未实现。

2026-09-29 GitHub Issue #31 adds the repository-local `implement-ticket` and `review-pr` workflows and clarifies shared-account review roles and the pre-merge review check. The workflows are agent guidance, not automated enforcement. Independent review: see [PR #32](https://github.com/ZombieIce/PrajnaQuant/pull/32).

2026-09-29 GitHub Issue #11（MVP-0 `prajna-data`）在分支 `11-prajna-data-skeleton-parquet-duckdb-guard` 新增 workspace crate 骨架，依赖 `prajna-domain`、Arrow 58.4.0 与 Parquet 58.4.0（仅启用 `arrow`/`zstd`）；Cargo.lock 已在线更新并提交。轻量 CI 对 `prajna-domain` 与 `prajna-data` 检查 DuckDB、Polars 和 `ashare-warehouse`，并为 `prajna-data` 增加 Clippy/test。`cargo fmt --all -- --check`、`cargo test --workspace --locked --offline`（104 passed、1 ignored）、workspace Clippy 及新 crate 定向验证通过；workspace Clippy保留 vendor `polars-io` 的 29 项既有 warnings。PR [#34](https://github.com/ZombieIce/PrajnaQuant/pull/34)；Independent review: see PR #34。此票只交付骨架，不包含数据业务逻辑。

2026-09-29 GitHub Issue #8 在分支 `8-prajna-domain-fixed-point` 实现并完成独立审查：新增轻量 `prajna-domain` workspace crate，提供固定 scale 18、`i128` 尾数且 `|mantissa| < 10^38` 的 `Price`、`Quantity` 与 `Notional`（`Amount` 为别名）。JSON 数字字面量文本直接解析，不经 `f64`；Serde 配置为字符串；测试覆盖规范化 round-trip、精度/边界拒绝、tick 对齐、整数倍溢出、比较和显式 `to_f64()`。轻量 CI 已加入此 crate 的 DuckDB 守卫、Clippy 与测试。`cargo fmt --all -- --check`、`cargo test --workspace --locked --offline` 与 `cargo clippy --workspace --all-targets --locked --offline -- -D warnings` 通过（vendor `polars-io` 保留既有 warnings）。此记录不表示 #7 的其他 Domain Core、Data Lake 或 Arrow/Parquet 契约已实现。

2026-09-28 POC-0 B2 正式结论（GitHub Issue #3）：`adopt`，范围限于固定 S2/S3 3×10、3×130 判定负载及 64×252 稳健性负载。四个负载的共同子集正确性均通过；在 Nautilus 2.0.0rc5、统一选定的 `cached_conversion_new_engine` 模式下，延迟中位、2-worker Runs/s 中位与 RSS 峰值和上界门槛均通过，预登记速度/RSS 预测成立。登记的判定与稳健性负载没有结论翻转；其他计时边界或未选模式的敏感性未测，保持 unresolved。最终原始样本、checksums、模式证据、provenance 与 build record 见[正式报告](../poc/poc0-benchmark/results/b2-formal-robustness-reset-selected-2026-09-28.json)、[release build](../poc/poc0-benchmark/results/b2-formal-release-build-2026-09-28.json)和[复跑说明](../poc/poc0-benchmark/README.md)。此结论不裁决 Nautilus 原生 reset（其错误日志触发统一 fallback）、ADR 0012 停牌生命周期、生产 Engine 选型或真实 ETF 业绩；MVP-3 开工前须由项目负责人审阅该结论及范围。2026-09-29 项目负责人已确认范围（Issue #25）：批准 MVP-3 自研 Fast Event L1 作为高吞吐事件层；该结论比较的是 Rust 引擎 + Rust 策略对 Nautilus + Python 策略，不是引擎核心性能，不否定 Nautilus 的 Accurate/Live 候选地位，也不评判维护成本。

2026-09-28 B2 reset parity（GitHub Issue #2）已在固定 Nautilus `2.0.0rc5` 上实测：S2 3×10、S3 3×130 与 S2/S3 64×252 四个负载，各连续运行三次 reset Run；完整投影及引擎状态快照与新建引擎一致，两个判定负载 checksum 与先前经 Rust golden 字段对拍的 Nautilus projection checksum 一致。但每次 reset Run 均记录 Nautilus 原生 `Invalid state trigger READY -> INITIALIZE` 错误日志，因此按 fail-closed 规则统一选择 `cached_conversion_new_engine`。带模式证据重跑后 S2/S3 decision loads 为 registered `adopt`，完整数据见[决策负载报告](../poc/poc0-benchmark/results/b2-matched-decision-loads-reset-selected-2026-09-28.json)；reset 错误日志、输入 hash、checksum 与状态快照见[reset evidence](../poc/poc0-benchmark/results/b2-nautilus-reset-parity-2026-09-28.json)，复跑入口见[README](../poc/poc0-benchmark/README.md)。当时 64×252 robustness 未按所选模式 registered 复核的状态，已由 GitHub Issue #3 的正式重测与上方结论更新；没有据此得出生产 Engine 或真实 ETF 收益结论。

实现扫描日期：2026-09-25；目标路线更新：2026-09-26。状态含义：Implemented=代码存在；Partially Implemented=只覆盖部分契约；Planned=有明确路线但无实现；Missing=未见实现；Unknown=仓库证据不足。**Implemented 不表示量化正确性已被充分测试或数据已被验收。**代码仓库已关联 GitHub `ZombieIce/PrajnaQuant`；本机被忽略的数据仍不是可交接资产。

## Target Architecture Baseline (2026-09-26)

2026-09-28 B2 票据 08 归因（Issue #3 完成前的中间状态，已由顶部正式结论取代）：固定 64×252 S2 首差附近的 10 只 ETF / 68 session 缩减用例，独立手算 04-08 现金 48350、NAV 99560、ETF038 当日买入 100 股；Rust 与修复后 Adapter 均通过。根因是 Adapter 逐证券开盘 Quote 顺序使买入早于卖出；修复后双方各 351 个订单/Fill，全部计入字段对齐，无剩余独立差异类别。[诊断票据](../.scratch/poc-0-b2-matched-remeasure/issues/08-s2-64x252-parity-diagnosis.md)与[原协议重测](../poc/poc0-benchmark/results/b2-robustness-64x252-parity-diagnosis.json)记录 S2/S3 correctness 与数值门槛通过。按 ADR 0014，故障属 Adapter，不触发 Fast Event `reject`；当时 reset parity 尚未验证，fallback 性能仅 exploratory，整体 B2 当时为 `unresolved`，ADR 0012 原生停牌生命周期继续排除。下方票据 04 的 351/348 数据是修正前归档，不代表当前状态。

2026-09-28 POC-0 票据 14 单 instrument 隔离实验：固定 Nautilus 2.0.0rc5 的公开 BacktestEngine 收到 QuoteTick、HALT 后，在两组同一后续时刻由策略直接提交的市价单触发原生 `on_order_rejected`（`Market PROBE.SIM is CLOSED`），引擎订单状态 `REJECTED`；无 HALT 对照在同一报价成交、状态 `FILLED`。[证据与复跑入口](../poc/poc0-benchmark/results/nautilus-halt-native-probe-2026-09-28.md)。MVP-4 可考虑由 Nautilus 原生状态处理停牌，但当前 Adapter 未改变，B2 判定范围及旧 A 股/ETF 执行时序均不变。

2026-09-28 POC-0 综合结论见 [POC-0-SYNTHESIS](../poc/poc0-benchmark/POC-0-SYNTHESIS.md)：B1 Custom SoA 对登记合成目标负载 `defer`；B1 Parquet 受限读取功能通过，布局性能 unresolved；B2 在四个固定合成负载及登记模式下 `adopt`，不构成生产 Engine 选型；B3 Python per-bar 和 batch 对 64×252 登记负载 `reject`，不外推其他规模。无真实市场 PIT 或历史 ETF 总回报结论。

2026-09-28 B2 matched remeasure 票据 04 完成（Issue #3 完成前的中间状态，已由顶部正式结论取代）：Rust B2 CLI 支持 B3 注册的 S2/S3 64×252 Dataset Version；stress fixtures 与登记 content hash、Rust account checksum 一致。两次独立 S2 Nautilus 对拍 checksum 一致，且相同六个非排除字段失败（订单/Fill、现金/NAV、持仓、账户现金/持仓、成本）：Rust 有 351 个订单/Fill，Nautilus 有 348 个。首个订单/Fill 日期为 Rust 2025-04-08、Nautilus 2025-04-09；HALT override 日期/证券在双方都没有项目订单或 Fill，ADR 0012 原生生命周期字段仍独立排除。S2 性能样本按 correctness-first 规则跳过；B2 overall 原映射为 `reject`，按 ADR 0014 改为 `unresolved`（差异未归因，该负载无独立金标准），诊断见 B2 remeasure 票据 08。S3 对拍及速度/RSS 数值门槛通过：串行中位 2,455,667 / 53,476,979.5 ns、并行中位 535.84 / 34.21 Runs/s、RSS 90,996,736 / 234,848,256 bytes（Nautilus worker 峰值和上界）。Nautilus 版本 2.0.0rc5；完整重复对拍、紧凑差异样例、原始样本和 provenance 见 [稳健性报告](../poc/poc0-benchmark/results/b2-robustness-64x252-2026-09-28.json)，10 GiB 空间闸门与 release build 记录见 [构建记录](../poc/poc0-benchmark/results/b2-robustness-release-build-final-2026-09-28.json)。

2026-09-28 B2 matched remeasure 票据 03 已完成（Issue #3 完成前的中间状态，已由顶部正式结论取代）：S2 3×10 与 S3 3×130 独立 correctness gate 均通过，并按 20 个单 worker 串行 Run、五组交替顺序的 2-worker × 6 Run 保存主计时及每 Run 冷端到端样本。fallback（缓存 QuoteTicks、每 Run 新建 Nautilus 引擎）下两负载都通过数值延迟/吞吐/RSS 门槛，先验预测成立；由于 reset parity 未验证，正式 decision 当时保持 `unresolved`，两个子负载仅为 exploratory。Rust/Nautilus 中位单 Run 延迟：S2 11,458 / 895,709 ns、S3 71,583 / 3,038,480 ns；五组并行 runs/s 中位：S2 61,224 / 1,219、S3 13,631 / 554；RSS：S2 11,534,336 / 145,965,056 bytes、S3 12,189,696 / 149,291,008 bytes（Nautilus 两 worker 峰值和上界）。空载 RSS 为 Rust --help 启动 8,650,752 bytes、Python 导入 Nautilus 后 50,593,792 bytes。报告、完整 provenance 和 10 GiB build record 见 [决策负载报告](../poc/poc0-benchmark/results/b2-matched-decision-loads-2026-09-28.json)、[build record](../poc/poc0-benchmark/results/b2-matched-decision-loads-release-build-2026-09-28.json)；reset 模式由票据 07 决定、64×252 稳健性由票据 04 补测、正式复测由票据 06 完成。

2026-09-28 B2 matched remeasure 票据 02（探索性 S2 tracer）已完成。较早 S2 结果保留作历史证据；应以票据 03 两策略统一报告作为当前判定负载证据。旧报告和范围见 [票据 02](../.scratch/poc-0-b2-matched-remeasure/issues/02-s2-golden-matched-tracer.md)。

2026-09-28 B2 matched remeasure 票据 05 已验收并标记 `resolved`：比较报告生成时记录 Fast Event POC / Nautilus Adapter 源行数、测试数、增量直接依赖、Nautilus 安装依赖闭包及 ADR 0012 语义差异引用；代理指标不参与纯判定函数。03 归档报告中的源文件范围计数为 3,857 / 1,221 非空非注释行、4 / 24 个对应测试，Rust 新增 crate 为 0，Python 直接依赖为 1（`nautilus_trader==2.0.0rc5`）。03 环境按 active marker 解析到 0 个传递发行包；四个 `visualization` extra 未启用，报告记录解析方法并注明它不是完整 lock。版本不满足声明或必需依赖元数据无法解析时标 `Unknown`。统计范围、验收验证及限制见 [票据 05](../.scratch/poc-0-b2-matched-remeasure/issues/05-maintenance-proxy-metrics.md) 与 [POC README](../poc/poc0-benchmark/README.md)；代理值不参与判定。

2026-09-28 POC-0 票据 10 已实现 B3 基础对照：共享 fixture 的 29 个 bar event 按日期/symbol 顺序投递给 Rust Native 与 PyO3 Python `on_bar()`；逐事件比较决策日期、symbol 和目标列表，同日错误 bar 不会通过。Python S1 targets 进入同一个 Rust Fast Event 账户路径后，与独立预期对拍逐单/逐日现金持仓、成本、NAV 和 PortfolioResult。Python 3.12.2 / PyO3 0.29.0 的 release 五样本已记录，复跑命令、provenance 和构建资源见 [B3 ticket 10](../.scratch/poc-0-benchmark/issues/10-pyo3-basic-callback-comparison.md)。该 3 ETF × 10 session 样本太小，吞吐/规模边界结论仍 `unresolved`；不是 Python 研究包或生产策略接口。

2026-09-28 POC-0 票据 11 已由用户验收并标记 `resolved`：S2 Momentum Rotation、S3 MA20/60 的 Rust Native、Python per-bar、Python batch 对拍与并行测量均完成。固定 golden 和 64×252 合成负载正确性均通过；最终 release 上预登记两倍 callback 延迟与 80% Runs/s 门槛未满足，Python per-bar 和 batch 对该目标负载结论为 `reject`。该结论只适用于测量负载/本机，不是通用 Python 或 GIL 结论；RSS 为包含全部候选的共享进程高水位。详细门槛、原始样本、provenance 与复跑方式见 [B3 ticket 11](../.scratch/poc-0-benchmark/issues/11-pyo3-strategies-and-parallel-boundary.md) 与 [POC README](../poc/poc0-benchmark/README.md#b3-real-strategies-and-parallel-boundary-ticket-11)。

2026-09-27 POC 构建资源边界已实现：默认应用仍启用 `app` feature；使用 `--no-default-features` 的 benchmark 依赖图排除 DuckDB 和仓库数据库 crate，并复用 workspace `target/`。warm-cache dev/release 构建及空间增量见 [原始记录](../poc/poc0-benchmark/README.md#shared-lightweight-poc-build-path)；cold build 未测，未清理缓存。测量时按 10 GiB 保留线执行闸门。

用户已确定新主路线为 Rust-first、多市场、三级回测引擎与未来实盘的平台；现阶段先做 [`POC-0`](poc-0-benchmark-spec.md)。完整目标见根目录 [`ARCHITECTURE.md`](../ARCHITECTURE.md)，新交付顺序见 [`product-roadmap.md`](product-roadmap.md)。目前没有 Vector / Fast Event / Accurate 三级生产实现、生产 Nautilus Adapter、持久 Factor Cache、跨市场账户、生产 Python/PyO3 策略入口或 Live Runtime。POC-0 票据 08 的固定合成 fixture 已通过 Adapter 项目订单、Fill、账户与成本对拍；Nautilus 原生订单仅提交 3 笔，Rust 参考含停牌拒单共 4 次尝试，原生生命周期仍 `unresolved`；票据 08 的早期吞吐快照也仍是探索性记录，正式 B2 吞吐结论以顶部 Issue #3 注册测量为准。现有 React 页面是已实现资产，新平台 MVP 不以 Web 为验收项。以下扫描记录描述旧 A 股纵向场景的代码与数据证据，不能升级为新目标能力。

POC-0 工作范围与验收已发布为本地 [`ready-for-agent` spec](../.scratch/poc-0-benchmark/spec.md)。独立 [`poc/b1-layout`](../poc/b1-layout/README.md) 标量读取 smoke 只对拍三种布局的动量；票据 02 的 SoA、票据 03 的 Arrow 完整 S2 候选和票据 04 的 Polars 候选已验收。票据 05 的版本化 Parquet 写入/复读、列/row group 裁剪与三候选对拍已由用户验收：10,000,029 行、404,509,661 字节的源文件在本机 Linux VM 以 Docker 256 MiB cgroup 限制成功复读，子进程 RSS 58,933,248 字节，容器峰值触及 256 MiB、无 OOM。macOS `RLIMIT_AS` 失败记录仍保留。票据 06 已完成确定性 64 × 252 合成面板的 S2 参数扫描与冷热因子缓存对比，正确性和 checksum 通过；Custom SoA 结论为 `defer`，miss/hit 两线程吞吐相对最佳替代分别 +8.99%/+0.32%，未达到 20% 采纳门槛。报告中的全进程 RSS 为 107,905,024 字节；不代表真实市场/PIT。自研 Fast Event 仍只有票据 07 的最小原型；Nautilus Adapter 的固定 S1 对拍见票据 08。票据 10 基础 PyO3 callback comparison 与票据 11 S2/S3、GIL 并行边界测量均已由用户验收；票据 09 原归档数据因转换边界、并行方式及 RSS 范围不同，独立选型结论仍 unresolved，正式 B2 注册负载和模式的结论以顶部 Issue #3 为准。

2026-09-27 票据 09 的 POC 新增 Fast Event / Nautilus S2 Momentum Rotation 与 S3 MA20/60 固定合成输入对拍；共同子集的信号、Fill、现金、持仓、成本、逐日 NAV 全部通过。S2 的停牌项目拒单与原生提交计数按 ADR 0012 单列，不参与决策。S3 的 130 个交易日合成输入及独立 MA20/60 预期已版本化。两线程 Rust 与两进程 Nautilus 的原始样本、峰值 RSS、warm-cache release 构建资源记录见 [票据 09](../.scratch/poc-0-benchmark/issues/09-b2-strategy-parity-and-throughput.md)；转换和初始化、并行方式及 RSS 范围不同，不能据现有吞吐数字做引擎选型，结论为 `unresolved`。仍非生产 Fast Event / Nautilus Backend 或真实市场业绩验证；B3 多策略/并行比较尚未开始。

2026-09-27 票据 05 最初在依赖解析阶段受阻；现以最小本地 `polars-io` vendor 补齐离线 Parquet 路径。实现、资源实测、macOS 受限内存启动失败与随后 Linux cgroup 复读通过的证据见 [票据 05](../.scratch/poc-0-benchmark/issues/05-parquet-and-out-of-core.md)。

2026-09-27 实现与验收：票据 04 Polars 已由用户确认并标记 `resolved`。`--candidate polars` 通过 Polars lazy expressions 计算加权因子评分、排序、TopK、按实际入选数量均分的目标权重及下一交易日组合收益；正确性通过后才测量。报告保留五个原始样本、阶段计时、DataFrame 构造时间和投影 checksum。固定 3 ETF × 10 日 fixture 仅验证语义与 CLI，不支持布局决策。当前工作树也包含此前已有的 Arrow 候选改动，最终提交须保留其独立归属；验证状态以本轮 HANDOFF 为准。

POC-0 票据 01 已验收：新增固定 3 ETF × 10 个交易日的版本化 fixture、独立排名/信号/事件账本金标准，以及 correctness-first 的 `benchmark-poc0` CLI。金标准通过后会记录 Rust 参考回测的原始耗时；不通过时输出差异报告并跳过计时。机器/代码/数据 provenance 已进入报告；该票据尚未测峰值 RSS，报告将其明确标为未知。这不是 B1 完整布局比较；当前实现位置和复跑方式见 [`POC-0 harness`](../poc/poc0-benchmark/README.md)。2026-09-27 复验：全工作区 83 项通过、1 项按既定条件跳过，格式、Clippy 与 release CLI 均通过。

POC-0 B2 票据 07 已于 2026-09-27 复核验收并标记 `resolved`：最小 Fast Event S1 Buy & Hold 原型通过固定输入与变体 fixture 检查停牌拒单/重试、缺 bar 沿用估值、末日未执行目标、逐日 NAV 恒等式、重复账本 checksum 和固定成本（包括非零买入税）；报告分列初始化、事件处理和端到端五次耗时。独立手算金标准逐日现金/持仓/NAV 与非零买入税变体均与报告数值一致。它仍不是完整 Fast Event Engine，也只有微型合成样本，因此不能支持性能选型。

POC-0 已按获确认的拆分发布为 [13 张本地执行票据](../.scratch/poc-0-benchmark/issues/01-fixed-dataset-and-benchmark-entry.md)。01–07 为 `resolved`；08 为 `unresolved`；03 的验收限于 Arrow 候选实现与正确性，release 结论由票据 06 的同条件 sweep 补齐。06 对本轮合成目标负载得出 `defer`，不是平台级布局裁决。07 是独立验收过的最小原型，不是完整 Fast Event。08 的 Nautilus 2.0.0rc5 固定场景中，Adapter 合成拒单与 Rust 项目事件对齐，但原生 Nautilus HALTED 拒单未验证；七项比较中六项通过、一项原生订单生命周期不一致。五次重复样本只支持内部稳定性，不支持跨引擎吞吐裁决。预检、原失败与复核证据见 [POC harness](../poc/poc0-benchmark/README.md#b2-nautilus-adapter-comparison-ticket-08)。

完整的问题排序、解决路径及验收判据见 [`priorities.md`](priorities.md)。这里保留按领域归类的事实状态。

## Current Phase

MVP-0 数据层现有 D7 instruments、bars、sessions 的 Arrow schema、Domain/RecordBatch 双向转换和 Parquet 读写；写入固定 ZSTD level 3、65,536 row group，记录规范 DSV 并校验主键按存储值字节序严格递增；读取校验表名、schema version 与字段定义。Bar 反序列化需调用方提供 Session 以验证区间。MVP-1 M3 已增加 D7 `execution_status` schema v1、行类型、Arrow 双向转换与通用 Parquet 读写，主键为 `(instrument_id, session_date)`，`available_at` 可空；无记录表示 UNKNOWN 且不可交易。MVP-1 M4 的 `synthetic-etf-daily` v3 已将状态接入 Normalizer、publish 和 rebuild：每个标的×calendar session（含缺 bar 日）一行，override 覆盖默认，按 fixture timezone 转换 `available_at`；可选 `opens` 使用逐日 open 计算 high/low。发布 manifest 含排序后的四张表；v1/v2 的输出与 fixture DSV 保持不变。`prajna-data` 按 `(id, version)` 注册 Normalizer，Raw 数值字面量直接解析为 scale-18 定点数，按 D6 的 `normalized/<table>/<dsv>/` 布局发布，以 manifest 标识已提交版本；同 DSV 写入由文件锁串行化。`rebuild_dataset` 可由 manifest 与不可变 Raw 重算并逐字段验证 manifest core，但不发布 Parquet。MVP-0 契约（身份、定点、切日、Raw 布局、DSV/逻辑 hash）与对应实现/测试位置见 [`ADR 0015`](decisions/0015-mvp-0-data-contracts.md)；schema 7 发布链未改，字段映射见 [`schema-7-publish-manifest-mapping.md`](contracts/schema-7-publish-manifest-mapping.md)。验证层次：`cargo fmt --all -- --check`、`cargo clippy -p prajna-data --all-targets --locked --offline -- -D warnings`、`cargo test -p prajna-data --locked --offline` 均通过；v3 的 30 行状态、两条 override、bars 逻辑 hash、幂等发布与删除 `normalized/` 后 rebuild 有 Rust 集成测试。Full workflow 的 `pyarrow-interop` job 仍用 v1 发布三表并校验 schema、metadata、行数与抽样数值（出现跳过即失败，[运行 36880462840](https://github.com/ZombieIce/PrajnaQuant/actions/runs/36880462840) 通过）；以上全部只用合成 fixture。未实现：Crypto 日线 UTC→`+08:00` 重采样的拒绝规则、Python 端逻辑 hash、真实数据 Normalizer、已发布快照导入、分区、quarantine、CLI。该能力不证明真实市场数据、PIT 或量化收益。

此前 A 股股票/ETF 日频 + Web 的初级产品路线已归档到 [`legacy-ashare-roadmap.md`](legacy-ashare-roadmap.md)；当前仍是本地 ETF Rotation MVP 加 A 股仓库。Batch 2 已有显式证券增量同步/失败恢复/不可变发布代码，隔离股票+ETF 真实小样本通过；已发布快照的只读证券/日线 API 和 K 线页面在合成目录通过。Batch 3 的日更身份导入与 ETF 诊断作业已有代码；生产同步/调度、真实状态、可信历史作业、Python 研究包和远程认证部署仍未验。五 ETF 真实状态覆盖不足，旧实验仍 `legacy_bar_only`，新 `status_gated` 占位跑全 UNKNOWN，P0-3 开放；股票回测没有开放。

### MVP-1 D7 面板读取

`prajna-research` 的 `load_panel` 已能校验 DSV manifest 并读取 schema v1 的 instruments、sessions、bars，按 Venue 生成有序 Session/Instrument 列表及完整 instrument × session Polars 网格。缺 bar 保持价格 null 且 `has_bar=false`；`close_available_at` 保留源 available_at 的 UTC 纳秒值，不推定为 close。当前面板验证限于 `synthetic-etf-daily` v1 的 3×10 合成 fixture。`factor` 模块已提供四种版本化定义、强类型参数校验、dependency-first 去重 DAG（含注入环测试）、status/availability 字符串与 Factor Values Arrow schema v1；canonical JSON 的有限 f64 权重用 16 位小写 IEEE-754 hex 保存，明确区分正负零。定义模型与身份有参数差异、非法输入和 schema 测试；尚无因子数值计算、持久缓存或 Vector 执行能力，也不证明真实数据或 PIT。

`prajna-research` 现提供 `StaticUniverse`：成员排序去重后以 restricted-JCS SHA-256 身份存储，原子发布、幂等复用并在读取时校验身份；可对照 DSV v1 instruments 表报告缺失成员。3×10 fixture 已验证 A/B/C 通过、含 D 失败。它是固定成员列表，不提供 point-in-time 成员可知性证明。

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
- 通用 Factor Registry/版本/持久缓存、因子值分布统计和相关矩阵；独立订单流水/可查询持仓时序 API；统一策略 ID/版本。回测报告内已有逐日持仓快照，但尚无独立持仓查询接口。MVP-1 M13 的独立标准库因子值/status、排名与 Vector 组合结果金标准已覆盖 3×10 和 64×252 v2（含 trend20）合成 fixture（见 `poc/mvp1-golden/`）；这不是 PIT 或真实市场数据验证。
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
