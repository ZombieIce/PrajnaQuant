# ADR 0018：Run Spec、Experiment 与 Execution 分层身份

- 状态：Accepted，2026-10-03（MVP-2 设计）；尚未实现

[`ADR 0011`](0011-immutable-data-and-reproducible-runs.md) 要求 Run 可追溯到配置、数据、代码与 seed，但没有固定身份边界。MVP-2 把身份分为三层，均以 restricted-JCS 规范化后取 SHA-256（浮点以 IEEE 754 bit 字符串表示，与 Factor 权重一致）：

- **Run Spec** `run:sha256:<hex>`：DSV、Static Universe 身份、Strategy 及规范化参数、成本、Availability Assumption、Engine 种类与语义版本（如 `vector@1`）、seed（仅当 Engine 使用随机性；否则必须缺省）。不含代码修订、线程数与 ResultLevel。
- **Experiment** `exp:sha256:<hex>`：规范化的 Experiment 定义（固定部分与 Parameter Space）；内容寻址，同一定义重跑得到同一身份。
- **Experiment Execution** `exe:sha256:<hex>`：Experiment 身份、git 修订、工作树 diff hash、rustc 版本与 target triple。身份已存在时重放须逐 Run 一致（结果表逻辑 hash 相同），不一致即报错，不覆盖；工作树不干净时仍落盘但标为不可完全复现。

## Considered Options

- 代码修订进入 Run 身份（ADR 0011 原文字面含义）：每次提交都会使全部 Run 换 ID，无法跨 commit 按同一 Run Spec 对比结果是否变化。改为由 Execution 承载代码身份。
- Experiment 用 UUID：每次执行都是新实验，“固定代码/数据/配置可重放”只能靠人工比对。内容寻址让重放成为存储层的校验。
- 不含 rustc 版本与 target：浮点结果可能随编译器与平台变化，无法区分“代码变了”与“编译环境变了”。
- 用 Parquet 文件 sha256 判定结果相同：违反 Logical Hash 约定；改为扩展逻辑 hash 支持 Float64（按 bits，区分 −0.0，拒绝 NaN）。

## Consequences

ADR 0016 的 Factor Cache 键用 crate 版本而非 git 修订，与此一致：缓存可跨 commit 复用，而结果按 Execution 分开保存。ResultLevel 不进任何身份；在同一 Execution 身份上把 Run 提升到更高等级时补写缺失表，已存在表须重算一致。Engine 语义（如 ADR 0017）变化必须提升 Engine 语义版本，否则相同 Run Spec 会在新 Execution 中出现未解释的差异。
