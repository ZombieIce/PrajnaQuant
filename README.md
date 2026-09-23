# Prajna Quant / A 股量化

当前阶段：本地 ETF Rotation MVP 与 A 股行情仓库。**研究结果尚不能视作无偏历史业绩**；已确认的数据池与收益口径限制见 [STATUS](docs/STATUS.md)。项目关联的 GitHub 仓库是 [ZombieIce/PrajnaQuant](https://github.com/ZombieIce/PrajnaQuant)；代码、配置与文档保存在 Git，真实行情库和回测产物仍保留在本地。

## 从这里开始

新 Agent 按 [AGENTS.md](AGENTS.md) 的顺序阅读；当前工作与下一步看 [HANDOFF.md](HANDOFF.md)，问题排序与解决路径看 [priorities.md](docs/priorities.md)。架构、时间、数据、因子、策略、引擎与 API 的实际状态分别在 `docs/` 同名文档。

## 实际组成

| 路径 | 当前作用 |
| --- | --- |
| `src/`, `sql/schema.sql` | Rust `ashare-warehouse`：采集、版本化 DuckDB、Parquet 导出 |
| `crates/quant-research/src/` | Rust `quant-research`：快照、ETF 因子研究、日频回测、参数网格、只读 Axum API |
| `apps/web/` | React + TypeScript + ECharts 的本地只读展示 |
| `warehouse.py`, `tests/test_warehouse.py` | 早期 Python 行情入库原型及其测试；无 Python 策略/回测引擎 |
| `configs/` | 单次实验和网格 JSON 示例 |
| `data-core/`, `research-output/` | 本地数据与结果，均被 `.gitignore` 排除；新环境需自行准备合格数据 |

没有 `pyproject.toml`、`requirements.txt`、notebooks、CI 配置、迁移脚本或服务端写入 API。`sql/schema.sql` 为当前 schema 定义，`ops.schema_version` 记录 1–4；不能把四行版本号当成可独立执行的迁移文件。

## 本地运行

需要 Rust（manifest 最低版本 1.85.1，扫描环境 1.98.1）、Node/npm、已有的 DuckDB 仓库与历史 ETF/沪深300数据。研究快照要求至少 1000 条沪深300日线；空环境运行 `run` 会失败，这是输入门槛。仓库使用项目 `.cargo/config.toml` 的镜像配置。

```bash
cargo run -p quant-research --release -- run --warehouse data-core/market.duckdb --output research-output --config configs/etf_momentum_60.json
cargo run -p quant-research --release -- run-grid --warehouse data-core/market.duckdb --output research-output --config configs/etf_rotation_smoke_grid.json
cargo run -p quant-research --release -- signal-research --warehouse data-core/market.duckdb --output research-output --signal momentum_60
cd apps/web && npm ci && npm run build && cd ../..
cargo run -p quant-research --release -- serve --output research-output --web-dist apps/web/dist --address 127.0.0.1:7878
```

`run-grid` 生成的轮动配置与 `run` 示例的单动量配置不是同一策略。`serve` 仅从文件读取已保存结果，不触发回测。完整 CLI、接口与验证命令见 [API](docs/api.md) 和 [HANDOFF](HANDOFF.md)。旧设计文档在 `docs/` 保留作背景，状态冲突以当前代码、测试和本组审计文档为准。
