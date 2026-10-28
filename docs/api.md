# 已实现 API 与规划边界

服务入口 `quant-research serve`（server.rs），默认 127.0.0.1:7878；Axum 托管 `apps/web/dist`。`--market-data-dir` 默认 `data-core`，证券行情只从该目录 `snapshots/current.json` 或指定不可变 ID 读取，校验 manifest/Parquet/目录/日历 hash，不打开正在写入的 DuckDB。Universe 配置写 API 无身份认证，因此服务拒绝非 loopback 绑定；CORS 仍 permissive。下表是实际路由，未出现的接口不得按已实现使用。

| 分类 | 方法/路径 | 现状/响应 |
| --- | --- | --- |
| Metadata | GET /api/v1/health | Implemented；status/service |
| Metadata | GET /api/v1/instruments?q=&asset_type=&snapshot_id=&limit=&cursor= | Implemented；`q` 至少 2 字，`asset_type=etf|stock`，默认 20/最多 100；从固定已发布快照目录查名称/代码/ID，返回 `items,snapshot,data_cutoff_date,next_cursor`。`stock` 包含已分类股票及明示未核验的 `EQUITY_CANDIDATE`；缺稳定 `instrument_id` 的真实隔离样本不冒充可查股票。续页应显式携带首屏 `snapshot_id` |
| Market | GET /api/v1/daily-bars?instrument_id=&snapshot_id=&start=&end=&limit=&cursor=&adjustment=none | Implemented；稳定 `SH/SZ/BJ:code` 主键、闭区间且最长 3661 自然日、默认 250/最多 1000 行升序分页；回显固定快照 ID/数据及 manifest hash、截止日、原始未复权口径、CNY/股或 CNY/份、股/份成交量与可空 CNY 成交额。`qfq/hfq` 拒绝；不补零、不抽样 high/low。无发布快照 503，坏 hash/身份 409，未知证券 404，参数/游标 400 |
| Dataset | GET /api/v1/datasets | 已发布研究 Dataset 目录；有效版本及隔离的 invalid_manifests，不选择 latest |
| Dataset | GET /api/v1/datasets/{dsv} | 固定版本的证券、交易条款、生命周期证据、实际 Session/Bar 覆盖及区间缺口 |
| Dataset | GET /api/v1/datasets/{dsv}/bars?instrument=&start=&end=&limit=&offset= | 固定版本/证券原始未复权 Bars，闭区间日期、默认 250/最多 1000，返回 total/offset/next_offset；定点数为字符串 |
| Factor | GET /api/v1/signals | Implemented；固定信号目录 |
| Factor | GET /api/v1/signals/{key} | Implemented；单个定义，未知 key 为 404 |
| Result / Factor | GET /api/research/signals/{key}/report | Implemented；按文件修改时间找最新已保存 report，未运行则 404。报告标注 `calendar_basis`、标签方法、评价状态、coverage/missing 计数；Pearson/Rank IC、分组前瞻收益在截面不足时为 null/空数组。分组收益是标签，不是组合净值 |
| Result / Strategy | GET /api/experiments | Implemented；枚举实验文件汇总，最新在前；可选 universe_id/version_id 必须同时提供并精确筛选；摘要显式携带 Universe 身份、PIT/覆盖状态和 `score_mode=rotation_composite|single_momentum`，旧文件返回 `legacy_universe_unknown` |
| Result / Backtest | GET /api/experiments/{id} | Implemented；返回完整 config、snapshot、factor、backtest、trades、equity/benchmark curves。新结果另含逐日 `backtest.position_curve`、期末 `backtest.instrument_performance`、`backtest.rebalance_deferrals` 及 `backtest.unexecuted_orders`（逐腿未执行原因、状态及状态源）；旧结果读取时新增数组默认为空、`execution_status_mode` 缺失为 null，不会补造历史审计。`position_curve.holdings[]` 新增可空 `mark_date` 与 `stale_calendar_days`。新版 ETF 快照带冻结执行状态并按有来源的 `TRADABLE` 门槛成交；旧快照没有状态列时报告 `legacy_bar_only` 并保留 bar-only 兼容路径 |
| Result / Factor | GET /api/v1/factor-reports | Implemented；可按 signal_key 与显式 universe_id/version_id 筛选已保存 report；缺少 Universe 身份的旧报告只在未筛选列表中出现，前端标为未知 |
| Universe | GET /api/v1/universes | Implemented；支持 asset_scope/status/q/limit，小型本地注册表暂不支持 cursor |
| Universe | POST /api/v1/universes | Implemented；创建草稿 |
| Universe | GET /api/v1/universes/{id} | Implemented；元数据、版本摘要和草稿 |
| Universe | PATCH /api/v1/universes/{id}/draft | Implemented；同一写锁内修改草稿，members 替换整个成员列表 |
| Universe | GET /api/v1/universes/{id}/versions/preview | Implemented；返回草稿差异字段、下一版本号和确认用 draft_hash |
| Universe | POST /api/v1/universes/{id}/versions | Implemented；经差异确认后发布不可变 UUID 版本；内容未变化时返回 created=false，不增加版本；预览后草稿变化则拒绝过期确认 |
| Universe | DELETE /api/v1/universes/{id} | Implemented；无版本删除草稿，有版本则归档 |
| Universe | GET /api/v1/universes/{id}/members?version_id=&as_of= | Implemented；指定版本/日期，决策截止上海时间 15:00；无历史 provider 时返回空成员、unknown/gaps 和警告 |
| Universe | GET /api/v1/universes/{id}/coverage?version_id=&start=&end= | Implemented；分别返回成员覆盖与行情覆盖。行情从最新且 manifest SHA-256 校验通过的 ETF Parquet 快照逐证券审计，包含行数、日期范围、缺失日、重复日与 OHLC 异常；快照 ID/hash 一并返回。若快照含 hash 校验通过的 `trading_calendar.parquet`，按完整自然月深交所官方开市日识别成员共同缺日，并返回 `calendar_snapshot` 与 `calendar_status`。无旁车/请求范围日历不全时不推断闭市，日历状态为 `unverified`/`gaps`；该来源尚未与上交所官方日历独立对拍。手工成员仍标 `retrospective_static`，并附能力阻断原因 |

未实现：上交所日历独立交叉核验、可信历史指数成员 provider、POST /api/v1/runs、运行状态/异步队列、网页发起新运行、股票因子/策略运行、batch 查询路由、实验/Universe 完整 cursor 分页、独立成交/仓位/时序查询、计算基准/超额指标 API。前端“新运行”面板仍禁用并标记 API/能力未就绪；Universe 管理和已保存结果筛选已连接上述 CRUD/结果 API，不生成模拟 Universe 或假报告。Universe API 配置存放在输出目录 universes/<id>.json，由服务进程写锁和原子替换保护，不写 DuckDB。

Universe 管理页默认用普通序号显示版本并隐藏 ID/哈希；折叠的技术信息仍可用于排查。归档保留 Universe、已发布版本和历史结果关联，禁止继续编辑/发布；未发布的空草稿才会物理删除。

## 规模与契约风险

实验详情一次序列化返回全部结果，包括 factor.daily、equity_curve、benchmark_curve、trades。当前本机多个实验 JSON 约 2.7–3.0 MB；十年更多因子/交易会放大响应、浏览器内存和重绘成本。`/api/experiments` 会逐文件读取并解析所有实验以生成目录；因子目录在浏览器对十个信号分别请求已保存报告。React ECharts 有 tooltip、inside/slider zoom，曲线 `showSymbol:false`；没有 brush/crosshair、显式抽样/虚拟化或十年大数据性能验收。Universe 注册表分页尚未支持 cursor；既有实验列表仍逐文件解析。

`server.rs` 以 `web_dist` 静态回退，仓库中旧 `dashboard.html` 和 `factor_dashboard.html` 当前未在服务端引用。`/api/research/...` 与 `/api/v1/...` 路径版本风格不统一；变更现有路径前检查前端调用。

## Batch 2 行情边界

`/market` 页面在合成股票/ETF 快照通过浏览器验收，显示日 K、成交量、快照 ID/hash、原始价单位与数据截止日，未当作实时行情。`serve` 对 B 发布格式使用 `snapshots/current.json` 的固定指针，对 C 旧 schema 1 合成发布格式使用 `publication.json`；部署时须显式选择同一 `--market-data-dir`，不会从生产 DuckDB 现查。B manifest 只有 `created_at`，API 的 `published_at` 对此格式为 null，不把创建时刻冒充发布时间。B 隔离真实样本的证券身份为 null、分类 UNKNOWN，故无法证明真实股票搜索/页面。旧 ETF-only 研究快照没有新发布证明时不冒充行情 API 发布版。实验/因子详情的多 MB 全量响应限制与证券日线分页是不同问题；十年真实证券查询性能尚未验。

## Batch 1 查询边界

五 ETF 最终实验 `4e076758-8069-4e0e-9269-1d3f9d574159` 可用 `GET /api/experiments/{id}` 查询，`GET /api/experiments?universe_id=d8811237-6c37-4189-86b8-9b05fbccc405&version_id=3fcfed9c-b8c1-4cce-a933-bc2b79ebf0da` 精确命中；错误版本不匹配。详情保留版本内容 hash、逐日成员 hash、快照 ID/hash、配置和假设。该真实快照缺交易状态列，`backtest.execution_status_mode=legacy_bar_only`；旧报告该字段为 null，前端应显示未知，不能把默认空 `rebalance_deferrals`/`unexecuted_orders` 解释成新规则下没有异常。`GET /api/v1/universes/{id}/coverage` 若不带 `start/end` 跨整份快照，可能因日历旁车覆盖不足返回 `unverified`；五 ETF 已审计区间需显式传 `2025-09-23` 至 `2026-09-21`。即使该区间开市日期完整，旧 ETF Parquet 缺逐行 source，整体行情审计仍 `unverified`。

## 已发布研究 Dataset 预览

`/datasets` 使用上述独立 Dataset 路由。`serve --dataset-root <LOCAL_LAKE>` 必须显式配置，省略时 API 返回 503 `dataset_root_unconfigured`；不回退 `--output`、旧行情快照或 MVP-6 存档。配置 root 的验证由生产读取入口执行（包括拒绝固定 MVP-6 存档及其后代），不会创建 root。服务仍只绑定 loopback。

服务通过 `PRAJNA_DATASET_PREVIEW_CLI`（缺省在 PATH 查找 `prajna-preview-dataset`）调用只读 CLI；无新平台 crate 直接依赖。成功与读取错误保留 schema version 1 的 CLI JSON envelope，成功体为 `{schema_version:1,data:...}`，不会计算因子、填补缺失或触发发布/下载。参数通过 argv 传递，不经 shell；bars 只接受上表参数，未知参数拒绝。非法输入 400、版本/证券不存在 404、root 或 CLI 不可用 503、manifest/表/IO 失败 500、CLI 协议异常 502，响应不缓存。详细字段/限制见 [读取契约](contracts/dataset-preview-read.md)。

页面不默认选版本/证券；日期筛选与分页固定 DSV，切换选择取消旧请求，错误、空目录、坏版本、缺行情及 Unknown 明示。价格/数量字符串直接展示，生命周期与实际行情起点分开，不将 Session 数量等同可交易覆盖。真实明细仅经本机服务在本机页面展示，禁止转贴到公开渠道。CLI 当前每请求读取整表，catalog 验证所有版本；本票不声称大规模性能或完整真实数据验收。

合成生产 API/浏览器验证准备（fixture root 必须尚不存在）：

```sh
cargo build -p quant-research --locked --offline
cargo test -p prajna-data --test dataset_preview_read --locked --offline
PRAJNA_PREVIEW_FIXTURE_ROOT=/tmp/preview-synthetic \
  cargo test -p prajna-data --test dataset_preview_read --locked --offline \
  export_synthetic_preview_fixture -- --ignored --exact
PRAJNA_DATASET_PREVIEW_CLI="$PWD/target/debug/prajna-preview-dataset" \
  target/debug/quant-research serve --output /tmp/preview-output \
  --dataset-root /tmp/preview-synthetic/lake --web-dist apps/web/dist
python3.12 scripts/verify_dataset_preview_http.py --fixture-root /tmp/preview-synthetic \
  --reader target/debug/prajna-preview-dataset
```

随后在本机浏览器访问 `/datasets`，选择 ETF/股票版本及证券，核对已知/未知生命周期、缺口、原始行情及空范围。浏览器自动化框架仍由 T04 建立；该命令仅验证 HTTP 与正式 CLI 的一致性，不能替代浏览器操作。

已安装 Playwright/Chrome 的本机也可运行独立合成浏览器 smoke：

```sh
PRAJNA_PREVIEW_FIXTURE_ROOT=/tmp/preview-synthetic \
  PRAJNA_PLAYWRIGHT_MODULE=<absolute-playwright-core-index.mjs> \
  PRAJNA_CHROME_EXECUTABLE=<local-chrome-executable> \
  node scripts/verify_dataset_preview_browser.mjs http://127.0.0.1:7878
```

该脚本只复用已安装工具并要求合成 fixture 标记；可选 `PRAJNA_PREVIEW_SCREENSHOT_PREFIX` 将截图留本机。它核对两个版本的证券/生命周期、缺口、原始 Bars、空范围与刷新，T04 仍负责浏览器测试框架及完整任务闭环。
