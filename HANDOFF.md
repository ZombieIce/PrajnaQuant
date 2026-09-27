# Agent Handoff — 新平台目标基线与 POC-0

日期：2026-09-26。用户确认 Rust-first 多市场平台路线取代此前 A 股日频 + Web 初级产品交付顺序。目标决策见 [`ARCHITECTURE.md`](ARCHITECTURE.md)，新路线见 [`docs/product-roadmap.md`](docs/product-roadmap.md)，领域词汇见 [`CONTEXT.md`](CONTEXT.md)，POC-0 工作范围与验收见 [本地 spec](.scratch/poc-0-benchmark/spec.md)，执行规范见 [`docs/poc-0-benchmark-spec.md`](docs/poc-0-benchmark-spec.md)，目标 ADR 为 0009–0011。旧 A 股路线保存在 [`docs/legacy-ashare-roadmap.md`](docs/legacy-ashare-roadmap.md)。这些是目标/试验文档，**不是三级引擎、Nautilus 或 Python 入口的已实现证据**；现有功能与量化 P0 风险仍以 `docs/STATUS.md` 及下方 Batch 2 交接为准。

## 当前交接（2026-09-27）

本轮尝试执行 POC-0 票据 05。未写源码：离线 Cargo 依赖解析启用 Polars Parquet 时分别缺少 `brotli` 与 `polars-sql` 索引元数据；联网 Cargo 无法解析 `mirrors.ustc.edu.cn`。构建资源 recorder 在依赖图阶段退出，未开始编译；当时可用空间约 13 GiB，按 2 GiB 上界满足 10 GiB 保留线。证据与建议复跑条件见 [票据 05](.scratch/poc-0-benchmark/issues/05-parquet-and-out-of-core.md)。**唯一建议下一步：**恢复 Cargo mirror/索引可用后，从票据 05 的依赖图与构建资源记录重试，再进行 Parquet 三候选与受限内存分块验收。

2026-09-27 构建资源决策（票据 13 完成前的交接记录）：用户确认先优化 POC-0 spec/票据，再由新 [票据 13](.scratch/poc-0-benchmark/issues/13-build-resource-boundary.md) 实施轻量构建边界。现场只读清点：根 `target/` 约 58 GB、独立 B1 target 约 1 GB、卷可用约 14 GiB，重复的 bundled DuckDB 构建产物是主要占用。新增构建须开工及预计结束均保留至少 10 GiB；记录增量/release 构建耗时与产物增量，冷构建仅在预算允许时测。票据 03/04 均已由用户确认验收。不得自动清空整个 target。本轮未清理文件。后续票据 13 已完成，当前票据 05 状态见上方交接。

2026-09-27 票据 04 已由用户确认验收并标记 `resolved`：`benchmark-poc0 --candidate polars` 使用 Polars lazy expressions 从压缩的 observed-close 序列计算短/长动量与样本波动率、复合分数、确定性排序、TopK/等权目标和简化组合收益；权重分母按实际入选数量计算。独立 CLI 用例覆盖 4 观察期首个有效日及手算分数、符号零平局、固定 fixture 手算收益、缺少次日 bar 返回 null、少于 top_n 时实际入选数量均分，以及错误/变体输入跳过计时。release 原始报告保存在 [`Polars results`](poc/poc0-benchmark/results/b1-polars-2026-09-27.json)。此前复验：`cargo test --workspace --locked --offline`（89 passed、1 ignored）、`cargo clippy --workspace --all-targets --locked --offline -- -D warnings`、`cargo fmt --all -- --check` 均通过。第一次 release 构建因磁盘空间不足中断；只清理了当时创建的临时 Cargo target 后，release 构建和运行成功。当前工作树还含 Arrow 未提交改动；两票代码尚未提交。固定 3×10 样本不支持布局/性能决策。票据 03 此后亦由用户确认验收；票据 13 之后完成，当前待办更新见上方。

提交已按领域拆分：旧 A 股状态取证 `e8764e1`、日更与身份导入 `8e8bff9`、诊断作业与页面 `c7906ab`；Agent 工具配置 `d8fab3a`；新平台架构与旧路线归档 `15cc6d5`；POC-0 计划 `20eb209`、B1 标量 smoke `e8c3bd8`、01 已验收 harness `6401651`、07 最小事件原型 `a907212`、02 SoA 候选 `d78df42`。各提交仅含所属批次文件，未推送远端。

当前 POC 状态：01–04 已验收；03 Arrow 为用户确认的实现验收，release 性能样本仍缺。Parquet 扫描、内存与 out-of-core 对比尚缺，B1 布局仍 Unresolved；07 是最小事件原型，不能支持 Fast Event 选型。Nautilus、PyO3 和真实 ETF 历史可信度门槛均未完成。所有已记录结果均来自固定 3×10 合成样本，不构成性能/架构决策证据。

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
