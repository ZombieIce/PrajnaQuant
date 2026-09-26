# Batch 3 B — 小范围真实日更、审计与发布交接

检查时间：2026-09-26 17:57:50 CST（09:57:50 UTC）；最新生产状态通过 `daily-sync-status` 的只读 DuckDB 连接及既有 `.writer.lock` 共享锁读取；成功取得共享锁表示该读取瞬间没有其他遵守此锁的 writer。OS 不允许读取完整进程清单，故进程身份仍为 Unknown。首次查询状态时该命令尚未有只读分支，使用 `Warehouse::open` 获得排他锁并运行幂等 schema 初始化/迁移（迁移 007）；没有并发 writer，也没有改变行情行，但该首次检查不是只读操作。随后实现只读状态入口并在只读模式再次核验。未删除锁、未停止 writer、未复制活跃 DuckDB。

## 当前任务与回填证据

生产任务表记录截至上述检查时间：

- `observed_market_daily_bar`：29,944 个 SUCCESS 窗口，记录覆盖至 2026-09-24；9,016 个 EMPTY 窗口仍不能视作完整覆盖；另有 1 个 FAILED 窗口。
- 失败历史窗口：`sz300288`，2020-05-19 至 2022-07-27，错误 `error decoding response body`，最后更新 2026-09-24 16:38:08 CST。
- `etf_daily_bar`：4,546 个 SUCCESS、5,498 个 EMPTY 窗口，最新 SUCCESS 截止 2026-09-21。Benchmark 最新 SUCCESS 同为 2026-09-21。
- 本次之前没有 `ops.daily_sync_job` 记录；不是从旧日志百分比推算回填进度。

命令：

```sh
cargo run -p ashare-warehouse --locked --offline -- --data-dir data-core daily-sync-status
```

## 有限证券身份和交易日

只选取沪市一只股票和一只 ETF：

| Symbol | 代码/名称/类型 | 身份来源 |
|---|---|---|
| `sh600519` | 600519 贵州茅台，SH，EQUITY | 上交所发行人披露：[2026-04-17 文件](https://www.sse.com.cn/disclosure/listedinfo/announcement/c/new/2026-04-17/600519_20260417_XK94.pdf) |
| `sh510300` | 510300 沪深300ETF华泰柏瑞，SH，ETF | 上交所证券/基金信息：[510300 页面](https://www.sse.com.cn/assortment/options/disclo/update/c/c_20260116_10805396.shtml) |

通过 `import-security-directory-csv` 单写者入口导入了带逐行来源的目录 CSV：source `sse-official`、run `02efe554-67d1-49a8-9b15-85308fd5e779`、2 行；名字与分类共 4 个版本修订。保留原有 `instrument_id` UUID，不做可能破坏旧引用的 ID 迁移；目录和 API 的现行 ID（按股票、ETF 顺序）分别为 `f2639412-bfc1-422a-a443-8fcc34bafff9` 与 `3159aadb-f604-44d7-ad57-780a1897d9b8`。第三批契约要求 `SH:code` 形式的稳定 ID；当前仓库/快照仍使用原 UUID，此差异需要统筹确定兼容映射或迁移方案，本批没有改动公共身份语义。当前名称/分类已由上交所材料支撑，不代表历史 PIT 身份证明；历史上市/退市日期未填，状态缺口保留 UNKNOWN。

更新了 2026 年 9 月上交所交易日历。当前系统日是 2026-09-26（周六）；日历将 9 月 25 日列为休市、9 月 24 日列为最近已完成交易日，因此当天和 9 月 25 日没有进入最终日线快照。

## 同步、审计与不可变发布

首次生产小样本同步命令：

```sh
cargo run -p ashare-warehouse --locked --offline -- --data-dir data-core sync-daily-latest \
  --symbol sh600519 --symbol sh510300 --lookback-days 5 --retries 2
```

成功 job `e0149cf6-0a91-4fb5-904b-f90db98e03f7`，source Tencent；范围仅为上述 2 个证券 × 2026-09-24。响应 8 行，插入 2 个行情版本、6 行判重、2 次请求成功。审计覆盖结果 `complete` 只对这 2 个证券与这 1 个交易日成立，不延伸至全市场、状态或 PIT 覆盖。

发布 snapshot `ccaff0e2-9871-496a-b09f-1870d165e15c`：

- 截止日：2026-09-24；调整口径 `none`；两证券各 1 行，缺失状态日 `unknown_state_security_days=2`。
- 日线 SHA-256：`0ddf5951b4af2105be20193ca01ffe22e5f5074df8011e7a019d05b140bbb1e7`。
- Manifest SHA-256：`639eaf4f2acf0bf8ec990118fb41a390e37cb50cdc0a1cfdefe19cc1e69eae9f`。
- 目录 SHA-256：`4190000e1b3275393eeb1fcfa777f5ba2dadc09357420af3451254bee51c34c7`；日历 SHA-256：`abc98a5492a7a1e60a74e59c65b110aff02d83941a29ffc2c8b9ca34570d122e`。
- Python 标准库独立复核了 manifest、数据、目录、日历文件 hash 和 `current.json` 引用关系，均相符。旧快照 `b8a40132-07eb-4e30-84ab-f90db8c6a1e7` 保留，未改写其文件。

生产无变化复跑经项目调度 runner 触发：

```sh
sh scripts/daily_sync_schedule.sh
```

记录 job `382a9a68-a62d-44c8-8448-f0c65ef0e639`，尝试 3 次后因 `sh510300: bounded retry limit exhausted` FAILED；响应行和新版本均为 0。底层失败原因没有从本次任务记录中进一步确认。失败没有发布新快照；查询确认 `current` 仍是上面的成功 snapshot，hash 不变。同步实现的隔离单测覆盖首次/重复/修订、失败恢复、有界重试、空响应未知、writer 排他、审计失败和原子发布中断；这些测试不代替本次真实接口复跑通过。

## 行情 API 与 `/market` 页面实测

使用已发布 snapshot 对应服务验证证券搜索与日线 API（先前临时探针测试后已删除临时测试代码），并通过浏览器打开 `http://127.0.0.1:17878/market`：

- 搜索 600519 显示贵州茅台 / SH / 股票；K 线日期 2026-09-24，OHLC 为 1250.01 / 1256.13 / 1231.05 / 1237.00，成交量 3,123,900 股，价格单位 CNY/股，Tencent，采集时间 2026-09-26 17:40:10 CST。
- 搜索 510300 显示沪深300ETF华泰柏瑞 / SH / ETF；OHLC 为 4.578 / 4.579 / 4.512 / 4.515，成交量 710,251,900 份，价格单位 CNY/份，Tencent，采集时间 2026-09-26 17:40:09 CST。
- 两页均展示截止日 2026-09-24、snapshot ID 与 manifest hash。来源未提供成交额，所以成交额保持缺失。
- `npm run build` 在 `apps/web` 通过；页面未修改。

API 读取契约：证券目录搜索返回稳定 `instrument_id`、代码、名称、市场及资产类型；日线查询绑定明确 `snapshot_id`，返回排序后的日期、OHLCV、来源、截止日及 snapshot/manifest 身份。C 的运行任务应固定引用 snapshot ID，不能读取同步中的写库状态。A 状态证据继续走统一单写者导入；缺证据保持 UNKNOWN。

## 调度状态和门槛

`configs/daily-sync.symbols` 当前仅列上述两个证券。`scripts/daily_sync_schedule.sh` 已改为可在 launchd 的非交互 PATH 下解析 `$HOME/.cargo/bin/cargo`，并支持 `CARGO_BIN` 覆盖。`sh -n scripts/daily_sync_schedule.sh scripts/install_daily_sync_launchd.sh` 通过。

计划配置生成器只渲染、不加载 LaunchAgent：工作日 19:30（本机时区）；日志位置为 `data-core/daily-sync.stdout.log` 与 `.stderr.log`；单次命令默认最多 2 次重试；停止方式为 `launchctl bootout gui/$(id -u) ~/Library/LaunchAgents/org.prajnaquant.daily-sync.plist`。但由于上述真实无变化复跑失败，本轮未生成/安装 plist、未 bootstrap/kickstart；`launchctl print` 显示 service not found。因此当前明确为**调度未运行**，也没有观察到计划任务执行。先修复/解释 Tencent ETF 请求失败，再手动重复通过，方可重新评估启用。

## 验证结果及限制

- `cargo test -p ashare-warehouse --locked --offline`：16 passed。
- `cargo test --workspace --locked --offline`：通过；仓库测试共 63 项 quant-research + 16 项 warehouse 通过，另有 1 个需本机审计样本的 ignored 集成测试。
- `cargo fmt --all -- --check`：通过。
- `cargo clippy -p ashare-warehouse --all-targets --locked --offline -- -D warnings`：通过。
- 最新 `cargo clippy --workspace --all-targets --locked --offline -- -D warnings`：未通过，C 所有文件 `crates/quant-research/src/run_jobs.rs:319` 有 `needless_borrow`（`snapshot_symbols(&snapshot)` 应去掉多余引用）。本批不修改 C 文件；需由 C/统筹修复后重跑。
- 调度 shell 语法检查通过；Web build 通过（有 Vite 大 bundle 提示）。
- 生产真实验证只覆盖 2 个证券的最近一个已完成交易日；无全市场回填/验证。无变化复跑真实接口失败，自动调度未运行。生产 DuckDB 的其他 writer 进程身份仍 Unknown；初次状态 CLI 成功取得排他锁，最后一次状态 CLI 成功取得共享锁。

## 集成建议

1. 统筹将本文件与本分支的 `src/lib.rs`、`src/main.rs`、`scripts/daily_sync_schedule.sh`、`configs/daily-sync.symbols` 合并；共享 `STATUS.md` / `HANDOFF.md` / 公共契约由统筹汇总更新。
2. 将两只样本证券目录以及日线读取契约交给 C；C 固定 snapshot `ccaff0e2-9871-496a-b09f-1870d165e15c` 验收只读，不读取 writer 数据库。
3. A 状态证据通过同一个排他写入门面导入；此快照 `unknown_state_security_days=2`，不可补成 TRADABLE。
4. 重试 Tencent 失败需先区分真实空响应、限流/超时和传输错误，再做成功的无变化复跑；之后再审查并启用调度。不要启动历史全市场抓取。
