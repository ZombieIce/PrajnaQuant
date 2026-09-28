# 03: 完整判定负载：S3、次级边界、重复协议与环境身份

**What to build:** 研究者对票据 09 的两个判定负载（S2 3×10、S3 MA20/60 3×130）运行比较入口，得到按登记协议采样、带完整环境身份、给出判定负载结论的报告。参见 [spec](../spec.md)。

**Blocked by:** 02

**Status:** resolved

- [x] S3 3×130 fixture 接入双方候选，过正确性门后按主边界采样；S2 与 S3 都满足条件才能判 `adopt`。
- [x] 报告增加次级边界：每 Run 从共享的规范 bars 开始，含转换、初始化和运行的端到端耗时；明确标注不参与判定。
- [x] 固定每 worker 预热 2 次、串行样本 20 个、并行 2 worker × 6 Run × 5 组，组间交替候选顺序；保存每个原始样本。
- [x] 记录 Rust 空二进制与"Python 导入 Nautilus 后"两个空载基线 RSS。
- [x] 记录 CPU/核数/内存/OS、rustc、Python、Nautilus 版本、Cargo.lock hash、二进制 hash、代码 revision、脏工作树状态和 Python 传递依赖解析快照（注明未形成完整 lock）。
- [x] 构建前后执行 10 GiB 空间闸门，单独列出构建耗时与 target 增量；闸门阻断时报告标 `unresolved` 并保存复跑条件。
- [x] 集成测试断言：报告含两种边界、所用 Nautilus 模式、每 worker 与合计 RSS、基线 RSS 和环境身份各节。

2026-09-28 实施与测量完成。综合报告为 [`b2-matched-decision-loads-2026-09-28.json`](../../../poc/poc0-benchmark/results/b2-matched-decision-loads-2026-09-28.json)，release 构建及空间记录为 [`build record`](../../../poc/poc0-benchmark/results/b2-matched-decision-loads-release-build-2026-09-28.json)。S2/S3 correctness 均通过，当前 fallback 下两负载均达速度/RSS 数值门槛且预登记预测均成立；但 Nautilus reset parity 未验证，因此两个负载及整体正式结论都保留 `unresolved` / `exploratory`，不得当作正式 `adopt`。报告保存主边界和端到端原始样本、每 worker 与 RSS 上界、空载 RSS 基线，以及 Apple M1 / 8 logical CPUs / 16 GiB 的环境身份。依赖快照按 active marker 解析出 0 个传递发行包（未启用的 visualization extras 除外），明确不是完整 lock。Rust warm release build command 用时 12.395 秒，记录完整时间间隔为 13.746 秒，target 逻辑增量 46,440 bytes；所有记录空间样本均高于 10 GiB。POC Python 测试 25/25 通过；其余验证及全量测试限制见当前 HANDOFF。

## Comments

**2026-09-28 并入本票提交的旁路修复（非 03 范围）：**review 指出判定函数仅凭协议标签就把 `matched_fallback` 判为 `registered`，旧报告被重算时会误标正式证据。`b2_matched.py` 的 `decide()` 新增 `mode_selection`（`reset_parity`、`selected_mode`），只有证据符合 ADR 0013 且与实测模式一致才为 `registered`；无证据时任何标签都是 `exploratory`，证据矛盾或与实测模式不符返回 `unresolved`；判定门槛不变。本票编排入口未传证据，判定保持 `exploratory`，与上文结论一致。另修正依赖 marker 解析，使未启用 extra 不再被误报为无法解析。并行中的票据 07 工作树改动不纳入本次提交；`.venv` 下 POC Python 测试 25/25 通过。
