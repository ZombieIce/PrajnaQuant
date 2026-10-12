# prajna-experiment

合成数据 Vector / Fast Event Experiment CLI，复用 Rust 策略、指标与 ResultLevel 存储契约（ADR 0017/0018/0019）。Fast Event 提供运行内精确账本验证；不建立真实市场/PIT 结论。

```text
prajna-experiment run <experiment.json> --lake <dir> [--level summary|standard|full]
                      [--promote <run-id>...] [--threads N]
prajna-experiment diff <exe-a> <exe-b> --lake <dir>
```

`--level` 默认 `summary`；省略 `--threads` 时使用 Rayon 默认线程数（包括 `RAYON_NUM_THREADS`），显式值必须大于零。线程数、ResultLevel 不进入身份。CLI 从当前 Git 工作树捕获代码来源；lake 和定义可以用任意绝对路径。脏工作树照常运行，`reproducible=false`。

不传 `--promote` 时，新建或重放当前 Execution。传入 `--promote` 时只提升已存在的当前 Execution，不先新建；须同时指定高于各选中 Run 现有等级的 `--level standard|full`。缺失 Execution、未知/重复 Run 或未提高等级均报错；未选中的 Run 不受影响。提升验证 Summary 和已有明细表，补写缺失表。

## 最小示例

从仓库根目录执行，需 Rust、Git、Python 3。示例在临时本地 clone 中记录来源，输出和定义在 clone 外，避免入库文件使工作树变脏。`prepare_fixture` 发布已入库的 **64×252 v2 fixture**（normalizer `synthetic-etf-daily@3`），从 panel 的全部证券保存 Static Universe，再写两个 Run 的 `experiment.json`。

```sh
repo=$(pwd)
cargo build -p prajna-experiment --bin prajna-experiment --example prepare_fixture --locked --offline
# 使用自定义 CARGO_TARGET_DIR 时，把下面路径改为其 debug 目录。
bin="$repo/target/debug/prajna-experiment"
prepare="$repo/target/debug/examples/prepare_fixture"
tmp=$(mktemp -d)
git clone --quiet --local "$repo" "$tmp/code"
"$prepare" "$tmp/lake" "$tmp/experiment.json" > "$tmp/definition.json"
cd "$tmp/code"

"$bin" run "$tmp/experiment.json" --lake "$tmp/lake" > "$tmp/first.json"
"$bin" run "$tmp/experiment.json" --lake "$tmp/lake" --threads 1 > "$tmp/replay.json"
# action 分别为 created / replayed，exp 和 exe 相同。

exe_a=$(python3 -c 'import json,sys; print(json.load(open(sys.argv[1]))["exe"])' "$tmp/first.json")
run_id=$(python3 - "$tmp/first.json" "$tmp/lake" <<'PYCODE'
import json, pathlib, sys
result = json.load(open(sys.argv[1]))
manifest = pathlib.Path(sys.argv[2]) / "experiments" / result["exp"].split(":")[-1] / "executions" / result["exe"].split(":")[-1] / "execution.json"
print(json.load(open(manifest))["runs"][0]["run_id"])
PYCODE
)
"$bin" run "$tmp/experiment.json" --lake "$tmp/lake" --level full --promote "$run_id" > "$tmp/promote-a.json"

# 仅在临时 clone 创建空提交：源码不变，但 git revision 改变，产生第二个 Execution。
git -c user.name=Fixture -c user.email=fixture@example.invalid -c commit.gpgsign=false commit --quiet --allow-empty -m "Second fixture execution provenance"
"$bin" run "$tmp/experiment.json" --lake "$tmp/lake" > "$tmp/second.json"
exe_b=$(python3 -c 'import json,sys; print(json.load(open(sys.argv[1]))["exe"])' "$tmp/second.json")
"$bin" run "$tmp/experiment.json" --lake "$tmp/lake" --level full --promote "$run_id" > "$tmp/promote-b.json"
"$bin" diff "$exe_a" "$exe_b" --lake "$tmp/lake" > "$tmp/diff.json"
# diff 退出 0：同一 exp，Run/指标/已保存明细一致。
cd "$repo"
```

新 Execution 保存于 `<lake>/experiments/<exp-hex>/executions/<exe-hex>/`。Summary 不创建 `runs/`。Standard 添加 `sessions`、`executions`；Full 再添加权重、排名及 pending 表。每 Run 的 manifest 区分未保存与 Engine 不产生的表；不把缺表解释为零。

## stdout JSON 与退出码

`run` 成功只输出一个 JSON 对象：`exp`、`exe`、`reproducible`、`run_count`、`failed_count`、`action`（`created`/`replayed`/`promoted` 三选一）、本次线程数、`timings_ms`（定义校验/展开、因子阶段、Run 阶段、总计）、`cache.compute_count`/`hit_count`、`written_bytes`/`written_files`。

计数和因子/Run 耗时来自**本次调用**；重放不会复用原 manifest 的旧耗时。总耗时还含数据加载、来源捕获、存储/重放校验等工作。写入量只统计本次发布的 Experiment/Execution 定义、Summary 和明细表；替换的提升 manifest 算一次，排除 Factor Cache、锁文件和临时校验写入。普通重放的发布写入量为零。manifest 保留首次运行的线程数、起止时间和阶段耗时。

`diff` 输出 #102 的按 Run 对齐报告：只出现在一侧的 Run、status/pending 变化、各指标差值（右减左）、明细表 hash 差异及首个不同的 key/字段。额外的 `summary_changed` 比较两份 Summary logical hash，覆盖数值/null、null 原因和回撤峰谷等变化。只在一侧存在的明细表也算差异；比较同等级结果可避免单纯由提升造成的差异。跨 Experiment 拒绝，不输出成功报告。

| 退出码 | 含义 |
| --- | --- |
| 0 | 成功；个别 Run failed 仍完成 Execution，在报告中计数 |
| 1 | I/O、来源捕获、执行/存储错误、Execution 不存在或身份格式错误 |
| 2 | CLI 参数、定义/展开错误、非法提升请求 |
| 3 | 重放不一致或已保存结果损坏 |
| 4 | 同 Experiment diff 有差异（stdout 保留差异 JSON） |
| 5 | 跨 Experiment diff |

运行 `--help`、`run --help` 或 `diff --help` 也可查看退出码。错误诊断写 stderr；错误路径不输出成功 JSON。

## 验证

```sh
cargo fmt --all -- --check
cargo clippy -p prajna-experiment --all-targets --locked --offline -- -D warnings
cargo test -p prajna-experiment --locked --offline
```

轻量 CI 已运行该 crate 的 all-targets clippy 与完整测试，包含 CLI 集成测试。测试直接启动已编译二进制，用入库 fixture 和独立临时 lake/Git 仓库覆盖新建、重放、提升、diff、损坏结果及退出码；不依赖调用者工作目录或未入库的数据产物。

## Fast Event 与三种策略（MVP-3）

`engine` 接受 `vector` / `fast_event`（语义版本 `vector@1` / `fast_event@1`）。
`strategy` 接受 `s2_rotation`（TopKRank）、`buy_and_hold`、`ma_crossover`。
S2 的九个参数及默认 Run Spec 字节保持不变；显式 `unfilled_entry: "skip"`
与缺省同一身份，`retry` 进入身份。BuyAndHold 固定 retry；MA 默认 skip，也可显式 retry。
新策略同样支持 `parameter_space.list` 与 `grid`：

```json
{"strategy":"buy_and_hold","parameter_space":{"list":[{"params_version":1}]}}
```

```json
{"strategy":"ma_crossover","parameter_space":{"grid":{"params_version":[1],"short_window":[20],"long_window":[60]}}}
```

Fast Event 必须增加以下配置；金额/数量用十进制字符串，规范化为 Domain scale-18
数值的唯一字符串，不经过 f64。比例成本沿用顶层五项 `costs` 的 f64 输入及 bits 身份，
执行时将其最短十进制表示转为 Domain 定点值（bps 用定点除以 10000）。

```json
{
  "engine":"fast_event",
  "fast_event":{
    "currency":"CNY",
    "initial_cash":"100000",
    "sizing":"lot",
    "rebalance":"full_target",
    "minimum_commission":"100",
    "portfolios":[{"id":1,"capital":"80000"}],
    "lot_size_source":{"kind":"instrument"}
  }
}
```

每个 VP 使用该 Run 的同一策略/参数与声明 Universe；VP 各自生成独立订单/Fill，
资本只在初始分配，余额为 Unallocated Capital。此输入不提供混合策略 VP 定义。
`lot_size_source.kind=instrument` 从该 DSV 的 Instrument 表读取，缺 lot size 不默认补齐。
也可用 `{"kind":"explicit","values":{"A.SYNTH":"100","B.SYNTH":"100"}}`：
显式值须正数并属于数据集 Venue，lot 模式须覆盖整个 Venue panel（包括非 Universe 标的）。
来源种类与已解析 lot sizes 都进入身份；账户币种须匹配 Universe Instrument quote_currency。
`vector_parity` 下 lot size 和最低佣金不参与成交，但显式配置仍进入 Run Spec。

`rebalance=entry_only` 为 POC 专用，必须 `sizing=lot`、`unfilled_entry=retry`，并显式
增加 `entry_budget=fixed_initial|remaining_cash`；不得暗中特化策略或修改信号时间。
S1 仍首 Session close 决策、下一 open 成交；这里不提供 POC S1 的延迟首次决策变体。
策略版本、sizing、rebalance、entry budget、retry、VP ID/资本、初始资本、币种、
lot 来源/解析值和最低佣金均进入 Run Spec；线程数及 ResultLevel 均不进入身份。

| ResultLevel | Fast Event 保存内容 |
| --- | --- |
| Summary | E11 指标；每 Session open 后和 close 的账户/VP 恒等式仍在 Run 内精确验证；不创建 `runs/` |
| Standard | Summary + `account_valuations`、`portfolio_valuations`：两点现金/持仓/权益、估值价、来源 Session/字段与账户未分配资本 |
| Full | Standard + `orders`（提交订单及 execution gate 阻塞/跳过/retry/pending 尝试）、`fills`（raw/实际成交额、比例佣金、最低佣金补足、税、滑点、现金变动及 F8 费用基数/方向）、`fee_settlements`（费用非零但数量不变的 F8 腿） |

Fast Event 表用稳定 `row_id` + JSON `payload` 的 Parquet schema：所有账本数字为精确
scale-18 字符串；空表仍保存并登记 logical hash。Orders 中 `record_type=attempt`
是执行诊断，不冒充正数量 Domain Order；`record_type=sizing_adjustment` 保存 lot_rounding / insufficient_cash 的请求及实际数量（可为零）；不可交易的持仓/买腿明确标出原因，末日 pending
保留原目标。成交前现金削减/整手裁减另存原因和前后数量；attempt 的 applied 是执行决策目标，
并非承诺的成交数量。完整现金重放需同时计入 Fill 与 fee-only settlement。

Run manifest 的 `accounting_verified=true` 只表示成功 Run 已通过运行内两层、两点的
精确恒等式；失败 Run 保存 `status=failed`、错误和 `accounting_verified=false`，无指标，
其余 Run 继续。`not_saved` 区分 ResultLevel 省略的表，`not_produced_by_engine` 则是
Engine 不产生的表；Vector 保持原有 orders/fills/cash_ledger 不产生声明。
Fast Event Summary 在摘要完成后释放明细，不跨整个 Rayon 扫描保留所有 Run 账本。
提升、重放和 diff 复用 MVP-2 存储规则，已有表与 Summary 必须 logical hash 一致。
重放 Summary 时若已有提升明细，会重新生成所需明细并校验，不能跳过验证。

Fast Event E11 以**账户每日 close 权益 / 初始账户资本**为 NAV，收益从第二 Session
开始，年化按 `sessions_per_year`；成交次数是 VP 执行尝试次数，不是 Fill 笔数。
`total_turnover` 为各 Session raw Fill 成交额（加 fee-only 腿费用基数）/ open 前权益之和；
`total_cost` 为费用 / 同一 open 前权益之和，均为无量纲值。Open 前权益 = open 后权益 +
当次全部费用。精确账本与费用字符串不被指标 f64 替代。Vector 继续使用 ADR 0017 的
open NAV；两者估值点不同，不能直接用 Summary Sharpe 当作引擎对拍。
仅验证合成数据，不建立真实市场、PIT、分红总回报、T+1/涨跌停或实盘结论。
吞吐/RSS 原始证据和复跑命令见 [`poc/mvp3-benchmark`](../../poc/mvp3-benchmark/README.md)。
