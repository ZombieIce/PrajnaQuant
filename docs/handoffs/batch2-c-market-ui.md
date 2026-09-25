# Batch 2-C：不可变日线查询 API 与证券行情页

集成补记（2026-09-25）：B 的正式发布格式已由统筹接入，运行 `serve` 时默认 `--market-data-dir data-core` 读取 B `current.json`；本交接下面的 `publication.json` 描述是 C 独立合成快照格式，仍用于兼容测试。最终跨链证据见 [`batch2-integration-acceptance.md`](batch2-integration-acceptance.md)。

日期：2026-09-24。范围是只读证券搜索、日频 OHLCV API、React 行情页、合成验收。**股票生产快照和真实股票覆盖未验证；本交接中的行情样本全部是合成数据。** 股票行情查询不开放股票回测。

## 页面与 API

- 网页路由：`/market`，`main.tsx` 使用 `lazy()` 懒加载；`Chart.tsx` 按需注册 ECharts `CandlestickChart`、`BarChart`、`DataZoomComponent`、`TooltipComponent` 等组件。
- `GET /api/v1/instruments?q=&asset_type=&snapshot_id=&limit=&cursor=`：`q` 至少 2 字符；`asset_type=etf|stock`；默认 limit=20、最大 100。`stock` 返回快照中 `EQUITY` 与 `EQUITY_CANDIDATE`；候选明确显示未核验。按 `instrument_id` 请求优先，身份统一为 `SH:600000` / `SZ:510300`，交易所为 `SH|SZ|BJ`。返回 `{items,snapshot,data_cutoff_date,next_cursor}`。原 ETF 路径、`q`、`asset_type=etf` 与 `items` 保持；Universe 页面把新契约 `SH/SZ`、`ETF` 映射到原页面既有 `XSHG/XSHE`、小写枚举。
- `GET /api/v1/daily-bars?instrument_id=&snapshot_id=&start=&end=&limit=&cursor=&adjustment=none`：证券与含端点日期必填；闭区间、最大 3661 个自然日（`end-start <= 3660`）；默认 limit=250、最大 1000；升序、不抽样，超过一页以游标续查。游标绑定 snapshot、证券、范围和查询过滤；续页必须指定相同 `snapshot_id`。`adjustment` 只接受 `none`，其他值 400。
- 每条 bar 返回 `trade_date, open, high, low, close, volume_shares, amount_cny, source, observed_at`。OHLC 缺失保留 null，成交额可空。价格为原始未复权；股票单位 CNY/股、ETF CNY/份，成交量股/份，成交额 CNY。不会把缺行情填成零价。
- 数据从 `data::load_snapshot_security_directory` 与 `data::load_daily_research_rows` 读取；前者按不可变 Parquet 返回 distinct 目录，后者限定单证券/日期范围并排除 `is_preheat`。不读取生产 DuckDB、不扫描全部市场日线来构造证券目录。

## 发布身份与错误

查询只读取 `output/snapshots/{snapshot_id}/daily_research.parquet`，并校验 `manifest.json`、Parquet SHA-256 和发布记录 SHA-256；最新快照按已发布 `published_at` 固定解析，具体页面请求/游标随响应和请求携带 snapshot id，不随发布指针漂移。

C 实现的 `publication.json` 形状：

```json
{
  "status": "published",
  "snapshot_id": "UUID",
  "published_at": "RFC3339",
  "data_cutoff_date": "YYYY-MM-DD",
  "coverage_status": "complete|gaps|unverified",
  "price_adjustment": "none",
  "source_status": "unknown|...",
  "manifest_sha256": "hex"
}
```

这是 B 写库/发布端需要采用或由统筹协调映射的接线点。旧 ETF-only Parquet 没有该发布记录或通用身份字段时，不会被冒充成新发布快照；不会默默退回“最新任意文件”。

- 400：参数、日期范围、游标、limit、复权口径无效/不支持。
- 404：快照 ID 不存在或该 snapshot 未包含证券。
- 409：发布身份、manifest/hash、字段或 Parquet 损坏。
- 503：无已发布通用快照、快照缺少发布记录/尚未发布。
- 合法范围无该证券 bar：200 与空 `items`。

## 页面行为与口径提示

搜索与选择股票/ETF后读取真实 API；日期默认覆盖最近最多 3661 天，用户可调整。已分类股票/ETF 显示“当前分类（非 PIT）”，候选分类明示未核验，`UNKNOWN` 不冒充股票；分类未知时价格单位显示 unknown。日 K 使用完整日交易 OHLC，成交量共用 x 轴缩放；tooltip 展示 OHLC、成交量/额、逐行来源和采集时间。数据身份区展示 snapshot ID、行情/manifest hash、截止日、原始价格口径及单位。覆盖非 complete、陈旧 cutoff、来源未知分别提示；来源未知不由 `selection_rule` 推断。空数据、错误、加载和下一页状态都有 UI。

## 测试与浏览器证据

- 7 个 `market_api` Rust 测试：股票/ETF 混合目录、搜索过滤/游标、日期排序和页边界、OHLCV 逐点及单位、NULL 成交额、预热排除、空数据/未知证券/缺字段/坏 hash/未发布区分、游标过滤绑定、新 snapshot 发布不改变显式固定查询。
- `market_snapshot_fixture` example 在临时输出创建 5,600 行合成混合快照（股票与 ETF 各 2,800 条工作日日线，覆盖十年级区间）；manifest 和发布状态都显式标识 `synthetic` / `unverified`。复现：

  ```bash
  cargo run --locked --offline -p quant-research --example market_snapshot_fixture -- /private/tmp/prajna-market-ui-fixture-20260924
  cargo run --locked --offline -p quant-research -- serve --output /private/tmp/prajna-market-ui-fixture-20260924 --market-data-dir /private/tmp/prajna-market-ui-fixture-20260924 --web-dist apps/web/dist --address 127.0.0.1:18801
  ```

- 实际 Codex 浏览器打开 `http://127.0.0.1:18801/market`，页面调用本地 Axum API。搜索 `600000` 并选择股票、搜索 `510300` 并选择 ETF 均成功；两类样本显示 1,000 根 K 线页和联动成交量；tooltip 分别显示 CNY/股、CNY/份的原始 OHLC 与成交量/额、`synthetic_fixture` 来源和 `observed_at`。滚轮/slider 缩放可用；周末日期 `2016-09-17` 显示空状态。响应中的 snapshot ID/hash、数据 cutoff=`2026-09-24`、覆盖 `unverified` 和来源 `synthetic` 均在页面可见。
- 10 年合成单证券 API 最大页观测：1000 条 JSON 为 **205,246 bytes**；debug handler 约 **445 ms**（机器本地单次，不是吞吐基准）。浏览器页面绘制 1,000 根蜡烛和 volume 图正常。API 页限制 1000 条，下一页仍用固定 snapshot。
- 同一浏览器的策略绩效、因子目录和 Universe 管理路由均载入；测试输出目录没有已保存实验/Universe，因此只核验旧页面能够加载及保持其空态/运行禁用提示。
- 前端 build 保留既有 ECharts 大 chunk 警告：`Chart` chunk 约 544 KB minified / 181 KB gzip。该路由懒加载，但 chart module 是因子/策略详情和行情页共用 chunk。

## 验证

- `rustfmt --edition 2024 --check crates/quant-research/src/data.rs crates/quant-research/src/server.rs crates/quant-research/src/market_api.rs crates/quant-research/examples/market_snapshot_fixture.rs`：通过。最终 `cargo fmt --all -- --check` 被共享 B 草稿拦截：当前 `src/lib.rs:519,526,530` 有无效 Rust 字符串/字符字面量；`src/main.rs` 另有格式差异。这些是 B 修改，未由 C 变更。
- `cargo test -p quant-research --locked --offline market_api -- --nocapture`：7 passed；包括响应大小观察。
- 最终 `cargo test -p quant-research --locked --offline`：55 passed；真实本地 Universe fixture 1 ignored。全 workspace 10 warehouse + 55 research 曾在 B 的 `src/lib.rs`/`src/main.rs` 草稿出现前通过；本次最后的 workspace 命令由上述 formatter 错误短路，未重复运行。
- `cargo clippy -p quant-research --all-targets --locked --offline -- -D warnings`：通过。全 workspace Clippy 曾在 B 草稿出现前通过。
- `cd apps/web && npm run build`：通过；有上述 chunk size warning。
- 最终 `git diff --check`：通过。

## 文件与统筹接线

C 修改：`crates/quant-research/src/market_api.rs`、`crates/quant-research/src/data.rs`（快照目录 distinct reader 与预热行过滤）、`crates/quant-research/src/server.rs`（保留原路由路径并 merge 行情 router）、`apps/web/src/pages/MarketPage.tsx`、`apps/web/src/Chart.tsx`、`apps/web/src/main.tsx`、`apps/web/src/shared.tsx`、`apps/web/src/styles.css`、`apps/web/src/pages/UniverseDirectory.tsx`（旧 Universe 搜索值映射）、本 example 和本交接。

`server.rs` 接线：`market_api::router(output)` 合并进现有 Axum app；`lib.rs` 没有新增公共模块入口，行情模块由 `server.rs` 私有路径加载。`data.rs`/`server.rs` 是公共接线文件，统筹合并时请保留 A/B 对应未提交改动；我未改回测、数据库 schema、STATUS 或总 HANDOFF。

## 未解决事项与建议

- 生产股票日线快照未生成/验证；本页不能证明股票市场覆盖。合成目录仅包含显式快照证券，也不是全市场主数据目录。
- 与 B 对齐 `publication.json` 发布记录文件名/字段及写入原子性；若 B 的发布模型不同，应在统筹接线时只改 resolver/适配，不绕开不可变 ID、manifest hash 或发布状态校验。
- 旧 ETF-only 快照若部署环境尚无通用已发布快照，`/api/v1/instruments` 明确返回不可用；需要统一决定批次过渡期是否继续另设只读 legacy adapter，不能假装旧快照具有新身份/来源证明。

唯一建议下一步：统筹与 B 确认发布 sidecar 合约后，在 B 发布的固定混合 snapshot 上复跑两类搜索和日线接口验收；保持真实股票覆盖状态为未验证。
