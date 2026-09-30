# Agent Handoff

本文件是入口；本轮票据交接与验证细节见对应 PR。

## 当前交接在哪里

- 进行中的票据：`gh issue list -R ZombieIce/PrajnaQuant --state open`，用 `gh issue view <N> --comments` 读取。
- 已实现内容、验证命令、环境限制、开放问题与唯一建议下一步：对应 PR 描述（`gh pr list -R ZombieIce/PrajnaQuant`）。
- Issue #16 当前交接：[PR #47](https://github.com/ZombieIce/PrajnaQuant/pull/47)；Independent review: see PR #47（待复核）。
- review 发现与结论：PR 评论。
- 项目当前能力、风险与量化口径：[`docs/STATUS.md`](docs/STATUS.md)。
- 逐票历史（截至 2026-09-30，含 POC-0、MVP-0、Batch 2 集成结论、验证记录）：[`docs/handoffs/poc-0-mvp-0-handoff-archive.md`](docs/handoffs/poc-0-mvp-0-handoff-archive.md)；更早的批次交接见 [`docs/handoffs/`](docs/handoffs/)。

## 使用与恢复

```bash
# 隔离或经授权的单写者数据目录；生产 writer 状态不明时不要并发运行
cargo run -p ashare-warehouse --release --locked -- --data-dir data-core sync-daily-latest --symbol sh600519 --symbol sh510300

# 先构建前端，再启动只读服务；行情从已发布快照目录读取，不打开写库
cd apps/web && npm run build && cd ../..
cargo run -p quant-research --release --locked -- serve --output research-output --market-data-dir data-core --web-dist apps/web/dist --address 127.0.0.1:7878
```

`--market-data-dir` 缺省为 `data-core`；无 `snapshots/current.json` 时新行情 API 返回不可用，不退回未发布文件。生产 `data-core/.writer.lock` 存在且实时持有者无法证明，不要写入生产库；真实数据和实验目录被 Git 忽略。

## 何时改本文件

仅当入口指引本身变化（例如恢复命令、文档位置或工作流约定改变）时修改，并按票据流程走独立 PR。
