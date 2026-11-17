# 永动机本机冻结与离线复算

本流程为 #392 固定旧 ETF 基线留存私有输入快照；不把真实行情、原始响应、详细账本、输入摘要或运行身份写入 GitHub 或聊天。回放只使用 Python 标准库，冻结步骤会在线取得基准锚点之后的腾讯不复权日线与新浪基金 `s/u` 因子。

## 冻结输入

旧本机项目须包含 `data/long_history/tencent_daily.csv.gz`、`data/etf_daily.csv`、`strategy-dashboard/nasdaq-513300.csv.gz`、`strategy-dashboard/data.json`、`backtest.py`、`dividend_data.py` 与 `strategy-dashboard/daily_update.py`。执行时通过 `--legacy-root` 指向旧项目目录；脚本只读这些文件。

为每次冻结指定新的、仓库之外且尚不存在的 snapshot 目录：

```sh
python3.12 scripts/eternal_baseline.py freeze \
  --legacy-root "<旧本机项目目录>" \
  --snapshot "<仓库外私有目录>/eternal-baseline-<新版本>"
```

冻结会保存所需资产的本机原始 CSV 行、原始 HTTP 响应、`requests.json` 请求记录、归一化行情/因子、历史参考净值、源码副本与 SHA-256 清单。旧源码按静态本地 import 递归归档，动态导入与外部依赖仍未验证。清单将配置、来源、运行环境、日期范围和输入文件一起绑定到 `input_version`，并校验 manifest 摘要。每个 snapshot 不可覆盖；遇到零成交量、缺重叠、缺初始因子或参考日期缺行情时，私有目录保留 `pending-gaps.json`，列出标的、日期、字段、影响及候选措施，状态为 `pending_decision`，不能作为冻结基线回放。其他数据校验或请求失败明确报错，不降级或静默跳过。`d5d48c1` 是 Issue 记载的源码修订，但旧本机副本无 Git 历史，manifest 明确记录其未验证状态。

## 离线复算

```sh
python3.12 scripts/eternal_baseline.py replay \
  --snapshot "<仓库外私有目录>/eternal-baseline-<新版本>" \
  --output "<仓库外私有目录>/eternal-baseline-<新版本>-replay.json"
```

`replay` 不访问网络，先校验 snapshot 文件集合与摘要，再生成逐日 NAV、现金、有效总回报指数单位持仓、信号、交易、费用及指标。结果文件必须是仓库外的新路径；同目录还会生成 `.run.json`，绑定结果 SHA-256、输入版本、回放源码摘要及运行环境。返回码 `0` 表示参考日期/NAV 与可用汇总字段在 `1e-7` 容差内匹配；返回码 `1` 表示复算完成但对拍不符；结构/输入错误会明确报错。详细输出只写本机文件。

回放要求当前源码与 snapshot 中冻结的 `source/eternal_baseline.py` 完全一致；源码不同或有效配置与冻结代码不一致时会失败。新版本不接受旧 schema；如需重跑旧版本，将冻结脚本**复制**到与 snapshot 和输出均不重叠的仓库外新目录（不能直接从 snapshot 内运行，否则旧版路径守卫会拒绝）：

```sh
mkdir -p "<仓库外私有运行目录>/scripts"
cp "<snapshot>/source/eternal_baseline.py" "<仓库外私有运行目录>/scripts/eternal_baseline.py"
PRAJNA_ETERNAL_BASELINE_REPOSITORY_ROOT="$(git rev-parse --show-toplevel)" \
  python3.12 "<仓库外私有运行目录>/scripts/eternal_baseline.py" replay \
  --snapshot "<snapshot>" \
  --output "<仓库外私有目录>/eternal-baseline-<新版本>-replay.json"
```

旧 schema 运行旧冻结脚本，只能验证旧版本行为；旧版不具备当前配置/源码绑定保护，不可当作当前 head 的复算证据。运行目录和结果路径均须在仓库外；环境变量只增加保护目录，不替代脚本所在仓库的路径守卫。

冻结的 `backtest.py`、`daily_update.py` 与 `dividend_data.py` 仅作为来源证据归档；独立回放适配器并不导入或执行这些旧文件。因此，snapshot 绑定证明回放使用了被冻结的适配器及其记录配置，不等于证明适配器与无法核实修订的旧生产源码等价；该等价性和需独立访问私有输入的实际复算仍是未验证证据边界。

```sh
python3.12 -m unittest tests.test_eternal_baseline -v
```

## 口径与验收映射

| Issue/spec 要求 | 实现入口 | 合成验证 / 本机证据 |
|---|---|---|
| 五标的、30/20/20/25/5 权重、10% 阈值、单边 5 bp；收盘触发、下一共同开盘执行 | [`scripts/eternal_baseline.py`](../scripts/eternal_baseline.py) 的 `WEIGHTS`、`simulate` | `test_close_signal_executes_whole_portfolio_next_common_open`；私有 replay summary |
| 基金拆分/分红总回报价格及缺因子拒绝 | `parse_sina_factors`、`total_return_prices` | `test_sina_split_dividend_factors_create_total_return_prices`、`test_split_factor_adjusts_raw_opening_basis`、`test_factor_gap_and_duplicate_dates_fail_closed` |
| 首个重叠收盘拼接、方向性涨跌停整篮子顺延 | `splice_etf`、`opening_limits`、`simulate` | `test_stitch_scales_after_first_overlap_and_uses_legacy_limits`、方向性涨跌停及缺重叠反例 |
| 初始资金、源码摘要、运行环境、配置、输入版本、暖机与回测区间 | 私有 `manifest.json` | 清单哈希绑定与源码摘要校验；旧生产源码等价性未验证；归一化本金口径见下 |
| 日期、信号/成交、费用、现金/持仓、逐日 NAV 与指标对照 | 私有 replay JSON 与 `compare_reference` | 日期/NAV/参考可用指标对拍；原参考输出不含信号、成交、费用、现金、持仓，故这些字段明确标为不可对照，不声称通过 |

旧模拟器只有归一化 `NAV=1.0`，没有人民币本金参数或参考字段；按负责人确认，manifest 记录 1.0 模型单位，并将 CNY 金额标为 unavailable。模拟器的 `units` 是旧实现使用的总回报指数单位，不应解释为实际 ETF 份额数量。旧看板源码与 Issue 声明的 `d5d48c1` 无可验证 Git 关联；摘要仅识别实际归档的源码文件，不能证明其与该修订相同或回放适配器与旧算法等价。以上限制均不通过调整策略、缩短区间或更换因子口径规避。

历史资产选择及代理拼接缺少上市、退出和历史成员 PIT 证明，存在选择及幸存者偏差风险；分红、拆分与复权因子的历史可用时点也未验证，历史总回报口径的 PIT 可靠性未知。未测量这些风险的大小。
