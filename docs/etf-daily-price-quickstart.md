# ETF 日线价格研究上手指南

准备一个小型单市场 ETF 池和一个交易日期区间。本文使用 MVP-7 本机工作台，原始输入与已发布数据分别存放在用户指定的本机输入目录和研究库中，结果也保存在研究库。输入、成交和比较规则见 [ADR 0039](decisions/0039-etf-daily-price-research.md)。

## 构建并启动

在仓库根目录运行。需要 Rust、Node 20+ 和 npm。首次构建需要获取锁定依赖。

```sh
npm --prefix apps/web ci
npm --prefix apps/web run build
cargo build --locked -p prajna-data --bins
cargo build --locked -p prajna-experiment --bins
cargo build --locked -p quant-research --bin quant-research

export PATH="$PWD/target/debug:$PATH"
export PRAJNA_EXPERIMENT_CLI="$PWD/target/debug/prajna-experiment"
export PRAJNA_LAKE=/absolute/local/path/lake
export PRAJNA_INPUT=/absolute/local/path/inputs
mkdir -p "$PRAJNA_LAKE" "$PRAJNA_INPUT"
```

`PRAJNA_LAKE` 和 `PRAJNA_INPUT` 使用独立本机目录，避开仓库、旧验收档案和发行包解压目录。

## 固定基础配置并导入

按 [交易 Session 日历](contracts/shanghai-shenzhen-session-calendar.md) 导入覆盖整个区间的沪深交易日历，记录发布版本。用 [证券证据入口](../crates/prajna-data/README.md#fixed-security-evidence-snapshots) 为每只 ETF 保存固定证券快照。交易所、上市日期、计价货币、手数、价格及数量增量应来自可复用的基础配置。日期区间要包含最长因子或策略参数所需的预热，以及用于模拟成交和持仓估值的后续 Session。

把日期、日历和证券快照的固定引用填入本机输入目录下的 `config.json`。导入命令以 [固定上游版本的日线方法](https://github.com/simonlin1212/a-stock-data/blob/f21534946c42be3ad048ac938eecb1bad84cb8ab/SKILL.md) 获取未复权及前复权响应，使用现有 Tencent 来源访问、间隔和停源规则。真实运行仅使用已获授权的来源。

以下是配置形状。用本机生成的版本替换`sha256:<…>`，日期与成员按实际研究范围填写。

```json
{
  "calendar_root": "/absolute/local/path/calendars",
  "calendar_version": "sha256:<calendar-version>",
  "evidence_root": "/absolute/local/path/security",
  "start": "2026-01-05",
  "end": "2026-01-30",
  "members": {
    "510300.XSHG": "sha256:<security-snapshot-1>",
    "510880.XSHG": "sha256:<security-snapshot-2>"
  }
}
```

```sh
prajna-import-daily-price --root "$PRAJNA_LAKE" --input-root "$PRAJNA_INPUT" \
  --config "$PRAJNA_INPUT/config.json" --fetch --receipt receipt.json
```

先看终端状态，再在输入目录打开 `receipt.json`。归档批次在 `daily-price/batches/`，失败细节在 `daily-price/diagnostics/`，逐次响应来源和状态在 `reports/`。记录 Dataset 版本和每只 ETF 的复权版本。区分缺交易 Session、缺原始价和缺复权价，保留源响应和实际覆盖。原始价的交易日缺口可能影响成交或估值。每次导入把本次响应保存到 receipt 的 `replay_dir`（相对输入根）。用 `--replay <replay_dir>` 复跑同一组固定响应，得到相同的 Dataset 和复权版本。手工准备的复跑目录也相对输入根，文件按 `<前缀和代码>-<raw或qfq>-<起日>-<止日>.json` 命名，分段边界与请求一致。修订输入会产生新版本。真实响应、详细覆盖、身份和研究结果仅留本机。

在第二个终端复用同一组路径启动工作台。

```sh
quant-research serve --address 127.0.0.1:7878 --web-dist apps/web/dist \
  --dataset-root "$PRAJNA_LAKE" --output "$PRAJNA_LAKE" \
  --market-data-dir "$PRAJNA_LAKE/market"
```

## 从固定 Dataset 到可重读的结果

在浏览器打开本机工作台（`http://127.0.0.1:7878`）。

1. 导入沿用生产池发布，目录中还会列出每只 ETF 的单证券 Dataset。在 `/datasets` 选择 receipt 的 `dsv`，确认证券、Session、行情缺口与复权绑定。要运行 Top-K，在 `/pools` 编辑并发布成员与该 Dataset 一致的池版本。
2. 在 `/research/momentum` 选择同一 Dataset、单 ETF 或固定池版本，并逐个绑定 receipt 中的复权版本。填写窗口，提交因子任务，完成后查看保存的图表、数值和有效样本状态。
3. 在 `/comparisons` 选择「单 ETF 择时 vs 同证券买入持有」，或选择「池内轮动 vs 同池期初等权持有」及「池内动量 Top-K 轮动」。选「日线价格模拟」，使用同一 Dataset、固定池及复权版本，配置参数、成本以及按手数可买入的本金后提交。单 ETF 和 Top-K 分别运行。两侧的评价窗口、价格收益和费用口径一致。
4. 任务完成后检查实际成交、费用、现金、持仓权益、跳过或延期记录与期末未执行目标。打开持久结果链接并刷新，核对结果按同一任务重读。再次以同一组固定输入提交，核对两侧窗口和保存的研究结果。执行身份同时受代码来源和运行环境约束。
5. 缺复权输入时检查信号缺口及预热。缺下一开盘价时检查跳过或延期原因。跨除权日按价格收益解读，分红和拆分造成的影响见 [ADR 0039](decisions/0039-etf-daily-price-research.md)。

## 合成复跑与发行物验收

在独立的空目录中运行以下命令。合成生成器提供基础配置和 2026 年 1 月 5 日至 20 日两只沪市 ETF 的原始与前复权响应。用生成的 `config.json` 直接运行同一生产导入器。

```sh
PRAJNA_DAILY_PRICE_FIXTURE_ROOT="$PRAJNA_INPUT" \
  cargo test -p prajna-data --test daily_price_import --locked --offline \
  generate_e2e_input -- --ignored --exact
prajna-import-daily-price --root "$PRAJNA_LAKE" --input-root "$PRAJNA_INPUT" \
  --config "$PRAJNA_INPUT/config.json" --replay replay --receipt receipt.json
```

按上面的工作台步骤操作。择时使用均线窗口 2 和调仓频率 1。Top-K 使用动量窗口 1、K 为 1 和调仓频率 1。设本金 10,000 元、买卖滑点 0 bp、最低佣金 5 元。在 [生产浏览器框架](../apps/web/e2e/README.md) 中也可运行同一套合成输入、导入和持久结果检查。

本通路的验收范围为单市场固定 ETF 池、Fast Event、单 ETF 择时及池内动量 Top-K。价格收益按 ADR 0039 的模拟假设解读。有限真实行情与发行包上的上手验证由 [#387](https://github.com/ZombieIce/PrajnaQuant/issues/387) 承接，其他池的验证见 [#317](https://github.com/ZombieIce/PrajnaQuant/issues/317) 和 [#318](https://github.com/ZombieIce/PrajnaQuant/issues/318)。
