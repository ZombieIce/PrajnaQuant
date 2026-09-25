# 本地日频同步与发布操作

状态：2026-09-25，Batch 2 的本地显式证券同步操作说明；生产调度未启用，远程服务器部署/认证仍未交付。完整验收范围见 [`Batch 2 集成验收`](handoffs/batch2-integration-acceptance.md)。

## 输入与单写者

`ashare-warehouse` 用 `<data-dir>/.writer.lock` 排他写 DuckDB。生产回填进程是否仍持锁为 Unknown；运行任何生产同步前，应先从实际进程/锁确认没有另一个 writer。同步名单必须是显式 `sh`/`sz` 六位代码；`configs/daily-sync.symbols.example` 是示例，不代表已核验股票或 ETF 目录。不要把当前观察集合当历史全市场。

隔离验证用独立目录：

```sh
cargo run -p ashare-warehouse --locked --offline -- --data-dir /private/tmp/pq-daily-check init
cargo run -p ashare-warehouse --locked --offline -- --data-dir /private/tmp/pq-daily-check fetch-calendar --year 2026 --month 9
cargo run -p ashare-warehouse --locked --offline -- --data-dir /private/tmp/pq-daily-check sync-daily --symbol sh600519 --symbol sh510300 --start 2026-09-24 --end 2026-09-24 --lookback-days 3 --retries 2
```

后两条需要外部来源，`--offline` 只禁止 Cargo 下载依赖，不使行情请求离线。同步重取修订回看窗口；相同内容 hash 不增有效修订。失败任务记录 `ops.daily_sync_job/attempt`；成功入库但覆盖审计失败，可补齐日历/行情后使用 `publish-sync --job-id <UUID>` 重做发布。本任务有 `EMPTY` 来源响应时，即使库中已有旧 bar，也不能把该任务发布为 `complete`，应重新同步取得非空来源证据。`sync-daily-latest` 仅在本机上海时间 18:30 后允许当天开市日，官方 SZSE 日历缺失/过旧时停止；非完整行情不得成为默认发布快照。`complete` 只指请求的显式证券×已确认开市日价格覆盖，不代表历史状态、PIT 或全市场。

## 只读查询与固定版本

同步发布 `snapshots/<snapshot_id>/` 的 Parquet、证券目录、日历与 manifest，校验后原子替换 `snapshots/current.json`；旧目录保留。研究 API 在请求开始读取该指针并固定 ID/hash，翻页继续传原 `snapshot_id`：

```sh
cd apps/web && npm run build && cd ../..
cargo run -p quant-research --locked --offline -- serve --output research-output --market-data-dir data-core --web-dist apps/web/dist --address 127.0.0.1:7878
```

本地入口 `/market`；API 为 `GET /api/v1/instruments` 与 `GET /api/v1/daily-bars`。无发布指针会返回不可用；证券目录无主键的隔离样本不会被猜成股票。股票行情可查不代表股票回测开放。服务仍只绑定 loopback，未提供远程认证。

## macOS 调度模板（未安装）

1. 复制 `configs/daily-sync.symbols.example` 为被忽略的 `configs/daily-sync.symbols`，人工审查小规模名单与输出目录。
2. 在没有其他 writer 的隔离/生产窗口，先手工运行 `sh scripts/daily_sync_schedule.sh` 并审阅任务 JSON 与快照 hash。
3. `sh scripts/install_daily_sync_launchd.sh` 仅生成 `~/Library/LaunchAgents/org.prajnaquant.daily-sync.plist`，不加载；模板设置本机时区周一至周五 19:30。检查 `ProgramArguments`、日志路径和名单后再由运维人员决定是否 `launchctl bootstrap`。

本批没有执行第 1–3 步的生产操作，也没有安装/启用 LaunchAgent。失败告警、备份恢复、服务器调度、认证/HTTPS 尚未验收；不能把模板存在写成“自动日更已上线”。
