# ADR 0013：POC-0 B2 Fast Event / Nautilus 同口径重测

- 状态：Accepted，预登记保存于 2026-09-28，revision `9c487f3ee60e2f8bbfc38b543b3b3cfd6b00bf2d`；正式同口径重测尚未开始
- 部分修订：`reject` 映射由 [ADR 0014](0014-b2-correctness-failure-attribution.md) 修订（测量后制定，须先归因）；本文正文保持预登记原文。
- 关联： [B2 matched remeasure spec](../../.scratch/poc-0-b2-matched-remeasure/spec.md)、[票据 01](../../.scratch/poc-0-b2-matched-remeasure/issues/01-preregister-matched-boundary-and-prediction.md)、[票据 09](../../.scratch/poc-0-benchmark/issues/09-b2-strategy-parity-and-throughput.md)、[ADR 0012](0012-poc0-nautilus-status-gate.md)

## 背景

票据 09 的 S2 Momentum Rotation 与 S3 MA20/60 共同子集正确性已通过，但性能仍为 `unresolved`：Rust 两线程复用准备好的 bars，Nautilus worker 每 Run 做 quote 转换和引擎初始化；两侧并行模型不同，RSS 归因范围也不同。此前样本不能用于应用票据 09 的相对门槛。本 ADR 在新的正式测量之前固定重测协议、数据边界和判定映射。票据 09 的门槛数值原样保留。

## 决定

### 比较范围与输入

- 判定负载是票据 09 的 S2 3 instruments × 10 sessions（Dataset Version `poc0.synthetic.etf-daily.v1`，SHA-256 `8c16742a2031cab19e08456dbbe009769b02f0331276618831cefdb6827b5a6f`）及 S3 3 instruments × 130 sessions（SHA-256 `a605295dd53e2f7e35758622671e2b3b85b35a6a791d42c850aeee55145311ef`）。
- 稳健性负载为 B3 已固定的 `poc0.b3.s2-scale-64x252.v1` 与 `poc0.b3.s3-scale-64x252.v1`，参数按各自 Dataset/Strategy 契约保存，不得与小负载参数混称。只有判定负载和稳健性负载各自通过共同子集 correctness gate 后，才能采集对应性能。
- 两候选使用相同 Dataset Version、策略参数、初始资金、手数、滑点和最低佣金。沿用 T 日收盘形成信号、下一可用日 open 执行；未来收益只作评价标签。S2/S3 各自沿用票据 09 的 fixture 与独立预期。64×252 correctness 以 Rust Fast Event 投影对拍 Nautilus Adapter 投影及 B3 固定 stress checksum；该负载没有独立手算金标准，报告必须披露这一证据强度。
- 停牌/执行状态导致的原生订单事件和生命周期遵循 ADR 0012：单列 Adapter 项目事件与 Nautilus 原生提交，只记录、不参与共同子集 correctness 或引擎判定。报告仍需明确本结论不裁决停牌下的原生生命周期语义。

### 计时与并行

- **主计时（判定用）：**每个 worker 内先准备一次输入。Rust worker 持有已准备 bars 与配置；Nautilus worker 持有已转换 QuoteTick、已加入 venue/instrument/数据的引擎。每 Run 计入策略实例创建、因子/信号计算、事件处理、账户推进和结果投影生成；不计投影序列化或 checksum。两候选按此边界计时。
- Nautilus worker 对每 Run 重置引擎并加入新的策略实例。若固定版本下重置所得投影不能与新建引擎一致，回退为缓存 QuoteTick 转换结果、每 Run 新建引擎；报告记录实际模式、测试证据和回退原因。
- **端到端次级计时（不参与判定）：**从共同规范 bars 开始，计入各候选的数据转换、引擎/上下文初始化和 Run 执行。与主计时分别报告。
- 单 Run 延迟在单 worker 串行样本中报告 median 与 p95。并行 Runs/s 以 2 workers 的固定 Run 总数墙钟时间计算，不计 worker 池启动或一次性数据下发。events/s 用每 Run 合成 quote 事件数和主计时延迟推算，标为派生值。
- 每 worker 预热 2 次；串行采样 20 次；并行采用 2 workers × 6 Runs、5 个重复组。每组开始前交替 Fast Event 与 Nautilus 的先后顺序。Rust 使用原生线程，Nautilus 使用原生进程 worker。每 Run worker 仅回传 Run 序号、耗时和 checksum；完整投影留在 worker 内。

### RSS 与资源归因

- golden 预检在独立进程运行。测量负载进程只做负载运行和 checksum 校验。
- RSS 比较值为测量负载全部进程的峰值之和：Rust 单进程；Nautilus 两个 worker 之和；不计编排进程。该合计标记为上界，因为各 worker 峰值可能不同时发生。另报每 worker 峰值和空载基线（Rust 空基准二进制；Python 导入 Nautilus 后）。macOS `ru_maxrss` 按平台单位转换并保存原始值/单位。
- 每次 release 构建前、后执行 10 GiB 可用空间闸门并记录命令、时间、空间快照、构建耗时和共享 `target/` 字节增量。资源不足或闸门阻断的必需测量记为缺失，不能套用门槛。冷构建保持 Unknown，除非单独满足资源与缓存隔离条件；不得清理或覆盖共享缓存来制造冷样本。

### 预登记门槛与判定映射

票据 09 的门槛数值不变：Fast Event 在 **S2 与 S3 两者**上都须满足中位单 Run 延迟不高于 Nautilus 的 `1/2`、2-worker 并行 Runs/s 不低于 Nautilus 的 `2×`，并且测量范围匹配时峰值 RSS 不高于 Nautilus；两策略共同子集正确性均须通过。

- `adopt`：判定负载 S2、S3 的正确性、延迟、并行吞吐、RSS 全部通过；稳健性 S2、S3 也通过，或因记录清楚的资源/适配限制未运行并明确标为“稳健性未验证”。
- `defer`：必需的判定负载双方均已按协议测量且未达到任一门槛；或判定负载通过但稳健性负载测出未通过。稳健性未运行本身不得写成稳健性通过。
- `reject`：仅在 ADR 0012 排除范围以外，有可复现的共同子集 correctness 失败。
- `unresolved`：必需判定测量缺失、10 GiB 资源闸门阻断、双方计时/RSS/并行边界无法按本 ADR 对齐，或正确性 gate 未能完成。性能采集不得越过失败的 correctness gate。

维护成本只记录、不设门槛且不参与判定：Fast Event POC 路径与 Nautilus Adapter 非测试代码行数、测试数、新增直接依赖数（Cargo crate / Python package）、Python 传递依赖解析数，以及已知不可消除语义差异数。所有数值说明统计路径与工具；依赖快照不等同完整 lock。

## 先验预测（不参与判定）

基于票据 09 已有分段数据，预测同口径重测中 Fast Event 在判定负载 S2、S3 均会达到上述延迟和吞吐速度门槛，且 Rust 的 RSS 合计会低于 Nautilus。依据是既有样本中 Nautilus S2/S3 事件处理 median 分别约 507 µs / 3.53 ms，而 Fast Event 每 Run median 约 20.5 µs / 140 µs；Nautilus 事件处理段本身约为 Rust 整次 Run 的 25 倍。旧 RSS 报告约为 Rust 全进程 30 MB（含 golden 预检）与 Nautilus 单 worker 75–77 MB，方向上预测 Rust 更低。

这些测量的计时边界和 RSS 归因并不匹配，数值仅支持“先验预测”的书面登记，不是新门槛、性能比值或结论证据。重测报告须逐项说明预测是否成立；若不成立，解释新口径如何改变观察结果。维护成本代理指标不参与先验预测或采纳门槛。

## 测量前保存规则

本 ADR 与票据 01 的预测已在 revision `9c487f3ee60e2f8bbfc38b543b3b3cfd6b00bf2d` 保存，早于首次正式 release 测量。此处协议不得依据正式重测结果追溯修改；若需变更，另立新 ADR 并将受影响结论标为未按原预登记协议测量。

## 适用范围

结论只覆盖本 ADR 指定的合成负载、Fast Event/Nautilus 实现版本、保存的依赖身份与测量机器；不构成生产 Engine 选型、真实 ETF 业绩/PIT 证明或真实执行可行性验证。B2 的 defer/unresolved 不阻塞 Domain Core、Data Lake 与 Vector 阶段；是否开始 MVP-3 由项目负责人按测量结果另行决定。
