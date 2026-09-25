# Agent Handoff — Batch 2 集成后

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
