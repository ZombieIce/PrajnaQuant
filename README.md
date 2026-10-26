# Prajna Quant / A 股量化

项目目标是 **Rust-first 多市场研究、并行回测与未来实盘平台**。当前已形成本地研究与回测底座：版本化数据、Factor/Cache、Experiment/Run、Vector/Fast Event、受限 Nautilus Accurate Backend，以及 Python/PyO3 Decision 入口；旧 A 股仓库、ETF 回测与 React 界面继续保留。

MVP-6 已按[负责人裁决](https://github.com/ZombieIce/PrajnaQuant/issues/182#issuecomment-6406038887)**有保留签收并收口**：[第二轮六项工程复核](https://github.com/ZombieIce/PrajnaQuant/issues/235#issuecomment-6400855766)全部 pass，证据处理流程 fail 与第一轮 fail 保留。真实验证仅限固定名单 ETF 工程场景，不证明完整历史 Universe、无偏策略业绩或实盘正确性。固定名单幸存者偏差、UNKNOWN 收窄、首笔成交及同 Session 资金先后、终止估值和真实未触发路径限制详见 [STATUS](docs/STATUS.md)。真实数据和详细结果仅留本机。

设计方向见 [ARCHITECTURE](ARCHITECTURE.md)，阶段与后续边界见[产品路线](docs/product-roadmap.md)。新平台不以 Web 为 MVP 验收项；多市场、Paper/Live 和统一新实验网页工作流仍是后续目标。

## 从这里开始

新 Agent 按 [AGENTS.md](AGENTS.md) 的顺序阅读；先区分[目标架构](ARCHITECTURE.md)、[产品路线](docs/product-roadmap.md)与已实现状态。当前能力与收口范围以 [STATUS](docs/STATUS.md) 和 GitHub 最新裁决为准；[HANDOFF.md](HANDOFF.md) 是历史交接入口。[priorities.md](docs/priorities.md) 及旧系统文档保留其 A 股路径适用范围。

MVP-5 的 Python Decision S2/S3 64×252 合成吞吐、分段耗时、As-of View 成本与 RSS，见[可复跑测量及 B3 并列表](poc/mvp5-benchmark/README.md)。只记录、不设门槛；不推翻 B3 `reject`，不构成生产吞吐承诺。

## 实际组成

| 路径 | 当前作用 |
| --- | --- |
| `crates/prajna-domain`, `prajna-data` | scale-18 Domain、不可变 Raw、版本化 Dataset、Arrow/Parquet 与重建校验 |
| `crates/prajna-research`, `prajna-account` | Factor/Cache、策略/Vector/Fast Event、定点账户与公司行动 |
| `crates/prajna-adapter`, `prajna-experiment` | 受限 Accurate Backend、Run 级并行、身份、重放/diff 与分级结果 |
| `crates/prajna-py`, `python/prajna`, `pyproject.toml` | 本地 Python 3.12+ 包、受限 As-of Decision、共用 Rust 执行及 Arrow 读取 |
| `src/`, `sql/schema.sql` | Rust `ashare-warehouse`：采集、版本化 DuckDB、Parquet 导出 |
| `crates/quant-research/src/` | Rust `quant-research`：快照、ETF 因子研究、日频回测、参数网格、结果查询与本地 Universe 管理 API |
| `apps/web/` | React + TypeScript + ECharts 报告和行情界面；Universe 管理、已保存结果筛选及股票/ETF K 线连接本地 API；新运行因服务端作业 API 缺失而禁用 |
| `warehouse.py`, `tests/test_warehouse.py` | 早期 Python 行情入库原型及其测试；与新 `prajna` 包分开 |
| `configs/` | 单次实验和网格 JSON 示例 |
| `data-core/`, `research-output/` | 本地数据与结果，均被 `.gitignore` 排除；新环境需自行准备合格数据 |

本地 Python 构建由根 `pyproject.toml`/maturin 管理，CI 见 `.github/workflows/`；Nautilus Backend 使用独立锁定环境。旧 DuckDB 路径中，`sql/schema.sql` 定义基础 schema，`sql/migrations/` 保存可幂等重放的升级；指数成分底座当前暂存第三方沪深300历史区间数据，公告时间为按有效日提前 14 天模拟，成员状态为 `unknown`、覆盖为 `unverified`，不能用于严格 PIT 回测。可用 `cargo run -- --data-dir data-core import-index-constitution-csv --input <CSV路径>` 幂等导入。服务提供本地 Universe CRUD；无认证写 API 仅绑定 loopback。

## 新平台入口

- [Experiment CLI](crates/prajna-experiment/README.md)：定义、参数扫描、Summary/Standard/Full、重放、提升和差异比较。
- [Python 构建与研究 API](crates/prajna-py/README.md)：Python 3.12 venv、maturin、Arrow 读取与 Decision 回调。
- [Nautilus Backend](backends/nautilus/README.md)：独立环境与支持范围；MVP-6 PIT 路径不接 Accurate。
- [本机工程验收器](scripts/mvp6_acceptance.py)：固定冻结输入验收；新环境须另行准备合格本机输入，Git 仓库不附带真实数据。

## 旧 A 股 / Web 本地运行

需要 Rust（manifest 最低版本 1.85.1，扫描环境 1.98.1）、Node/npm、已有的 DuckDB 仓库与历史 ETF/沪深300数据。研究快照要求至少 1000 条沪深300日线；空环境运行 `run` 会失败，这是输入门槛。仓库使用项目 `.cargo/config.toml` 的镜像配置。

```bash
cargo run -p quant-research --release -- run --warehouse data-core/market.duckdb --output research-output --config configs/etf_momentum_60.json
cargo run -p quant-research --release -- run-grid --warehouse data-core/market.duckdb --output research-output --config configs/etf_rotation_smoke_grid.json
cargo run -p quant-research --release -- signal-research --warehouse data-core/market.duckdb --output research-output --signal momentum_60
cd apps/web && npm ci && npm run build && cd ../..
cargo run -p quant-research --release -- serve --output research-output --market-data-dir data-core --web-dist apps/web/dist --address 127.0.0.1:7878
```

`run-grid` 生成的轮动配置与 `run` 示例的单动量配置不是同一策略。`serve` 可读取已保存结果、管理本地 Universe，并从 `--market-data-dir` 的已发布不可变快照服务行情；不会触发回测。无发布指针时行情 API 返回不可用。日更入口与未启用的调度模板见 [本地同步操作](docs/daily-sync-operations.md) 和 [Batch 2 B 交接](docs/handoffs/batch2-b-daily-sync.md)；完整接口、Universe 契约与验收见 [API](docs/api.md)、[Universe 契约](docs/universe-contract.md)、[Batch 2 验收](docs/handoffs/batch2-integration-acceptance.md) 和 [HANDOFF](HANDOFF.md)。旧设计文档在 `docs/` 保留作背景，状态冲突以当前代码、测试和本组审计文档为准。
