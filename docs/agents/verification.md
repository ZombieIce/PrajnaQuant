# 定向验证规则

按受影响入口与 Issue/spec 的验收映射选择验证，交接记录实际命令、结果及限制。代码与测试先读后跑；纯文档只检查 diff、引用文件/链接、规则一致性与变更技能的元数据，无需编译或真实采集。

## 本机命令

Rust 代码改动运行 `cargo fmt --all -- --check`，优先受影响 crate：

```sh
cargo test -p <crate> --locked --offline
cargo clippy -p <crate> --all-targets --locked --offline -- -D warnings
```

按需选择 feature。仅在票据明确要求、跨 crate 变更或定向验证不能覆盖风险时运行全 workspace，并在交接说明原因。只能在 `app` 下编译的测试/example 必须加 feature gate；测试不依赖未入库本地产物或特定工作目录。

Python 使用 3.12；创建 venv 用 `python3.12 -m venv .venv`，测试前确认解释器版本。仓库原型按范围运行 `.venv/bin/python -m unittest discover -s tests -v`；其他 Python 路径运行相关测试。PyO3 显式设置 `PYO3_PYTHON`，本机 `prajna-experiment` 测试设置 `PRAJNA_TEST_PYTHON` 指向该 3.12 解释器。进程控制测试由 CI 覆盖，沙箱内跳过不算失败，但不视为已验证。

修改 `prajna-py` 或 `python/prajna` 时，先按 [prajna-py README](../../crates/prajna-py/README.md) 激活 3.12 venv、安装其指定 maturin，再从仓库根执行：

```sh
export PYO3_PYTHON="$VIRTUAL_ENV/bin/python"
export PRAJNA_TEST_PYTHON="$VIRTUAL_ENV/bin/python"
python --version
cargo test -p prajna-py --locked --offline
cargo clippy -p prajna-py --all-targets --locked --offline -- -D warnings
maturin develop --locked --offline
cargo build -p prajna-experiment --bin prajna-experiment --example prepare_fixture --example acceptance_fixture --example pit_fixture --locked --offline
python -m unittest discover -s python/tests -v
```

上述 fixture/CLI 构建是 Python 集成测试所需输入，不能以缺少未构建产物的跳过代替通过。关联 Experiment/B3 或 Backend 改动再按 README 与 Issue 选择相应验证；依赖缺失、离线 cache 不全或环境不支持时明确报告未验证范围。

前端改动在 `apps/web` 运行 `npm run build`。环境缺依赖则按 lockfile 准备，失败不改称通过。

## CI 来源

[CI workflow](../../.github/workflows/ci.yml) 的 `lightweight` 是合并必需检查；它包含当前新平台 crate 的定向 lint/test、依赖图守卫、Python/PyO3、合成 POC 与 feature 类型检查，并非只测旧 quant-research。复现时读取受影响 steps 并在本机 Cargo 命令加 `--offline`，不要维护另一份完整 CI 命令清单。

[Full workflow](../../.github/workflows/full.yml) 由手动/每晚触发，不阻塞合并；workspace（bundled DuckDB）、B3、Nautilus、PyArrow 和前端等任务的实际命令、平台与不允许跳过的门禁以 workflow 为准。Issue 明确要求的验证即使不在必需 CI 中仍须完成。轻量 CI 设计内跳过 Nautilus 不证明真实 Backend 通过。

## 量化验证与证据

触及研究/执行/账户路径时，按影响范围复用独立合成金标准及 PIT/公司行动对拍：

- `poc/mvp1-golden/`、`poc/mvp3-golden/`；
- `crates/prajna-research/tests/pit_s2.rs`、`crates/prajna-research/tests/corporate_actions.rs`（3 ETF × 10 Session）。

核查 look-ahead、日期对齐、含应收的账户恒等式和成本。合成覆盖、真实已触发事件与未触发路径分别报告，真实证据按 AGENTS/当前 spec 只留本机，公开使用合成输出与允许的文字结论。formatter/lint 或编译不能替代验收行为的验证；环境阻塞时报告缺口与下一步，必需验证未完成不称可合并。
