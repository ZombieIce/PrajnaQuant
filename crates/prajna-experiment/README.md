# prajna-experiment

MVP-2 合成数据 Vector Experiment CLI，复用 Rust 的执行、指标和 ResultLevel 存储契约（ADR 0017/0018）。没有真实市场/PIT 或现金账本守恒结论。

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
