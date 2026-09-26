# POC-0 Benchmark Spec

状态：执行规范，2026-09-26。目标是用可复跑的实测数据裁决目标架构中三项未决性能假设。B1 已有标量列访问 smoke，但没有可裁决布局的完整对比；本文所有规模与检查点都是试验设计，不是结果。工作范围与验收见 [本地 POC-0 spec](../.scratch/poc-0-benchmark/spec.md)，目标架构见 [`ARCHITECTURE.md`](../ARCHITECTURE.md)。

## 先固定语义和输入

1. 使用版本化合成数据：固定 seed、交易日历、证券数、bar 数、缺失分布与数据 hash；再用一份许可明确的已发布 ETF 快照作现实形状复核。真实快照不足以证明 PIT 或投资业绩。
2. 三个参考策略：S1 Buy & Hold、S2 Momentum Rotation、S3 MA20/60。S2 固定窗口、排序方向、TopK、平局顺序、调仓日期、目标权重、缺值规则与 T 收盘信号 → 下一可用日 open 执行。未来收益仅作为评价标签。
3. 先给每个实现同一份独立金标准：至少 3 ETF × 10 个交易日，含一处缺 bar、一处不可成交、非零佣金与滑点。逐日列出目标、订单/成交（引擎适用时）、现金、数量、估值价、成本和权益。现有单元 fixture 可借鉴，但须导出为独立、可复用输入与预期；不能让被测实现自己生成期望结果。
4. 比较只在共同语义下进行。Vector 的简化模型与事件账户分别标注；若要数值对拍，先关掉流动性/延迟/复杂滑点，并统一交易日历、成交时刻、价格、成本、手数、舍入与估值。无法统一的差异单独报告，不归咎于性能或 Bug。

## 统一测量协议

- 每次记录 Git revision、dirty diff 摘要、依赖锁定版本、Rust/Python 版本、编译参数、CPU 型号/核心数、内存、操作系统、输入 hash、策略参数、成本配置、seed 和并行度。
- 将数据生成、磁盘读取/解码、格式转换、因子计算、策略/回测循环、结果序列化分开计时；另测冷启动端到端时长。缓存热/冷分别标注，不能混在一组统计。
- 固定预热和重复次数，至少报告中位数、p95、离散范围、峰值 RSS、吞吐量及结果 checksum。单机测量先控制线程数与其他负载；逐 Run 并行吞吐另列，避免把单 Run 延迟与扫描吞吐混同。
- 小、中、大三种数据规模在可用机器的内存预算内逐级提高；大样本必须验证分块/流式路径，不允许靠整库装入 RAM 冒充 out-of-core。规模、预算和任何 OOM 记录在原始结果中。
- 先过正确性门槛，再比较速度。基准代码、输入生成器、原始测量 JSON/CSV 和一条可复跑命令入库；大数据产物只记录 manifest/hash 与获取方法。

## B1：Polars / Arrow / Custom SoA

**问题：** Rust 热路径是否值得维护自定义列数组；磁盘/研究处理是否仍按目标技术分工。

**共同工作负载：** 对相同日频 OHLCV 面板计算 S2 所需动量、波动率、排名和 TopK，再形成权重与简化组合收益。逐项对齐窗口端点、null/NaN、缺日、排序平局和浮点容差。分别测 Parquet 扫描/列裁剪、Arrow 批次转换、内存内重复运行和参数扫描复用。保留一种朴素现有路径作工程基线。

**指标：** rows/s、factor-values/s、每秒参数 Run、转换成本、峰值 RSS、缓存命中后的节省、总存储字节。只有热路径净收益足以抵偿转换与维护成本时才采纳 Custom SoA；阈值须在看结果前结合目标硬件预算登记，不事后挑选胜者。

## B2：自研 Fast Event / NautilusTrader

**问题：** 在同等 L1 交易语义下，自研 Fast Event 的吞吐优势与维护成本是否成立。

**共同工作负载：** S1/S2/S3 共用合成 bar、市场日历、价格精度、账户初始值、市场单、固定佣金/滑点和下一可用 open 执行。先用单 Venue、长仓、无杠杆、无部分成交的子集比较；再记录 Nautilus Adapter 为达到同一语义需要的数据转换和对象构造。当前 ETF 回测可作对照，但不能直接命名为完整 Fast Event Engine。

**指标：** events/s、runs/s、单 Run 延迟、初始化/转换占比、峰值 RSS、成交/现金/仓位/权益差异。若两 Backend 的成交时序无法对齐，先列出不可消除的语义差异，并仅比较各自内部重复运行；不得用不同模型的 Sharpe 差断言 Engine Bug。固定 Nautilus 版本及 API 入口，因为其 Rust API 仍在演进。

## B3：Rust Strategy / PyO3 Python callback

**问题：** Python `on_bar()` 在哪些 bar × 策略 × 参数规模下限制吞吐。

**共同工作负载：** 同一事件流与决策逻辑，用 Rust Native 和 PyO3 每 bar 回调实现 S1/S2/S3；再加入“仅计数”空回调测固定跨语言成本。记录单线程回调、多个独立 Run 并行和可批处理调用，区分 Python 计算、数据转换、GIL 与 Rust 引擎开销。

**指标：** callbacks/s、events/s、runs/s、每次回调额外耗时、峰值 RSS、并行扩展率与结果一致性。性能拐点由目标研究规模和实测吞吐共同确定；MVP 允许 Python 回调，但报告其适用规模。

## 产出与决策门槛

每项交付：`README` 复跑命令、锁定输入与金标准、原始测量、汇总图表、正确性/语义差异、环境信息、结论 `adopt / defer / reject / unresolved` 及理由。先完成 B1 最小可运行纵切，再执行 B2/B3；任何单项依赖或版本无法安装时保留可复跑输入与 harness，标记 `unresolved`，不填造性能数字。

POC-0 结束时仅决定测量能支持的技术选择。真实 ETF Rotation 的历史 Universe、分红和执行状态 P0 问题仍按 [`docs/priorities.md`](priorities.md) 单独验收。官方候选接口资料：[Nautilus 回测](https://nautilustrader.io/docs/latest/concepts/backtesting/)、[Nautilus Rust](https://nautilustrader.io/docs/latest/concepts/rust/)、[Arrow 格式](https://arrow.apache.org/docs/format/Columnar.html)、[Parquet](https://parquet.apache.org/)。
