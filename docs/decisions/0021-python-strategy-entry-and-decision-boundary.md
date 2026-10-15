# ADR 0021：Python 策略入口与 Decision 边界

- 状态：Accepted as target architecture，2026-10-13（MVP-5 设计，spec [#154](https://github.com/ZombieIce/PrajnaQuant/issues/154)，设计文档票据 [#155](https://github.com/ZombieIce/PrajnaQuant/issues/155)）；Python 入口尚未实现

[`ADR 0001`](0001-computation-ownership.md) 保留 Rust 的研究计算、回测、账户、成本与绩效权威。现有平台策略是封闭的 `VectorStrategy`，三个 Engine 共用 close-session Decision；POC-0 的 `b3-pyo3` 是 Rust 嵌入 CPython 的基准，Nautilus worker 是独立子进程，均不是 Python 研究入口。MVP-5 选择 Python 宿主调用 Rust 扩展，以受可用时刻约束的纯 Decision 函数扩展策略，不新增量化核心或事件接口。

## 决定

### 范围与宿主（#154 P1–P3）

本期 Python 可以定义 Experiment、编写每个决策 Session 调用一次的 Decision 函数，以及只读访问 Dataset Version 面板、Factor Values 和 Run 结果。Python `on_bar` 逐 bar 回调、声明式 Strategy IR 推迟；不允许 Python 策略读取账户或持仓、不允许 Python 计算 Factor Values，也不支持随机策略或接受策略 seed。

Python 是宿主进程，通过 PyO3 extension module 调用 Rust：新 crate `crates/prajna-py`（`cdylib`）配 `python/prajna` 包，使用 maturin 构建 `abi3-py312` wheel，仅做本地 `maturin develop`，不发布 PyPI。绑定层只包装 `prajna-experiment` / `prajna-research` 现有入口，不复制执行逻辑。`quant-research` 的 `b3-pyo3` 保持不动；[`ADR 0020`](0020-accurate-event-adapter-and-ledger-authority.md) 的 Nautilus worker 仍是子进程。

### 唯一策略输出与校验（P4、P8）

Python 策略只输出 **Decision**：目标权重集合加可选排序，不包含订单、成交或账户状态。策略声明 `Vectorizable`，经共用执行层由 Vector / Fast Event / Accurate Event 消费；沿用已有能力路由与 Engine 模式限制，不新增路由，不赋予 Python 策略订单或逐事件 API。

Rust 在执行前校验 Decision：每个权重有限且非负，权重总和 ≤ 1，标的属于该决策 Session 的 Universe。违反任一规则即 Run 失败，不静默修正。Decision 是意图，后续成交、现金余量、数量与费用由 Engine 决定并随结果追溯；Vector 的简化结果不因此获得逐笔账本守恒证明。

### As-of View、纯函数与调度（P5–P7）

调用契约为 `decide(view, params, prev_decision) -> Decision`。Rust 为每次调用构造只读 **As-of View**，采用零拷贝 Arrow 切片，只提供：

- 在该决策 Session 收盘时 `available_at` 已知且已可用的数据；可用时刻未知的数据不进入，除非 Run 显式声明 Availability Assumption。
- Universe 在该时点的成员，以及策略声明的 Bar 列与 Rust Factor Values。
- `lookback_sessions` 窗口内的数据；执行状态也只包含决策时点已知的部分。

不能仅按行情日期截断而忽略字段可用时刻，不暴露之后才可用的数据、账户、持仓或成交反馈。当前 Static Universe 在每个时点均为全部显式成员；这种成员选择不是历史 point-in-time 证明。

决策函数是纯函数；`prev_decision` 仅为策略自身上一次输出，不是账户持仓，不允许跨调用的可变 Python 状态。策略声明 `rebalance_every`，沿用 `TopKRank` 调度语义；Rust 只在这些决策 Session 调用一次。每次必须返回完整 Decision，没有 `None` 或“跳过”语义。

时序沿用现有执行契约：T 日 close 依据当时可用信息产生 Decision → 下一可用 Session open 由共用执行层执行。延期、pending、数量、账本与成本仍属 Engine；`prev_decision` 不因尚未成交而变成实际仓位。本文不变更执行时序。

#### #159 已确认的 As-of 实现边界

项目负责人在 #159 实现中确认：源 Arrow 面板的直接切片保留完整底层 buffer，Python `buffers()` 可读到被排除的数据；因此 Rust 先将允许的行/列裁剪到独立紧凑 buffer，再经 PyCapsule 零拷贝导出到 PyArrow。**源面板裁剪有复制成本**，不是整条路径零拷贝；后续 #162 计时必须包含该成本。此边界限制交给策略的 Arrow 数据，不是禁止受信 Python 自行访问 globals/文件的 sandbox，也不是纯函数检测器。

Python 声明没有唯一 score，故先等到完整 `lookback_sessions` 窗口，且每个声明 Factor 在当前 Session 至少有一个 close 前可用的 `ok` 值；无 Factor 时只需完整窗口。从首个满足条件的 Session 按 Venue Session 下标固定间隔调用，之后即使数据缺失也不跳过；warm-up 未完成则不调用。#159 只生成单 Run Decision 序列，尚不改变任何 Engine 执行、账户或复现身份契约。

### Factor 与只读数据（P9、P14）

Python 只能声明和参数化已注册的 Rust `FactorKind`；Factor Values 只由 Rust 计算。决策函数内的临时指标不是 Factor Values，不进入 Factor Cache。

只读 API 返回 `pyarrow.Table`，经 Arrow PyCapsule 交换；Polars 不是硬依赖。可读对象为：

- Dataset Version 面板，按 DSV、Universe、列与日期范围选择。
- Factor Values；Factor Cache 未命中时由 Rust 计算并写入 Cache。
- Run 结果，按 Experiment / Run 与 ResultLevel 读取；请求未保存的级别或表时返回显式错误，不返回空表。

Python 不写 Lake。只读研究 API 可读取明确选择的历史面板，但策略调用只能拿到受限 As-of View；不能将完整研究面板作为策略输入。

### 身份与 Experiment API（P10、P11）

沿用 [`ADR 0018`](0018-run-spec-experiment-execution-identity.md) 的分层身份：

| 层级 | Python 入口的约束 |
| --- | --- |
| Run Spec | 记录 `strategy_id@version` 与参数；参数是可 JSON 序列化的 dict，由 Rust 规范化。既有数据、Universe、成本、Availability Assumption、Engine 语义及 Backend 身份仍属于 Run 输入；策略源码 hash 不进入 Run Spec。 |
| Experiment | Python 构造的 Experiment 序列化为与 `experiment.json` 相同的规范定义，保持 Grid / List 两种 Parameter Space，以及相同的 Experiment / Run Spec 身份和结果。 |
| Experiment Execution | 在既有代码修订、工作树差异与编译环境之外，记录策略源码 hash、Python 版本与依赖锁。代码改变但策略版本不变时，产生新的 Execution，不产生新的 Run Spec。 |

Notebook 内定义、没有可追溯源文件的函数使 Execution 标为“不可完全复现”，处理同脏工作树。含 Python 策略的 Experiment 由 CLI 运行时，显式报错 `requires Python host`，不静默跳过。ADR 0018 的 Engine 随机性身份规则继续保留，本期 Python 策略自身不引入随机性或 seed。

Experiment 定义的 Python 引用 wire 格式为 `"strategy": "strategy_id@version"`：
两部分非空、不含空白和额外 `@`；内置 Rust 策略字符串、参数规范化与身份不变。
Python 参数为 JSON object，Grid/List 展开后按 Run Spec 身份排序去重。任意嵌套
参数中的有限浮点在 canonical JSON 中编码为 `{"f64":"<16 位 IEEE-754 bits>"}`，
字符串编码为 `{"string":"<原文>"}`；object/array 递归保留结构，null/bool 与
安全整数保持 JSON 类型。这样浮点不会与恰好包含 bits 的字符串或同名 object
碰撞，且 `0.0` / `-0.0` 保持不同身份。整数仍受 restricted-JCS 的 ±(2^53−1)
约束。原始输入 JSON 与 canonical 身份 JSON 分开，后者不是解析器的输入格式。
#158 仅实现定义、身份与 Rust 策略执行；Python 引用的执行及源码/环境 Execution
来源由 #160 落实，不因为能构造引用而宣称完成 P10 的 Python 策略复现。

### 两阶段并行与确定性（P12、P13）

1. 在 GIL 下串行计算所有参数组合的 Decision 序列。
2. 释放 GIL，由 Rayon 并行执行各 Run；Accurate Event 仍使用 worker 进程池。

不让 Rayon 线程直接调用 Python，不采用 free-threading 或子解释器。策略必须确定；同一 Execution 的重放必须逐 Run 一致。两阶段执行依赖 Decision 不读取实际账户路径，不能把第一阶段扩展为预生成订单并开环重放 Accurate Backend；其 Order / Fill / Reject 闭环仍遵守 ADR 0020。

## 被拒绝的备选方案

- **Python `on_bar` 逐 bar 回调**：POC-0 B3 对 64×252 登记负载为 `reject`，平台尚无 Rust 原生 EventDriven 策略接口，不先为 Python 单独冻结事件 API。已有能力标记不等于原生逐事件策略接口。S2 Rust / Python per-bar / Python batch 为 126.04 / 6.67 / 14.48 Runs/s，其他规模保持 `unresolved`；不外推通用 Python/GIL 拐点（[B3 归档证据](../../poc/poc0-benchmark/POC-0-SYNTHESIS.md#b3pyo3)）。
- **整段面板一次调用，无防护或事后检查前缀不变性**：会先暴露未来数据；事后检查不能替代按可用时刻和 Universe 可知性构造的 As-of View，也不能保证任意策略没有使用未来值。
- **Rust 宿主嵌入 CPython，或以子进程协议承载 Python 策略入口**：不符合本期 Python 宿主与本地扩展模块的研究流程，并增加生命周期或协议边界；现有 B3 嵌入基准及 Nautilus worker 保持各自用途，不作为策略入口。
- **Python 计算 Factor Values**：形成第二套因子执行与缓存权威，破坏 Rust Factor 身份、可用时刻和结果契约；Python 内临时指标不冒充 Factor Values。
- **源码 hash 进入 Run Spec**：每次源码变化都会更换配置身份，无法按同一 Run Spec 跨代码比较；源码及环境属于 Execution。
- **Rayon 直接调用 Python，或采用 free-threading / 子解释器**：与本期 GIL 下串行决策、释放 GIL 后并行执行的边界冲突；不以新的 Python 并发模型绕过 B3 的规模限制。

## 后果与未决验证

本文记录已确认的目标契约，不证明 Python 入口、As-of View 防护或策略复现已经实现。后续验收按 [#154 P16](https://github.com/ZombieIce/PrajnaQuant/issues/154)：Python / CLI Experiment 身份和结果等价，Python S2/S3 Decision 与 Rust 完全一致、Vector / Fast Event 结果完全一致、Accurate 一次冒烟，未来数据与非法 Decision 对抗测试，以及源码/环境复现记录。64×252 S2/S3 吞吐只记录、不设门槛，与 B3 并列；路线图 MVP-5 原验收文字保留，逐 bar callback 依本 ADR 推迟。

以下仍为 #154 的 Fog，不在本票冻结：

- 源码 hash 仅覆盖定义模块，还是包含传递导入的本地模块；依赖锁来自 `pip freeze`、`uv.lock` 或其他来源；由两阶段执行票据 [#160](https://github.com/ZombieIce/PrajnaQuant/issues/160) 提出。
- PyO3 / maturin 版本，是否沿用 B3 的 PyO3 0.29.0；由骨架票据 [#156](https://github.com/ZombieIce/PrajnaQuant/issues/156) 决定。
- 每次 As-of View 的 Python 形态与开销；由吞吐票据 [#162](https://github.com/ZombieIce/PrajnaQuant/issues/162) 实测。
- Python 宿主启动 Accurate worker 时的进程与环境隔离；由 #160 核实。

合成接口与语义对拍不构成真实市场、历史 PIT、分红总回报或生产可用性结论。本票不实现代码或修改 STATUS / HANDOFF；轻量 CI 的扩展构建与 Python 测试由后续票据按 #154 P15 落实。


### #160 Execution provenance and host execution (2026-10-14)

The Python host supplies a matching Strategy declaration and pure Decision function
alongside the existing Experiment reference. Canonical deduplicated Run parameter
values are generated serially under GIL; owned Decisions then enter the existing
Rust execution schedule with GIL detached. Routing, next-session open execution,
accounting and ResultLevel storage remain shared. Accurate retains its process pool.

The Execution source hash covers the defining module's entire file. A separate
`marshal.dumps(function.__code__)` hash captures the loaded function, and the
Strategy declaration participates in Execution identity. Transitive local imports,
globals, closures and external data are outside this hash scope; deterministic pure
functions and dependency management remain caller obligations. File-less Notebook
functions are explicitly not fully reproducible. Installed dependencies are the
sorted `sys.executable -m pip freeze --all` snapshot (including editable/direct URL
entries), not a resolver lock or wheel-content hash. Python version is `sys.version`.
Paths/code filenames are identity-bearing. Existing Rust/CLI identity bytes remain
unchanged when optional Python provenance is absent; Python provenance affects only
Execution identity. Replay checks every retained Run table's logical hash.

The specified worker interpreter preserves its venv. Worker processes ignore host
Python overrides/user-site via `-E -s`, use separate process groups and per-Run packet
directories, and inherit no host Python objects. Trusted worker sibling imports and
non-Python OS environment remain available. This is not a security sandbox. A real
Python-hosted Accurate smoke with hostile PYTHONPATH runs in Full only; lightweight
CI intentionally skips that smoke.


### #178 PIT extension (2026-10-15)

The optional MVP-6 path replaces Static membership with the per-Session Historical
Catalog Universe and Rust Eligible Set from ADR 0022. Python receives lifecycle
IDs, known derived statuses/directional gates and optional Rust `etf_tri_momentum`
Values. Only positive targets and ranking entries in Eligible and outside known
risk exclusions are accepted; Catalog-member zero targets permit exit intent
without passing holdings to the callback. Actual exits remain in the common Rust
Engine. PIT parameters are the strict S2 lookback/top_k/rebalance_every object;
the declaration retains its existing full-window and Factor-readiness warm-up,
then schedules by `Strategy.rebalance_every` from the first ready Session.
Readiness uses only current Universe members; later empty Eligible Sessions
do not skip callbacks. The parameter cadence applies to built-in S2, not Python
callbacks, so cross-strategy comparisons must explicitly align Decisions.
This preserves the owner-confirmed Python scheduling contract for PIT and Static.
Compact Arrow export still bounds every row by current membership, window and
actual availability. Matching built-in PIT definitions retain CLI/Python identity
and result parity; Python references retain distinct strategy and source/environment
Execution identities. Only synthetic 6a engineering inputs are validated here.
