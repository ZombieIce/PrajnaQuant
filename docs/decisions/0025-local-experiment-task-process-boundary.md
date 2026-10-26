# ADR 0025：本机 Experiment 任务与 CLI 子进程边界

- 状态：Accepted implementation contract，2026-10-26（Issue [#287](https://github.com/ZombieIce/PrajnaQuant/issues/287)；架构裁决见 [#286](https://github.com/ZombieIce/PrajnaQuant/issues/286#issuecomment-6412305939)）。

## 背景

MVP-7 的本机研究任务需要复用既有数据、因子与 Experiment 能力，同时保留旧 `quant-research` 诊断 Run 的 JSON、身份和重启行为。新平台 crate 不应作为 `quant-research` 的 Rust 依赖：除了依赖方向约束，Cargo feature 统一也可能改变旧服务的 JSON 语义。服务应通过已发布的 `prajna-experiment` CLI 与新平台边界通信。

## 决定

- 现有 `quant-research` Axum 服务继续拥有本机任务 API、持久化与单 worker 串行队列。旧 Run 与新 Experiment task 共用队列容量和 FIFO 执行；旧 `RunJob` 的结构、状态和 `/api/v1/runs` 路由不变。
- 新任务使用独立类型 `prajna_experiment`、版本 `1`，通过 `POST /api/v1/tasks/experiments` 提交；`GET /api/v1/tasks/experiments` 列出最近任务，`GET /api/v1/tasks/experiments/{id}` 读取单项。提交要求 1–128 字符的幂等键和 JSON 对象定义。请求 SHA-256 覆盖序列化后的任务类型、版本与定义，不覆盖幂等键；相同键和定义返回既有任务，不同定义返回冲突。
- 任务保存在配置的输出根目录 `tasks/<task-id>/`：`request.json` 固定提交定义和幂等键，`experiment.json` 是 CLI 输入，`task.json` 保存任务状态、过程组 ID 与本次进程代际 UUID，`result.json` 保存 CLI 的 JSON 结果。结果必须包含字符串 `exp` 与 `exe` 身份。实验产物仍由 CLI 写入同一个配置输出根目录。
- 服务从 `PRAJNA_EXPERIMENT_CLI` 读取可执行文件路径；未设置时在 `PATH` 中查找 `prajna-experiment`。调用形式为 `run <experiment.json> --lake <output-root> --level summary`。`quant-research` 不直接依赖或编译任何 `prajna-*` crate。
- 在 macOS 与 Linux 上，服务为 CLI 创建独立进程组，并通过 `PRAJNA_EXPERIMENT_TASK_ID` 与 `PRAJNA_EXPERIMENT_PROCESS_GENERATION` 环境标记其任务和本次进程代际。`task.json` 持久化组 ID 与代际 UUID。等待、失败清理、恢复和进程对象异常退出均先检查当前组成员是否同时报告匹配的任务和代际标记，再允许发送信号；组已不存在时视为无需清理，缺少代际、标记不匹配或进程检查失败时拒绝信号并保留组 ID/代际及结构化错误。这样即使旧 PGID 被复用，也不会仅凭数字信号另一任务。其他操作系统当前将 CLI 执行报告为不支持。
- CLI 非零退出的本地失败详情保留退出码（或 Unix signal）及至多 16 KiB 的 UTF-8 替代解码 stderr，并标记截断；stderr 始终被完整排空以避免子进程阻塞。完整结构化 `RunError` 写入 `task.json`，经任务 GET 和进程重启后仍保留。成功结果不包含 stderr；诊断只属于本机任务状态，不进入公开日志或仓库证据。
- 服务恢复时，新任务的 `queued`、`running` 状态变为 `interrupted`，不自动重新排队；其幂等身份仍保留。终态任务保留其状态与结果。旧诊断 Run 的已排队恢复方式保持原样；旧 `running` Run 仍恢复为 `failed`，不复用新任务状态或身份。

## 后果与限制

此边界只执行并读取 Summary 结果，不把新平台核心编译进旧服务，也不修改已有 Run/Experiment 结果。T01 尚不提供 HTTP 取消或重试端点；持久过程组身份与清理原语为后续取消路径提供服务端能力。进程检查与 CLI 诊断验证使用合成输入，不证明真实行情、策略收益或完整 MVP-7 产品流程；受限沙箱可能无法检查或终止子进程组，此时实现拒绝清理并明确报告未验证的所有权。
