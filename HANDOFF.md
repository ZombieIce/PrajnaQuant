# Agent Handoff — Batch 1 集成后

日期：2026-09-24。正式验收矩阵与复现证据见 [Batch 1 集成验收](docs/handoffs/batch1-integration-acceptance.md)；上一轮逐项日志保存在 [历史交接归档](docs/handoffs/pre-batch1-handoff-archive.md)，其中早期“未修复/未展示”表述已被本页和验收报告取代。

## 当前状态

A/B/C 的代码和本地结果在同一 `main` 工作区完成集成，验收基线 HEAD 为 `8381beb`；检查时没有其他 worktree。Rust 合成手算验证执行延期、状态拒单、账本和通用快照。真实五 ETF 用已发布 Universe 版本与固定 ETF 快照完成复跑、落盘、API 精确筛选和浏览器展示。**本次真实快照无执行状态列，实验明确为 `legacy_bar_only`；本批结论为部分通过。**

项目实际输出：`research-output/experiments/4e076758-8069-4e0e-9269-1d3f9d574159/experiment.json`。Universe `d8811237-6c37-4189-86b8-9b05fbccc405`、版本 `3fcfed9c-b8c1-4cce-a933-bc2b79ebf0da`、内容 hash `91aa64f7872ced85b4513f224fa617a14fa12ae99c86f5936085a86466a4ba18`；快照 `fe1dd8a3-d227-4e06-bb98-f0a6b3be0f34`、ETF 文件 hash `8c8debb7e1fdd7b3b592cc0991e807b26f1e94a59fb7bbe7e66e00f3030af872`。固定成员 513300、518880、159612、510320、159952；2025-09-23—2026-09-21 的 241 个深交所日历开市日每只都有 bar。成员仍是 `retrospective_static`，行情来源逐行未验证，零分红/原始价格收益假设不能解释为总回报或无偏历史收益。

## 已验收与开放

- 已验收：3 ETF×10 日手算账本、Top-N 超额触发保护、状态未知/停牌与缺价原因、延期及目标替换、目标买单跳过、末日目标审计、逐日 NAV/成本/P&L；通用股票/ETF 冻结快照及预热/正式区间合成测试；旧快照/旧报告兼容；实际版本发布幂等、五 ETF 结果与 API/网页追溯。
- 开放 P0：历史 ETF PIT 名录与上市/终止、公司行动/总回报、可信历史执行状态覆盖与可知时点。旧 ETF 快照仍 `legacy_bar_only`；真实停牌、涨跌停方向限制、成交量/排队未验。股票通用数据接口已存在，股票回测仍被能力门槛阻断。沪深300真实成分验证暂缓。
- 外部输入：本机被 Git 忽略的 `data-core/` 与 `research-output/snapshots/`。`data-core/.writer.lock` 存在且系统拒绝进程枚举，生产回填实时进度 Unknown；本批未连接生产 DuckDB。复现需要上述 hash 对应的本地文件与发布版本 JSON。

## 验证与复现

`cargo fmt --all -- --check`、`cargo test --workspace --locked --offline`（10 仓库 + 49 研究；1 真实 opt-in 默认忽略）、`cargo clippy --workspace --all-targets --locked --offline -- -D warnings`、`cd apps/web && npm run build`、`git diff --check` 均通过。前端仅有 535.01 kB 图表 chunk 警告。真实 opt-in 测试使用 **绝对** `PRAJNA_ETF_MVP_OUTPUT=/Users/yibinzhang/Documents/ChatGPT/A股量化/research-output` 后 1 passed；相对路径会因 Cargo 测试工作目录而找不到文件。

```bash
cargo run --locked --offline -p quant-research --example etf_universe_mvp -- \
  --output research-output \
  --universe-id d8811237-6c37-4189-86b8-9b05fbccc405 \
  --version-id 3fcfed9c-b8c1-4cce-a933-bc2b79ebf0da
```

该命令新增一个实验 UUID，不覆盖旧结果；example 核对固定快照 ID/hash 和发布成员。服务端仅有本地结果 GET 与 Universe 管理 API，没有 `/api/v1/runs`；前端新运行按钮保持禁用。独立测试端口 18791/18792 已用于本批 HTTP/浏览器验收，不影响用户已有服务。

## 唯一最优先下一步

获取并核验这五只 ETF 在固定窗口内有来源、覆盖和历史可用时刻证据的执行状态，导出带状态的冻结快照，在同一发布 Universe 版本上复跑并比较成交/未执行审计。在这项真实证据到位前保持 P0-3 开放。
