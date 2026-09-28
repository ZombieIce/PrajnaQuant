# 03: 完整判定负载：S3、次级边界、重复协议与环境身份

**What to build:** 研究者对票据 09 的两个判定负载（S2 3×10、S3 MA20/60 3×130）运行比较入口，得到按登记协议采样、带完整环境身份、给出判定负载结论的报告。参见 [spec](../spec.md)。

**Blocked by:** 02

**Status:** ready-for-agent

- [ ] S3 3×130 fixture 接入双方候选，过正确性门后按主边界采样；S2 与 S3 都满足条件才能判 `adopt`。
- [ ] 报告增加次级边界：每 Run 从共享的规范 bars 开始，含转换、初始化和运行的端到端耗时；明确标注不参与判定。
- [ ] 固定每 worker 预热 2 次、串行样本 20 个、并行 2 worker × 6 Run × 5 组，组间交替候选顺序；保存每个原始样本。
- [ ] 记录 Rust 空二进制与"Python 导入 Nautilus 后"两个空载基线 RSS。
- [ ] 记录 CPU/核数/内存/OS、rustc、Python、Nautilus 版本、Cargo.lock hash、二进制 hash、代码 revision、脏工作树状态和 Python 传递依赖解析快照（注明未形成完整 lock）。
- [ ] 构建前后执行 10 GiB 空间闸门，单独列出构建耗时与 target 增量；闸门阻断时报告标 `unresolved` 并保存复跑条件。
- [ ] 集成测试断言：报告含两种边界、所用 Nautilus 模式、每 worker 与合计 RSS、基线 RSS 和环境身份各节。
