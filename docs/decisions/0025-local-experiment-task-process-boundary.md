# ADR 0025：本机 Experiment 任务与 CLI 子进程边界

- 状态：Accepted implementation contract，2026-10-26（Issue [#287](https://github.com/ZombieIce/PrajnaQuant/issues/287)；架构裁决见 [#286](https://github.com/ZombieIce/PrajnaQuant/issues/286#issuecomment-6412305939)）。

## 背景

MVP-7 的本机研究任务需要复用既有数据、因子与 Experiment 能力，同时保留旧 `quant-research` 诊断 Run 的 JSON、身份和重启行为。新平台 crate 不应作为 `quant-research` 的 Rust 依赖：除了依赖方向约束，Cargo feature 统一也可能改变旧服务的 JSON 语义。服务应通过已发布的 `prajna-experiment` CLI 与新平台边界通信。

## 决定

- 现有 `quant-research` Axum 服务继续拥有本机任务 API、持久化与单 worker 串行队列。旧 Run 与新 Experiment task 共用队列容量和 FIFO 执行；旧 `RunJob` 的结构、状态和 `/api/v1/runs` 路由不变。
- 新任务使用独立类型 `prajna_experiment`、版本 `1`，通过 `POST /api/v1/tasks/experiments` 提交；`GET /api/v1/tasks/experiments` 列出最近任务，`GET /api/v1/tasks/experiments/{id}` 读取单项。提交要求 1–128 字符的幂等键和 JSON 对象定义，可选 `result_level: summary|standard`（缺省 summary）。请求 SHA-256 覆盖序列化后的任务类型、版本、定义及非 summary 的结果级别，不覆盖幂等键；相同键和定义返回既有任务，不同定义/级别返回冲突。旧请求省略级别时 hash、`task.json` 与既有 Summary 任务保持不变。
- 任务保存在配置的输出根目录 `tasks/<task-id>/`：`request.json` 固定提交定义和幂等键，`experiment.json` 是 CLI 输入，`task.json` 保存任务状态、过程组 ID 与本次进程代际 UUID，`result.json` 保存 CLI 的 JSON 结果。结果必须包含字符串 `exp` 与 `exe` 身份。实验产物仍由 CLI 写入同一个配置输出根目录。
- 服务从 `PRAJNA_EXPERIMENT_CLI` 读取可执行文件路径；未设置时在 `PATH` 中查找 `prajna-experiment`。执行 Summary 任务的调用形式为 `run <experiment.json> --lake <output-root> --level summary`；Standard 任务先调用 `promote ... --level standard`，仅在没有当前 Execution（exit 6）时回退为 `run ... --level standard`。级别来自冻结的任务请求。`quant-research` 不直接依赖或编译任何 `prajna-*` crate。
- 在 macOS 与 Linux 上，服务为 CLI 创建独立进程组，并通过 `PRAJNA_EXPERIMENT_TASK_ID` 与 `PRAJNA_EXPERIMENT_PROCESS_GENERATION` 环境标记其任务和本次进程代际。`task.json` 持久化组 ID 与代际 UUID。等待、失败清理、恢复和进程对象异常退出均先检查当前组成员是否同时报告匹配的任务和代际标记，再允许发送信号；组已不存在时视为无需清理，缺少代际、标记不匹配或进程检查失败时拒绝信号并保留组 ID/代际及结构化错误。这样即使旧 PGID 被复用，也不会仅凭数字信号另一任务。其他操作系统当前将 CLI 执行报告为不支持。
- CLI 非零退出的本地失败详情保留退出码（或 Unix signal）及至多 16 KiB 的 UTF-8 替代解码 stderr，并以 `stderr_truncated` 标记超过字节上限。stderr 使用非阻塞读取；CLI 退出且进程组清理检查返回后，若后代仍持有 stderr，最多再读取 200 ms，超时以 `stderr_capture_incomplete` 明示，避免卡住共用串行队列。完整结构化 `RunError` 写入 `task.json`，经任务 GET 和进程重启后仍保留。若进程组所有权检查同时失败，外层错误报告清理失败，`details.cli_error` 保留完整 CLI 错误；重启再次清理时继续保留该嵌套错误，不重复累积。成功结果不包含 stderr；诊断只属于本机任务状态，不进入公开日志或仓库证据。
- 服务恢复时，新任务的 `queued`、`running` 状态变为 `interrupted`，不自动重新排队；其幂等身份仍保留。终态任务保留其状态与结果。旧诊断 Run 的已排队恢复方式保持原样；旧 `running` Run 仍恢复为 `failed`，不复用新任务状态或身份。

## 只读 Dataset 适配（#290）

本机 Dataset API/页面复用同一进程边界原则：服务调用 `prajna-preview-dataset`，不直接依赖新平台 crate。显式 `--dataset-root` 与原有输出/行情根分开，未配置时不可用；只读 CLI 负责正式 Dataset 校验和数据口径，HTTP 保留其版本化 envelope，不发布或采集。具体配置、响应和错误映射见 [API](../api.md#已发布研究-dataset-预览) 与 [读取契约](../contracts/dataset-preview-read.md)。Dataset 路由不继承旧 permissive CORS，按实际绑定 Host/HTTP Origin 限定本机工作台，外部/opaque Origin（含预检）拒绝。旧 Experiment/诊断路由和结果身份保持既有契约。

## 结果读取适配（#339）

`succeeded` 新任务可通过 `GET /api/v1/tasks/experiments/{id}/result` 读取持久结果。服务从 `task.json` 的 `result.exp/exe` 调用 `prajna-experiment result <exp> <exe> --lake <output-root>`，只读 CLI 校验实验/执行身份以及 manifest 中每一张已保存表的 hash，返回 Summary 与已保存 NAV/权益序列的 `schema_version: 1` envelope；不在返回 envelope 中的已保存表也验证，任一 hash 不符即失败。服务逐字节保留 envelope、不重算、不补零，并将该路由放在旧 permissive CORS 之外，按实际绑定 Host/HTTP Origin 限定本机工作台；任务不存在/未成功/结果损坏/CLI 不可用分别映射 404/409/500/503，协议异常 502。成功与错误响应均为 `no-store`。字段和限制见[结果读取契约](../contracts/experiment-result-read.md)。旧诊断 Run、旧任务 JSON 和旧 Summary 结果不变。

`result_level: standard` 任务先调用 `prajna-experiment promote <experiment.json> --lake <output-root> --level standard`：已有 Summary Execution 时只提升仍低于 Standard 的 Completed Run（failed Run 保持原级别，不阻塞），没有可提升的 Completed Run 时以 `action: replayed` 成功返回（含全 failed，任务 succeeded 并保留 failed_count；返回前仍校验 Summary/已保存表，损坏则失败）；选中 Completed Run 在重放中失败则整批中止、不发布 partial 表。没有当前 Execution（目录不存在，promote exit 6）时才回退到 `run ... --level standard`；Execution 目录存在但 manifest 缺失/损坏、no-op 校验失败或其他 promote 失败都不回退。`default_result_level` 保持 Execution 创建时的级别，重复 Standard 任务仍由 promote 的 no-op 路径处理。服务仍不直接依赖新平台 crate。

## 冻结选择适配（#340）

`POST /api/v1/tasks/selections` 调用独立 `prajna-freeze-selection` CLI，从固定 DSV/证券冻结显式 buy-and-hold 定义后复用原 T01 提交，服务仍不编译新平台 crate。提交要求 dataset-root 与 output canonicalize 相同，避免 T01 的 `--lake <output>` 读取另一数据根；不复制/迁移 Dataset。默认版本、选择证据与拒绝条件、响应及原始价格研究限制见[契约](../contracts/single-security-selection.md)。新路由按本机 Host/Origin 限定，位于旧 permissive CORS 外；原任务接口/身份和旧诊断保持不变。

## 研究池适配（#292）

`GET/POST /api/v1/research-pools` 复用子进程边界调用 `prajna-pools`，本机 Host/Origin/no-store 与 Dataset 路由一致；新平台 CLI 拥有池发布、既有 StaticUniverse 身份及本机持久化，旧服务只适配命令/envelope。冻结选择可显式引用已发布池版本，原单证券身份与旧 UUID Universe 不变；范围与错误见[研究池契约](../contracts/research-pools.md)。

## 后果与限制

此边界默认执行并读取 Summary 结果，显式请求 Standard 时会提升已有 Summary Execution；不把新平台核心编译进旧服务，也不修改已有 Experiment 身份或结果表内容（提升会按已有的显式能力补写明细；manifest 的默认级别不改变）。结果读取验证所有已保存表 hash，但只返回 Summary 与 NAV/权益表，不提供 orders/fills/权重明细、分页或按需提升；T01 尚不提供 HTTP 取消或重试端点。持久过程组身份与清理原语为后续取消路径提供服务端能力。进程检查与 CLI 诊断验证使用合成输入，不证明真实行情、策略收益或完整 MVP-7 产品流程；受限沙箱可能无法检查或终止子进程组，此时实现拒绝清理并明确报告未验证的所有权。
