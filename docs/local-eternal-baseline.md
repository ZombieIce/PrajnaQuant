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

冻结会保存所需资产的本机原始 CSV 行、原始 HTTP 响应、`requests.json` 请求记录、归一化行情/因子、历史参考净值、源码闭包副本与 SHA-256 清单。每个 snapshot 不可覆盖；遇到数据缺失、因子不完整、日期重复或请求失败会报错，不降级或静默跳过。`d5d48c1` 是 Issue 记载的源码修订，但旧本机副本无 Git 历史，manifest 明确记录其未验证状态，并固定实际源码文件摘要。

## 离线复算

```sh
python3.12 scripts/eternal_baseline.py replay \
  --snapshot "<仓库外私有目录>/eternal-baseline-<新版本>" \
  --output "<仓库外私有目录>/eternal-baseline-<新版本>-replay.json"
```

`replay` 不访问网络，先校验 snapshot 文件集合与摘要，再生成逐日 NAV、现金、有效总回报指数单位持仓、信号、交易、费用及指标。结果文件必须是仓库外的新路径；同目录还会生成 `.run.json`，绑定结果 SHA-256、输入版本、回放源码摘要及运行环境。返回码 `0` 表示参考日期/NAV 与可用汇总字段在 `1e-7` 容差内匹配；返回码 `1` 表示复算完成但对拍不符；结构/输入错误会明确报错。详细输出只写本机文件。

```sh
python3.12 -m unittest tests.test_eternal_baseline -v
```

## 口径与验收映射

| Issue/spec 要求 | 实现入口 | 合成验证 / 本机证据 |
|---|---|---|
| 五标的、30/20/20/25/5 权重、10% 阈值、单边 5 bp；收盘触发、下一共同开盘执行 | [`scripts/eternal_baseline.py`](../scripts/eternal_baseline.py) 的 `WEIGHTS`、`simulate` | `test_close_signal_executes_whole_portfolio_next_common_open`；私有 replay summary |
| 基金拆分/分红总回报价格及缺因子拒绝 | `parse_sina_factors`、`total_return_prices` | `test_sina_split_dividend_factors_create_total_return_prices`、`test_split_factor_adjusts_raw_opening_basis`、`test_factor_gap_and_duplicate_dates_fail_closed` |
| 首个重叠收盘拼接、方向性涨跌停整篮子顺延 | `splice_etf`、`opening_limits`、`simulate` | `test_stitch_scales_after_first_overlap_and_uses_legacy_limits`、方向性涨跌停及缺重叠反例 |
| 初始资金、源码/依赖闭包、运行环境、配置、输入版本、暖机与回测区间 | 私有 `manifest.json` | 冻结输入清单与源码摘要；归一化本金口径见下 |
| 日期、信号/成交、费用、现金/持仓、逐日 NAV 与指标对照 | 私有 replay JSON 与 `compare_reference` | 日期/NAV/参考可用指标对拍；原参考输出不含信号、成交、费用、现金、持仓，故这些字段明确标为不可对照，不声称通过 |

旧模拟器只有归一化 `NAV=1.0`，没有人民币本金参数或参考字段；按负责人确认，manifest 记录 1.0 模型单位，并将 CNY 金额标为 unavailable。模拟器的 `units` 是旧实现使用的总回报指数单位，不应解释为实际 ETF 份额数量。旧看板源码与 Issue 声明的 `d5d48c1` 无可验证 Git 关联；摘要只能证明实际输入源码文件闭包，不能反推该修订相同。以上限制均不通过调整策略、缩短区间或更换因子口径规避。
