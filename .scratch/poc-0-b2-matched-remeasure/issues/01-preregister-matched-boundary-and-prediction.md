# 01: 预登记统一口径 ADR 与书面预测

**What to build:** 项目负责人在任何正式重测之前，能从一份 ADR 读到 B2 同口径比较的全部规则，并从本票据读到基于已有分段计时写下的预测；结果出来后无法移动标准。参见 [spec](../spec.md)。

**Blocked by:** None (can start immediately)

**Status:** resolved

- [x] 新 ADR 记录主计时边界（worker 内一次性准备输入；每 Run 计入策略实例创建、因子/信号计算、事件处理、账户推进和结果投影；不计序列化与 checksum）、Nautilus 重置与回退模式、次级端到端边界仅作报告。
- [x] ADR 记录指标定义（单 worker 串行中位/p95；2 worker 墙钟 Runs/s，不含池启动和一次性数据下发）、预热/样本/重复组数、组间交替顺序、原生并行模型与每 Run 回传内容。
- [x] ADR 记录 RSS 归因：预检进程分离；负载进程峰值之和，标为上界；另报每 worker 与空载基线。
- [x] ADR 记录判定负载（票据 09 的 S2 3×10、S3 3×130）、稳健性负载（B3 的 64×252 S2/S3）和完整的 `adopt / defer / reject / unresolved` 映射；门槛数值原样引用票据 09，不做修改；ADR 0012 的排除维度保持不变。
- [x] 本票据 `## Answer` 写入书面预测：两策略在判定负载上均达到速度门槛，Rust RSS 更低；列出依据数值及其口径差异，声明预测不参与判定。
- [x] ADR 与预测在首次正式测量（票据 06）之前保存，并记录保存时的 Git revision：`9c487f3ee60e2f8bbfc38b543b3b3cfd6b00bf2d`。

## Answer

**预登记预测：**同口径重测的判定负载中，Fast Event 的 S2、S3 都会达到票据 09 的速度门槛（中位单 Run 延迟至少快 2×，2-worker 并行 Runs/s 至少高 2×），且 Rust RSS 合计更低。预测不参与正式判定；最终报告需对照预测说明是否成立。

预测依据为 2026-09-27 票据 09 原始报告：Nautilus S2/S3 事件处理 median 约 507 µs / 3.53 ms，Fast Event 每 Run median 约 20.5 µs / 140 µs。Nautilus 的事件处理段单独计时，Rust 数值则是整次 Run，旧口径不匹配；只能作为事前预测理由，不能当作重测速度比。RSS 方向预测依据为旧报告 Rust 全进程约 30 MB（含 golden 预检）及 Nautilus 单 worker 约 75 MB（S2）/77 MB（S3）；归因范围同样不匹配，不能作为正式 RSS 比较。

预登记 ADR：[ADR 0013](../../../docs/decisions/0013-poc0-b2-matched-remeasure.md)。ADR 与预测已于 `9c487f3ee60e2f8bbfc38b543b3b3cfd6b00bf2d` 保存；该 revision 必须早于首次正式 release 测量。
