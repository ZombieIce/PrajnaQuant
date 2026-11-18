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

## CLI 兼容配置任务（#394）

已运行本机服务后，生产 CLI 将冻结 snapshot 路径和配置提交到同一任务队列。Snapshot 路径必须为绝对路径；任务保存该输入引用、有效配置和状态。配置错误在受理时按字段拒绝且不建任务；受理后的缺失/损坏输入、子进程错误会形成可重启读取的 `failed` 状态和本机错误详情。

```sh
quant-research eternal-compat submit \
  --snapshot "<仓库外私有目录>/eternal-baseline-<版本>"
quant-research eternal-compat status <task-id>
quant-research eternal-compat result <task-id>
```

`submit` 默认生成幂等键并使用 `NAV=1.0` 模型单位、固定五资产 `30/20/20/25/5` 权重、`10%` 调仓阈值与单边 `5 bp` 费率。可用 `--config <JSON 文件>` 提交部分变体，例如配置 `{"rebalance_threshold":0.05}`；有效的完整配置会随任务和结果持久化。可调字段为 `initial_nav`（至少 `f64::MIN_POSITIVE` 的有限模型 NAV 单位）、`weights`（恰为五个固定标的且总和 1）、`rebalance_threshold`（0 到 1）及 `fee_rate_one_way`（0 到小于 1）。有效配置并不保证所有缩放后账本值都可表示：如果正 NAV、单位、持仓或费用缩放后溢出、舍入为零，或除回初始 NAV 后不能在相对误差 `1e-12`（零绝对容差）内还原归一化值，runner 会显式失败、持久化字段/标的原因且不发布结果；不会把正配置静默变成零持仓。没有人民币本金字段；绝对 CNY 金额不可用，`initial_nav` 不应解释为 CNY。可用 `--server http://127.0.0.1:7878` 指定其他 loopback 服务地址，CLI 拒绝非 loopback URL。

资本缩放前也核查逐日归一化结果：正权重的单位数或正单位数的持仓若已下溢为零，以 `compat_normalized_value_underflow` 失败并记录字段、标的和日期；零权重允许零持仓。估值核对前，归一化和缩放后账本先逐资产独立核对单位数：初始日按 `本金 / (1 + 单边费率) × 权重 / 当日开盘总回报指数`；实际调仓日以前一日单位数乘执行开盘价得到持仓，以精确有理数求解费用方程 `净值 = 资本 − 费率 × Σ|净值 × 权重 − 持仓|` 的唯一根，再按 `净值 × 权重 / 开盘价` 得出预期；非调仓日单位数必须原样沿用。任一资产相对预期超过 `1e-12`（零预期须为零）即以 `compat_normalized_allocation_precision_loss` / `compat_scaled_allocation_precision_loss` 失败，保存字段、标的、日期、分配依据和容差，不发布结果；这样冻结模拟器在分配中间值的次正规舍入（如权重 `5e-324`、费率 `0.5`）不会因单位与估值自洽而漏检。同一遍历中，预期费用取 `资本 − 精确费用后净值`（初始日资本为本金，调仓日为执行开盘估值）；初始/调仓日的 `daily.fees`、调仓 `trades.fees_in_initial_nav` 与 `summary.fees_in_initial_nav` 对照独立费用及其总和，非调仓日费用须为零，现金须为零或原样沿用。费用是两个资本量之差，binary64 抵消使其自身相对精度受限，冻结求解器也只按资本的 `1e-13` 收敛，故费用字段单独采用相对自身 `1e-10`，不按资本或组合 NAV 放宽；零预期须为零。授权真实默认配置与 5% 变体的冻结费用相对误差高于 `1e-12`、低于 `2e-12`，若采用 `1e-12` 会拒绝默认对拍。极小费率（如 `1e-7` 的初始费用）、完全无换手但记录噪声费用的调仓等无法保证该精度时，以 `compat_normalized_fee_precision_loss` / `compat_scaled_fee_precision_loss` 失败，保存字段、日期、费用依据和容差，不发布结果。随后归一化和缩放后账本均逐资产核对 `holdings = units × 当日收盘总回报指数`：从有限 binary64 的精确有理数值建立独立乘积，要求相对该乘积的误差不超过 `1e-12`，不使用全组合 NAV 或固定绝对容差。即使中间值仍非零，也能识别次正规舍入造成的重大相对误差；零预期必须对应零估值。逐日 NAV 也对照 `现金 + Σ units × 当日收盘总回报指数` 的精确值，相对误差不超过 `1e-12`。超限分别以 `compat_normalized_value_precision_loss` / `compat_scaled_value_precision_loss` 失败，保存字段、标的、日期、估值依据和容差，不发布结果。这不改变冻结模拟算法或接受配置范围。

任务类型/版本为 `prajna_eternal_compat@1`，其兼容运行 ID 与输出文件、摘要、冻结输入版本、兼容 runner/源代码摘要和运行环境独立于 Rust Experiment、Run、Execution。参考比较始终使用单位 NAV 模拟结果；`initial_nav` 只缩放结果中的 NAV、单位、持仓、现金和费用，因此仅改变初始模型 NAV 不会制造归一化参考 NAV 的假 mismatch。默认有效配置必须与冻结参考对拍为 `match`；不匹配时 runner 以 `compat_default_reference_mismatch` 失败，不写成功结果，任务状态及原因持久化。显式变体仍保存 `frozen_reference_comparison`，其中 `evaluation_mode=informational_variant` 表示有效配置造成的参考差异仅供说明，不会单因对比不匹配而失败；只有有效默认配置标为 `required_default_parity`。`status` 可在重启后读取终态及失败原因；`result` 仅对成功任务返回通过 SHA-256 校验的已保存 JSON。未成功任务的 result 请求返回冲突及持久错误，损坏输出不会作为成功结果返回。兼容任务 submit/status 和 result 路由都只接受配置的本机 Host 与同源 Origin；CLI 的 loopback 请求不带 Origin 时允许。原生任务与旧 run 路由不因此增加该限制。服务端持久文件和 CLI 完整结果均可能含详细研究数据，只应留在本机私有输出目录，不要复制到 GitHub 或公开日志。

若 snapshot 含 `pending-gaps.json`，runner 会在读取 manifest 前检查它，因此无 manifest 的 pending-only snapshot 也会以 `compat_input_gaps_pending` 失败，不会写成功结果。有效 gap 报告的全部字段（包括可选 `source` 和未预先识别的字段）保存在本机任务状态 `error.details.runner_error.details.pending_gap_report`；标准 JSON 的有限且可无损往返数值会保留。小数转 binary64 后的最短十进制表示须与输入数值相等（例如 `9007199254740993.0` 会被拒绝，而 `0.1` 允许）；不要求输入保留空白或指数写法。非有限/非标准常量、超过有符号 64 位或无符号 64 位整数范围的值、会下溢或发生十进制精度损失的小数会以 `compat_pending_gaps_invalid` 显式拒绝，`report_contents_persisted: false` 说明原报告未按原类型完整结构化保存，避免丢弃/变形数据后假称完整。拒绝时，若原文可按 UTF-8 解码，`pending_gap_report_raw_json` 保存原始 JSON 文本；可解析的报告另在 `recoverable_pending_gap_report` 保留其余字段及全部 gaps，把不支持的数值/字符串改为显式 `unsupported_json_number` / `unsupported_json_string` 标签，孤立 surrogate 键省略，具体规则由 `report_recovery_encoding` 标注。原文保留不表示原类型结构化完整，失败诊断完整标志仍为 false；重复键/语法错误不猜测恢复结构。`checks_not_run` 明列 manifest、文件清单、配置、源码、bar/factor、总回报价与涨跌停、历史重叠/拼接、共同交易日、模拟及参考对比各阶段。畸形报告、重复 JSON 键或包含孤立 surrogate 的键/字符串以 `compat_pending_gaps_invalid` 失败，避免 Python 接受但 Rust 无法读取整个 sidecar；不可读报告以 `compat_pending_gaps_unreadable` 失败；符号链接（包括悬空链接）或其他非普通文件以 `compat_pending_gaps_unsupported` 明确拒绝，避免对 FIFO 等输入阻塞。完整 runner 失败结构写入任务目录的 `compat-failure.json`，stderr 仍受服务端 16 KiB 上限并只输出短摘要；runner 同时标记 sidecar 是否保存及诊断是否完整，服务仅在 sidecar 与 stderr 错误码/消息匹配且完整标志未关闭时设 `runner_error_details_complete: true`。若原错误详情不可序列化，runner 仍写入含错误码/消息及省略原因的部分 sidecar，并由状态明确标记诊断不完整；sidecar 丢失/损坏也会标记不完整，而不会假称 stderr 包含全部 gaps。结果读取对失败任务仍返回冲突。

为确保完整 sidecar 可由 Rust status/restart 解析，pending-gap JSON 容器嵌套深度上限为 64。超限报告以 `compat_pending_gaps_invalid` fail-closed；原始 JSON 仍保存，普通 gap 字段和其他可恢复内容进入 `recoverable_pending_gap_report`，过深子树以 `unsupported_json_depth` 标记，诊断完整标志为 false。若兼容子进程失败同时遇到进程组清理异常，外层清理错误会在 `details.cli_error` 保留 sidecar 中的结构化 runner 错误；服务重启再次清理时继续保留该嵌套诊断。

读取及校验冻结数据时发现的 `InputGaps` 以 `compat_input_invalid` 失败；已知 gap 保存在 `runner_error.details.input_gaps`，包括原有标的、日期、字段、影响及候选措施。成交量 gap 在对应 bar 有来源时附带其来源；不会臆造其他来源。加载失败的 `check_progress` 记录实际执行范围：清单/文件完整性、配置、源码，逐标的 bar/factor 读取、bar 校验、因子校验与总回报价构造、涨跌停构造，逐代理对历史拼接，以及共同交易日、模拟和参考比较。`completed` 表示该调用已返回；`partial` 表示已开始但未完成，或仅部分标的/代理对完成；`not_run` 表示尚未调用。分组的 `scopes` 保留每个标的或代理对的状态；不会声称失败调用内部具体检查了多少行。`checks_not_run` 列出尚未完成的阶段，保留 `partial` 与具体范围，而非固定三项。保持首个失败即退出，不枚举尚未检查数据的假想缺口。详情通过 sidecar、任务状态及重启查询保存；诊断完整标志仅表示已知诊断及执行范围完整保存，不表示所有检查已通过。

结果中的 `units` 是旧模拟器的有效总回报指数单位；`holdings` 是相应持仓按模型 NAV 估值的金额，不是总回报指数单位、ETF 实际份额或 CNY。数值边界之外，配置被受理也不保证每次都能执行成功：冻结 `eternal_baseline.py` 的 `rebalance` 使用固定 12 轮费用迭代，合成有效长仓例子在单边费率 `0.001` 收敛，但在 `0.5`、`0.9` 和 `0.9999999995` 下会显式报费用求解不收敛；这会作为运行时失败持久化，不扩大接受验证或更改冻结算法。修改 `eternal_baseline.py` 会使 snapshot 的源码绑定失效，须先获负责人裁决。

实现者在干净代码头使用负责人授权的私有输入运行了真实回放 harness：生产 CLI 默认配置与 5% 变体、expected replay 逐项对比及结果重启读取均通过。默认配置的 `required_default_parity` 另将冻结历史 reference 的日期、NAV 及其中可用的汇总字段作为有限对照；该 reference 不含信号、交易、费用、现金和持仓明细，这些完整结果字段与 #392 expected replay 对照。本次有限复验不表示 #394 整体验收完成，也不证明冻结适配器与旧生产源码等价、历史 PIT/Universe/调整数据覆盖或未触发路径。两次真实运行均未出现 directional-deferral 事件，因此该真实路径仍未验证。完整输入、结果与运行日志仅留本机私有目录，不发布。

合成回归仍覆盖默认/配置变体、阈值信号与下一共同开盘执行、无效配置、缺失输入、子进程失败和重启持久读取；合成结果本身不替代上述受限真实回放，也不代表 #394 整体验收完成。

```sh
python3.12 -m unittest tests.test_eternal_baseline tests.test_eternal_compat -v
cargo test -p quant-research --features app --test eternal_compat_cli --locked --offline -- --test-threads=1
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
