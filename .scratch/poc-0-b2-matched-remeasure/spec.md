# POC-0 B2：Fast Event / Nautilus 同口径重测

Status: resolved
Type: spec

**Conclusion:** 2026-09-29 完成，B2 正式结论 `adopt` 及负责人确认的范围见 [POC-0-SYNTHESIS B2 小节](../../poc/poc0-benchmark/POC-0-SYNTHESIS.md)。

## Problem Statement

POC-0 综合结论把 B2（自研 Fast Event 对 NautilusTrader）标为 `unresolved`。原因不是正确性，而是测量口径不一致：S1/S2/S3 共同子集的决策、Fill、现金、持仓、成本和 NAV 已对拍通过，但 Rust 侧两线程复用已准备 bars，Nautilus 侧两个 warm 进程每 Run 重做 quote 转换和引擎初始化；Rust RSS 是含 golden 预检的整进程峰值，Nautilus 是单 worker 峰值。因此票据 09 预登记的门槛（两策略均 2× 更低中位单 Run 延迟、2× 更高并行 Runs/s、峰值 RSS 不更高）无法判定。

项目负责人需要在开工 MVP-3（Portfolio / Fast Event L1）前知道：在同一语义、同一机器、同一口径下，自研 Fast Event 的吞吐优势是否足以承担其维护成本。已有分段计时显示，即使完全去掉 Nautilus 的转换和初始化，其事件处理段仍比 Rust 整次 Run 慢约 15–25×；但这是看过数据后的事后拆分，不能替代预登记协议。

## Solution

提供一个 B2 同口径比较入口。研究者一条命令即可在同一 release 构建和固定 Nautilus 版本下，以相同 worker 数、相同 Run 数、相同计时边界和相同 RSS 归因范围，分别运行 Fast Event 与 Nautilus 的 S2 Momentum Rotation 与 S3 MA20/60。入口先过共同子集正确性门，再采集原始样本，最后按预登记规则输出 `adopt / defer / reject / unresolved`，并附维护成本代理指标。

在任何测量之前，先把统一口径、判定映射和基于现有分段数据的书面预测登记下来。重测结果是唯一判据，预测只用于事后说明结论对口径是否敏感。B2 结论仅作为 MVP-3 开工的前置条件，不阻塞 MVP-1/2。

## User Stories

1. 作为项目负责人，我希望 B2 在同口径下得到可判定的结论，以便决定 MVP-3 是否自研 Fast Event。
2. 作为项目负责人，我希望统一口径和判定映射在测量前登记为 ADR，以便结果出来后不移动标准。
3. 作为项目负责人，我希望票据 09 的门槛数值保持不变，以便新结论与已登记协议一致。
4. 作为项目负责人，我希望在重测前看到一份基于已有分段计时的书面预测，以便事后判断结论是否依赖口径选择。
5. 作为项目负责人，我希望重测报告明确说明预测是否成立，以便识别口径敏感性。
6. 作为项目负责人，我希望 B2 结论不阻塞 Domain Core、Data Lake 与 Vector 阶段，以便关键路径不被 POC 拖延。
7. 作为项目负责人，我希望维护成本以代理指标记录，以便"采用"不只看速度。
8. 作为项目负责人，我希望维护成本代理指标不设门槛、不参与判定，以便不引入缺乏量尺的伪精确标准。
9. 作为 Engine 开发者，我希望两个候选接收相同的 Dataset Version、Strategy 参数、初始资金、手数、滑点和最低佣金，以便比较同一工作负载。
10. 作为 Engine 开发者，我希望两个候选的计时区间都包含策略/因子计算、事件处理、账户推进和结果投影，以便比较真实的每 Run 工作量。
11. 作为 Engine 开发者，我希望两个候选都在 worker 内只准备一次输入数据，以便主口径对应研究漏斗中"同数据、多参数"的扫描场景。
12. 作为 Engine 开发者，我希望 Nautilus 在 worker 内复用已加入 venue、instrument 和数据的引擎，每 Run 重置后加入新的策略实例，以便不把可摊销的初始化计入每 Run。
13. 作为 Engine 开发者，我希望若 Nautilus 固定版本的重置无法得到与新建引擎一致的投影，入口退回到"缓存转换结果、每 Run 新建引擎"并在报告中标明，以便如实计入 Nautilus 在扫描中不可避免的成本。
14. 作为 Engine 开发者，我希望另报每 Run 完整端到端耗时（含输入转换与引擎初始化），以便看到冷路径成本，但它不参与判定。
15. 作为 Engine 开发者，我希望投影序列化和 checksum 计算在计时区间之外，并对双方一致处理，以便不把报告开销计入引擎速度。
16. 作为 Engine 开发者，我希望单 Run 延迟在单 worker 串行下采样，并行 Runs/s 在 2 worker 下以墙钟计量，以便两个指标各自对应登记定义。
17. 作为 Engine 开发者，我希望双方各用原生并行模型（Rust 线程、Nautilus 进程），worker 数与总 Run 数一致，以便比较各自在实际使用中的并行能力。
18. 作为 Engine 开发者，我希望 worker 与主进程之间每 Run 只传递 Run 序号、耗时和 checksum，以便 IPC 开销不被完整投影放大。
19. 作为 Engine 开发者，我希望 worker 池启动和一次性数据下发不计入并行墙钟，以便与 Rust 线程创建对等。
20. 作为 Engine 开发者，我希望预热、串行样本数、并行重复组数固定且在报告中保存每个原始样本，以便统计量可复核。
21. 作为 Engine 开发者，我希望在重复组之间交替候选的运行顺序，以便降低热状态和系统噪声的顺序偏差。
22. 作为 Experiment 维护者，我希望 RSS 在只运行测量负载的独立进程中采集，golden 预检在另一进程完成，以便 RSS 不含预检。
23. 作为 Experiment 维护者，我希望 RSS 比较对象是"执行测量负载的全部进程峰值之和"，Rust 为单进程，Nautilus 为 2 个 worker 之和，编排进程不计入，以便两边归因范围一致。
24. 作为 Experiment 维护者，我希望同时记录每 worker 峰值与 Python/Nautilus 空载基线 RSS，以便解释差异来源。
25. 作为 Experiment 维护者，我希望峰值之和明确标为上界（各 worker 峰值不一定同时出现），以便不夸大并发内存。
26. 作为 Experiment 维护者，我希望报告记录 CPU、核数、内存、OS、rustc、Python、Nautilus 版本、Cargo.lock hash、二进制 hash、代码 revision 与脏工作树状态，以便同条件复跑。
27. 作为 Experiment 维护者，我希望 Python 传递依赖的解析快照随报告保存并注明未形成完整 lock，以便复现限制可见。
28. 作为 Experiment 维护者，我希望构建前执行 10 GiB 空间闸门并单列构建耗时与 target 增量，以便资源限制不伪装为性能结论。
29. 作为量化研究者，我希望判定负载仍是票据 09 的 S2 3×10 golden 与 S3 3×130 MA20/60 fixture，以便与已登记协议一致。
30. 作为量化研究者，我希望另以预登记的 64×252 S2/S3 数据集做稳健性检查，以便看到 Nautilus 固定开销被摊薄后的比例。
31. 作为量化研究者，我希望稳健性负载与判定负载结论不一致时整体判为 `defer`，以便小负载结论不被外推。
32. 作为量化研究者，我希望稳健性负载因资源闸门或适配限制无法运行时，报告保留判定负载结论并标明"稳健性未验证"，以便不把缺失测量当作通过。
33. 作为量化研究者，我希望 64×252 负载的正确性门以 Nautilus 共同子集投影对拍 Rust Fast Event 投影（Rust 已由 B3 stress checksum 固定），并注明该负载没有独立手算金标准，以便正确性依据的强度可见。
34. 作为量化研究者，我希望任一候选在任一负载上共同子集对拍失败时停止该负载的性能采集，以便不比较快而错的实现。
35. 作为量化研究者，我希望触及停牌状态的场景继续按 ADR 0012 分列"Adapter 项目事件"与"Nautilus 原生提交"，且不因此拖累其余字段，以便沿用已划定的比较边界。
36. 作为量化研究者，我希望 T 日收盘形成信号、下一可用日 open 执行的时序在两个候选中保持不变，以便重测不引入新的未来函数风险。
37. 作为审阅者，我希望报告把正确性结果、原始测量、判定、预测对照和维护代理指标分节列出，以便独立核查。
38. 作为审阅者，我希望结论只覆盖测量过的负载、版本和机器，并声明不构成生产 Engine 选型或真实 ETF 业绩验证，以便不被过度引用。
39. 作为审阅者，我希望综合报告、STATUS、HANDOFF 和票据 09 在重测后同步更新，以便证据链一致。

## Implementation Decisions

- **唯一新增接缝：B2 同口径比较编排入口。**它以独立子进程启动 Rust 侧 B2 测量 CLI 与 Nautilus worker，汇总为一份比较报告。判定规则实现为纯函数：输入为双方各负载的正确性状态、原始样本和 RSS 记录，输出结论与理由。编排入口使用 Python，因为 Nautilus 候选只能在 Python 中运行；Rust 仍是账户与结果权威，编排入口只读报告、不重算账户。
- **沿用的接缝：**Rust `benchmark-poc0-b2` CLI 与 Nautilus Adapter。只增加统一边界所需的输入：Dataset Version/路径（支持 64×252 数据集）、串行/并行模式、worker 数、Run 数、重复组数、是否跳过进程内 golden 预检（仅在编排入口已于前一进程完成预检时使用）。不为测试暴露内部对象。
- **预登记产物（测量前完成）：**新 ADR 记录统一计时边界、并行模型、RSS 归因、负载与判定映射；门槛数值原样引用票据 09，不修改。同时在对应票据中写入书面预测：依据已有分段数据，预测两策略在判定负载上均达到速度门槛（Nautilus 仅事件段已比 Rust 整 Run 慢约 15–25×），RSS 预测 Rust 更低。ADR 与预测须先于首次正式测量提交或保存。
- **主计时边界（判定用）：**worker 内一次性准备输入。Rust 为已准备 bars 与配置；Nautilus 为已转换 QuoteTick，以及已加入 venue/instrument/数据的引擎。每 Run 计时区间包括：策略实例创建、因子/信号计算、事件处理、账户推进和结果投影生成；不包括投影序列化与 checksum。Nautilus 每 Run 先重置引擎再加入新策略实例；若重置后投影与新建引擎不一致，则改为"缓存转换、每 Run 新建引擎"，并在报告中记录所用模式与原因。
- **次级计时边界（仅报告）：**每 Run 端到端，从共享的规范 bars 开始，包含候选各自的输入转换、引擎/上下文初始化和运行。
- **指标定义：**单 Run 延迟为单 worker 串行样本的中位数与 p95；并行 Runs/s 为 2 worker 下固定 Run 数的墙钟吞吐，不含 worker 池启动和一次性数据下发。events/s 由每 Run 合成 quote 事件数与延迟推得。预热每 worker 2 次；串行样本 20 个；并行为 2 worker × 6 Run，重复 5 组。组间交替候选顺序。
- **并行模型：**Rust 用同进程线程，Nautilus 用进程池，均为 2 worker。每 Run 结果只回传序号、耗时和 checksum。
- **RSS 归因：**golden 预检在单独进程完成；测量进程只做负载运行与 checksum 校验。比较值为执行负载的全部进程峰值之和（Rust 1 进程；Nautilus 2 worker 之和，不含编排进程），标为上界。另报每 worker 峰值与空载基线（Rust 空二进制启动、Python 导入 Nautilus 后）。macOS 下以子进程 `ru_maxrss` 计量，单位按平台换算。
- **负载：**判定负载沿用票据 09 的 S2（3×10 golden，Dataset SHA-256 `8c16742a…`）与 S3（3×130，SHA-256 `a605295d…`）。稳健性负载为 B3 已固定的 `poc0.b3.s2-scale-64x252.v1` 与 `poc0.b3.s3-scale-64x252.v1`，采用相同门槛。
- **判定映射（沿用票据 09 并补充稳健性）：**
  - `adopt`：判定负载上 S2、S3 均满足中位单 Run 延迟 ≤ Nautilus 的 1/2、并行 Runs/s ≥ Nautilus 的 2×、峰值 RSS 之和 ≤ Nautilus，且共同子集正确性全部通过；稳健性负载同样满足，或因资源/适配原因未运行（标"稳健性未验证"）。
  - `defer`：双方在匹配口径下均已测得，但判定负载未达门槛；或判定负载达标而稳健性负载未达标。
  - `reject`：仅在 ADR 0012 排除维度以外出现可复现的正确性失败，**且诊断证据归因于 Fast Event 违反独立期望**（[ADR 0014](../../docs/decisions/0014-b2-correctness-failure-attribution.md)，看到票据 04 结果后修订）。未归因的跨候选差异判 `unresolved` 并标 `attribution_required`。
  - `unresolved`：任一必需的判定负载测量缺失、资源闸门阻断，或口径无法按 ADR 对齐。
- **维护成本代理指标（只记录）：**Fast Event POC 路径与 Nautilus Adapter 的非测试代码行数、测试数、新增直接依赖数（Cargo crate / Python 包）、Python 传递依赖解析数，以及已知不可消除语义差异数。
- **构建：**使用 `quant-research --no-default-features` release 与共享 workspace target；构建前后执行 10 GiB 空间闸门并记录耗时与 target 增量，冷构建仍为 Unknown。
- **收尾同步：**更新 POC-0 综合报告的 B2 行、票据 09 状态、STATUS 与 HANDOFF；路线图把 B2 结论标为 MVP-3 前置条件。

## Testing Decisions

- 好的测试只断言外部行为：给定固定输入与配置，比较报告中的正确性状态、原始样本结构、归因范围标签、判定与理由；不断言候选内部类型、调用次数或绝对耗时。
- **判定纯函数单元测试**：用手写的固定测量记录覆盖 `adopt`、速度未达门槛的 `defer`、RSS 未达门槛的 `defer`、稳健性不一致的 `defer`、稳健性未运行的 `adopt` + 标签、未归因正确性失败的 `unresolved` + `attribution_required`、缺少 RSS/并行测量的 `unresolved`，以及 ADR 0012 排除维度不一致但其余字段通过的情形。边界值（恰好 2×、RSS 相等）须有用例。
- **编排入口集成测试**：用小规模参数运行，断言正确性门失败时不采集该负载性能；报告包含两种计时边界、所用 Nautilus 重置模式、每 worker 与合计 RSS、预测对照节和维护代理指标节。环境缺少固定 Nautilus 版本时跳过，并在报告中标 `unresolved`。
- **Rust CLI 测试**：新增的数据集选择、串行模式和跳过预检选项在 3×10 fixture 上产生与现有 golden 一致的 checksum；在 64×252 数据集上产生与 B3 已登记 stress checksum 一致的账户投影。
- **Nautilus 重置一致性测试**：同一 worker 内重置后连续多次 Run 的投影 checksum 与新建引擎一致；不一致时断言入口选择回退模式。
- 先例：Nautilus Adapter 与 preflight 的现有 Python 测试、quant-research 中 B2/B3 的 golden 与 checksum 测试、票据 11 的并行重复与顺序轮转协议。
- 正式测量只在 release 构建上进行，先过正确性门再计时；时序回归不作为常规测试断言。

## Out of Scope

- 修改票据 09 登记的门槛数值，或以 Sharpe/收益差判断实现优劣。
- ADR 0012 的停牌原生拒单隔离实验；另开票据，作为 MVP-4 Nautilus Adapter 的前置条件。
- 生产 Fast Event、生产 Nautilus Adapter、Accurate Backend 或 Live Runtime。
- 冷构建测量与每候选独立构建归因。
- 给维护成本设门槛或货币化。
- B1、B3 的重测；除 64×252 稳健性负载外的其他规模扩展。
- 真实 ETF 数据、历史 PIT、分红总回报或可成交性验证。

## Further Notes

- 机器基线为 Apple M1（8 逻辑核）、16 GiB、macOS arm64；Nautilus 固定 `2.0.0rc5`，使用公开 `BacktestEngine` 接口。结论只覆盖该机器与版本。
- 预测依据：票据 09 报告中 Nautilus S2/S3 事件处理段中位约 507 µs / 3.53 ms，Rust 单 Run 中位约 20.5 µs / 140 µs；Rust 整进程 RSS 约 30 MB，Nautilus 单 worker 约 75 MB。这些数值口径不同，只用于写预测，不用于判定。
- 64×252 负载的 S2 是 Top-5、每 5 个 eligible session 调仓，与判定负载参数不同；报告须分别列出两类负载的参数。若该负载包含停牌状态，沿用 ADR 0012 的分列方式。
- 若 B2 最终为 `defer` 或 `unresolved`，MVP-3 是否仍自研 Fast Event 由项目负责人另行决定，本 spec 不预设。
