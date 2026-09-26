# Prajna Quant / A 股量化

项目主目标已更新为 **Rust-first 多市场研究、并行回测与未来实盘平台**；设计基线见 [ARCHITECTURE](ARCHITECTURE.md)，交付顺序从 [POC-0](docs/poc-0-benchmark-spec.md) 开始，见[产品路线](docs/product-roadmap.md)。当前代码仍是本地 ETF Rotation 场景与 A 股行情仓库，并有 React 网页；新平台 MVP 不以 Web 为验收项。Batch 2 已实现显式证券增量同步/恢复、不可变日频发布、只读证券/日线 API 和股票/ETF K 线页面；同步仅在隔离真实小样本验证，页面在合成快照验证，生产日更调度尚未启用。Python 研究包与远程认证服务仍缺。真实五 ETF 旧实验是 `legacy_bar_only`；新全 UNKNOWN 状态占位跑只证明拒单门槛，P0-3 未关闭。股票回测尚未开放。**研究结果尚不能视作无偏历史业绩**；证据见 [Batch 2 验收](docs/handoffs/batch2-integration-acceptance.md) 和 [STATUS](docs/STATUS.md)。代码、配置与文档保存在 Git，真实行情库和回测产物仍保留在本地。

## 从这里开始

新 Agent 按 [AGENTS.md](AGENTS.md) 的顺序阅读；先区分[目标架构](ARCHITECTURE.md)、[产品路线](docs/product-roadmap.md)与已实现状态。当前工作与下一步看 [HANDOFF.md](HANDOFF.md)，既有 A 股正确性问题看 [priorities.md](docs/priorities.md)。架构、时间、数据、因子、策略、引擎与 API 的实际状态分别在 `docs/` 同名文档。

## 实际组成

| 路径 | 当前作用 |
| --- | --- |
| `src/`, `sql/schema.sql` | Rust `ashare-warehouse`：采集、版本化 DuckDB、Parquet 导出 |
| `crates/quant-research/src/` | Rust `quant-research`：快照、ETF 因子研究、日频回测、参数网格、结果查询与本地 Universe 管理 API |
| `apps/web/` | React + TypeScript + ECharts 报告和行情界面；Universe 管理、已保存结果筛选及股票/ETF K 线连接本地 API；新运行因服务端作业 API 缺失而禁用 |
| `warehouse.py`, `tests/test_warehouse.py` | 早期 Python 行情入库原型及其测试；无 Python 策略/回测引擎 |
| `configs/` | 单次实验和网格 JSON 示例 |
| `data-core/`, `research-output/` | 本地数据与结果，均被 `.gitignore` 排除；新环境需自行准备合格数据 |

没有 `pyproject.toml`、`requirements.txt`、notebooks 或 CI 配置。`sql/schema.sql` 定义基础 schema，`sql/migrations/` 保存可幂等重放的升级；指数成分底座当前暂存第三方沪深300历史区间数据，公告时间为按有效日提前 14 天模拟，成员状态为 `unknown`、覆盖为 `unverified`，不能用于严格 PIT 回测。可用 `cargo run -- --data-dir data-core import-index-constitution-csv --input <CSV路径>` 幂等导入。服务提供本地 Universe CRUD；无认证写 API 仅绑定 loopback。

## 本地运行

需要 Rust（manifest 最低版本 1.85.1，扫描环境 1.98.1）、Node/npm、已有的 DuckDB 仓库与历史 ETF/沪深300数据。研究快照要求至少 1000 条沪深300日线；空环境运行 `run` 会失败，这是输入门槛。仓库使用项目 `.cargo/config.toml` 的镜像配置。

```bash
cargo run -p quant-research --release -- run --warehouse data-core/market.duckdb --output research-output --config configs/etf_momentum_60.json
cargo run -p quant-research --release -- run-grid --warehouse data-core/market.duckdb --output research-output --config configs/etf_rotation_smoke_grid.json
cargo run -p quant-research --release -- signal-research --warehouse data-core/market.duckdb --output research-output --signal momentum_60
cd apps/web && npm ci && npm run build && cd ../..
cargo run -p quant-research --release -- serve --output research-output --market-data-dir data-core --web-dist apps/web/dist --address 127.0.0.1:7878
```

`run-grid` 生成的轮动配置与 `run` 示例的单动量配置不是同一策略。`serve` 可读取已保存结果、管理本地 Universe，并从 `--market-data-dir` 的已发布不可变快照服务行情；不会触发回测。无发布指针时行情 API 返回不可用。日更入口与未启用的调度模板见 [本地同步操作](docs/daily-sync-operations.md) 和 [Batch 2 B 交接](docs/handoffs/batch2-b-daily-sync.md)；完整接口、Universe 契约与验收见 [API](docs/api.md)、[Universe 契约](docs/universe-contract.md)、[Batch 2 验收](docs/handoffs/batch2-integration-acceptance.md) 和 [HANDOFF](HANDOFF.md)。旧设计文档在 `docs/` 保留作背景，状态冲突以当前代码、测试和本组审计文档为准。
