# 定向验证规则

按受影响入口与 Issue/spec 的验收映射选择验证，交接记录实际命令、结果及限制。代码与测试先读后跑；纯文档只检查 diff、引用文件/链接、规则一致性与变更技能的元数据，无需编译或真实采集。

## 执行角色与证据归属

- **实现者：** 必须运行受影响入口的适用命令、Issue/spec 要求及 Acceptance Mapping 场景；按实际风险在实现前选择关键反例。必需输入先构建，跳过/零匹配/缓存命中不自动证明行为通过。
- **独立 reviewer：** 独立读取原始需求、diff、代码及已有测试，按风险复现关键反例、有疑点的结果和受修复影响的邻近失败路径；不要求机械重跑无关全套。说明所选命令为何覆盖风险，引用未复跑证据时标明来源与限制。关键反例仍缺必要独立验证时保留验证缺口；实现者跑过不能冒充 reviewer 本人复跑。
- **每份证据：** 记录对应 SHA、实际命令（含 feature/过滤条件）、匹配/执行数量、环境、结果与覆盖范围。无过滤则说明执行数量，无数量的文档/静态检查记文件/链接/场景数量或 N/A 原因。零匹配不算通过；跳过项逐项说明。复用缓存时区分构建缓存与实际测试执行，不以更快或少跑推断已验证。
- **引用证据：** 标明执行者、原记录链接、SHA、命令/数量/环境/范围是否仍适用于当前 head；有变化就补受影响证据。只有自己实际执行的命令可称“本人复跑”。覆盖未知标 unverified，不推测匹配数或运行结果。

验证记录是受权限约束的证据索引，真实数据/身份不因上述字段要求进入公开输出，仍遵守 AGENTS 与当前 Issue/spec。首次 review 的完整覆盖及 re-review 的增量格式见 [review-pr](../../.agents/skills/review-pr/SKILL.md)，最终 head 与角色隔离见 [tracker](issue-tracker.md)。

## Prepare 验证能力前检

Prepare 尽早运行 [preflight](workflow-tools.md)：核对 Node/Python/Rust、Python标准库可加载性、浏览器、`ps`/进程组可见性、loopback 端口、磁盘 10 GiB 保留量。给出实际可用环境/执行者或缺口；检查时端口可用不保证后续启动成功，浏览器文件存在不证明可启动。缺权限使用正常审批或具备能力的执行者，不放宽生产进程守卫或绕过沙箱。

涉及产品路径时在实现中尽早实际运行生产入口合成 E2E，前检不替代行为验证。Acceptance Mapping 标明 CI job、命令/feature 实际覆盖；`quant-research --no-default-features` 不编译 app 读取侧，PR `web` job 显式运行 `comparison_api::tests` 和 app 生产浏览器链。其他新增 app 测试依其入口补定向运行，不能因 job 名存在推定已覆盖。

## 最终 HEAD 与影响复用

每次 push（含文档）均须独立增量审查最终 HEAD；不表示所有测试必须重跑。按代码、构建/配置、契约和依赖影响更新验收映射：受影响入口复跑；未变部分引用原执行者、PR记录、SHA、命令/匹配数量、环境及仍适用依据。未知标 unverified，缓存命中不代替实际执行。

纯说明性文档不机械复跑 Golden/E2E；文档含契约、构建或验收变化仍评估行为影响。Issue 明确规定最终 HEAD 必跑项继续执行，除非对应 Issue 获明确变更。逐次执行证据留 PR，STATUS/ADR 保留稳定能力与真实验证边界，不能通过移动证据掩盖不一致。

测试记录入口 `workflow.py run` 强制显式选择、执行者、原始日志位置，记录数量、结果、时间、SHA和环境；零匹配及缺少运行不通过，允许空集必须附原因。Rust 默认 ignored 数不自动失败；unittest/Playwright 跳过须说明适用理由，全部未执行仍不通过。调用者须确认数量覆盖必需选择，工具解析汇总不能推定每个必需场景已运行。

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

[CI workflow](../../.github/workflows/ci.yml) 的 `lightweight` 是合并必需汇总门禁；`core` 保留原验证命令，`web-applicability` 判断新增前端任务适用性，命中时 `web-production-validation` 必须成功。未命中是经批准的 N/A；缺失/失败/未授权跳过不得变绿。它包含当前新平台 crate 的定向 lint/test、依赖图守卫、Python/PyO3、合成 POC 与 feature 类型检查，并非只测旧 quant-research。复现时读取受影响 steps 并在本机 Cargo 命令加 `--offline`，不要维护另一份完整 CI 命令清单。

[Full workflow](../../.github/workflows/full.yml) 由手动/每晚触发，不阻塞合并；workspace（bundled DuckDB）、B3、Nautilus、PyArrow 和前端 build 等任务的实际命令、平台与不允许跳过的门禁以 workflow 为准。Issue 明确要求的验证即使不在必需 CI 中仍须完成。轻量 CI 设计内跳过 Nautilus 不证明真实 Backend 通过。

## 量化验证与证据

触及研究/执行/账户路径时，按影响范围复用独立合成金标准及 PIT/公司行动对拍：

- `poc/mvp1-golden/`、`poc/mvp3-golden/`；
- `crates/prajna-research/tests/pit_s2.rs`、`crates/prajna-research/tests/corporate_actions.rs`（3 ETF × 10 Session）。

核查 look-ahead、日期对齐、含应收的账户恒等式和成本。合成覆盖、真实已触发事件与未触发路径分别报告，真实证据按 AGENTS/当前 spec 只留本机，公开使用合成输出与允许的文字结论。formatter/lint 或编译不能替代验收行为的验证；环境阻塞时报告缺口与下一步，必需验证未完成不称可合并。
